use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use dashmap::DashMap;
use moka::sync::Cache;
use serde::{Deserialize, Serialize};

use crate::engine::accuracy_gate::AccuracyGateDecision;
use crate::files::atomic_writer::AtomicWriter;
use crate::files::codec::StateCodec;
use crate::model::edit::Edit;
use crate::model::file_result::FileResult;
use crate::model::exec_trace::PolicyExecutionTrace;
use crate::model::rename_plan::SemanticRenamePlan;
use crate::model::violation::Violation;

const CACHE_VERSION: u32 = 3;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CacheConvergencePair {
    loser: String,
    winner: String,
    count: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CachedFileResult {
    changed: bool,
    semantic_rename_plans: Vec<SemanticRenamePlan>,
    convergence_pairs: Vec<CacheConvergencePair>,
    violations: Vec<Violation>,
    edits: Vec<Edit>,
    #[serde(default)]
    policy_traces: Vec<PolicyExecutionTrace>,
    #[serde(default)]
    accuracy_gate: Option<AccuracyGateDecision>,
    warnings: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PersistedCheckResultCache {
    version: u32,
    fingerprint: String,
    entries: std::collections::HashMap<String, CachedFileResult>,
}

pub struct CheckResultCache {
    enabled: bool,
    path: PathBuf,
    fingerprint: String,
    l1: Option<Cache<String, Arc<CachedFileResult>>>,
    disk_entries: DashMap<String, Arc<CachedFileResult>>,
    dirty: AtomicBool,
}

impl CheckResultCache {
    pub fn open(path: PathBuf, enabled: bool, l1_size: usize, fingerprint: String) -> Self {
        let path = Self::normalize_store_path(path);
        if !enabled {
            return Self {
                enabled: false,
                path,
                fingerprint,
                l1: None,
                disk_entries: DashMap::new(),
                dirty: AtomicBool::new(false),
            };
        }

        let entries = Self::load_entries(path.as_path(), fingerprint.as_str());
        Self {
            enabled: true,
            path,
            fingerprint,
            l1: Some(Cache::builder().max_capacity(l1_size as u64).build()),
            disk_entries: DashMap::from_iter(entries.into_iter().map(|(k, v)| (k, Arc::new(v)))),
            dirty: AtomicBool::new(false),
        }
    }

    fn normalize_store_path(path: PathBuf) -> PathBuf {
        if path.is_dir() {
            return path.join("check_results.bin");
        }
        if path.extension().is_none()
            && path
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("check_results"))
        {
            return path.with_extension("bin");
        }
        path
    }

    pub fn content_hash(text: &str) -> String {
        let crc = crc32fast::hash(text.as_bytes());
        format!("{crc:08x}-{:x}", text.len())
    }

    pub fn get(&self, path: &Path, content_hash: &str) -> Option<FileResult> {
        if !self.enabled {
            return None;
        }
        let key = Self::make_key(path, content_hash, self.fingerprint.as_str());
        if let Some(l1) = self.l1.as_ref() {
            if let Some(value) = l1.get(&key) {
                return Some(Self::to_file_result(path, value.as_ref()));
            }
        }

        let arc_value = self.disk_entries.get(&key)?.clone();
        if let Some(l1) = self.l1.as_ref() {
            l1.insert(key, arc_value.clone());
        }
        Some(Self::to_file_result(path, arc_value.as_ref()))
    }

    pub fn put(&self, path: &Path, content_hash: &str, result: &FileResult) {
        if !self.enabled || result.error.is_some() {
            return;
        }
        let key = Self::make_key(path, content_hash, self.fingerprint.as_str());
        let arc_value = Arc::new(Self::from_file_result(result));
        if let Some(l1) = self.l1.as_ref() {
            l1.insert(key.clone(), arc_value.clone());
        }
        self.disk_entries.insert(key, arc_value);
        self.dirty.store(true, Ordering::Release);
    }

    pub fn flush(&self) -> Result<()> {
        if !self.enabled || !self.dirty.load(Ordering::Acquire) {
            return Ok(());
        }

        let entries: std::collections::HashMap<String, CachedFileResult> = self
            .disk_entries
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().as_ref().clone()))
            .collect();
        let payload = PersistedCheckResultCache {
            version: CACHE_VERSION,
            fingerprint: self.fingerprint.clone(),
            entries,
        };
        let bytes = StateCodec::encode_binary(&payload)?;
        AtomicWriter::write_bytes(self.path.as_path(), bytes.as_slice())?;
        self.dirty.store(false, Ordering::Release);
        Ok(())
    }

    fn make_key(path: &Path, content_hash: &str, fingerprint: &str) -> String {
        format!(
            "{}\u{0}{}\u{0}{}",
            Self::normalize_path(path),
            content_hash,
            fingerprint
        )
    }

    fn normalize_path(path: &Path) -> String {
        path.canonicalize()
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .to_string()
    }

    fn load_entries(path: &Path, fingerprint: &str) -> std::collections::HashMap<String, CachedFileResult> {
        if !path.exists() {
            return std::collections::HashMap::new();
        }
        let Ok(persisted) = StateCodec::read_decode_binary::<PersistedCheckResultCache>(path)
        else {
            return std::collections::HashMap::new();
        };
        if persisted.version != CACHE_VERSION || persisted.fingerprint != fingerprint {
            return std::collections::HashMap::new();
        }
        persisted.entries
    }

    fn from_file_result(result: &FileResult) -> CachedFileResult {
        let convergence_pairs = result
            .convergence_pairs
            .iter()
            .map(|((loser, winner), count)| CacheConvergencePair {
                loser: loser.clone(),
                winner: winner.clone(),
                count: *count,
            })
            .collect::<Vec<_>>();
        CachedFileResult {
            changed: result.changed,
            semantic_rename_plans: result.semantic_rename_plans.clone(),
            convergence_pairs,
            violations: result.violations.clone(),
            edits: result.edits.clone(),
            policy_traces: result.policy_traces.clone(),
            accuracy_gate: result.accuracy_gate.clone(),
            warnings: result.warnings.clone(),
        }
    }

    fn to_file_result(path: &Path, cached: &CachedFileResult) -> FileResult {
        let mut convergence_pairs = BTreeMap::<(String, String), usize>::new();
        for pair in &cached.convergence_pairs {
            if pair.count == 0 {
                continue;
            }
            *convergence_pairs
                .entry((pair.loser.clone(), pair.winner.clone()))
                .or_insert(0usize) += pair.count;
        }
        FileResult {
            path: path.to_path_buf(),
            changed: cached.changed,
            pending_text: None,
            semantic_rename_plans: cached.semantic_rename_plans.clone(),
            convergence_pairs,
            violations: cached.violations.clone(),
            edits: cached.edits.clone(),
            policy_traces: cached.policy_traces.clone(),
            accuracy_gate: cached.accuracy_gate.clone(),
            error: None,
            backup_path: None,
            warnings: cached.warnings.clone(),
            elapsed_engine_ms: 0.0,
            elapsed_total_ms: 0.0,
            policy_certainty: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::runtime::result_cache::CheckResultCache;

    #[test]
    fn normalizes_directory_cache_path() {
        let temp_root = std::env::temp_dir().join(format!(
            "fmt_cache_{}_{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("t")
        ));
        let _ = std::fs::create_dir_all(temp_root.as_path());
        let normalized = CheckResultCache::normalize_store_path(temp_root.clone());
        assert!(normalized.ends_with(PathBuf::from("check_results.bin")));
        let _ = std::fs::remove_dir_all(temp_root);
    }

    #[test]
    fn normalizes_legacy_check_results_filename() {
        let legacy = PathBuf::from("var/cache/check_results");
        let normalized = CheckResultCache::normalize_store_path(legacy);
        assert!(normalized.ends_with(PathBuf::from("check_results.bin")));
    }
}
