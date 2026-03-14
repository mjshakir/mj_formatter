use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use tracing::warn;

use crate::config::app_config::AppConfig;
use crate::files::atomic_writer::AtomicWriter;
use crate::model::file_result::FileResult;

#[derive(Clone, Debug, Serialize)]
struct ManifestMeta {
    format_version: u32,
    tool: &'static str,
    run_id: String,
    created_at_unix_ms: u64,
    root: String,
    backup_dir: String,
    mode: String,
    suffix: String,
    files: usize,
}

#[derive(Clone, Debug, Serialize)]
struct ManifestFile {
    source: String,
    backup: String,
    size: u64,
    mtime_ns: u64,
    relative_path: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct ManifestPayload {
    meta: ManifestMeta,
    files: Vec<ManifestFile>,
}

// Deserialization-only counterparts used by `restore`.
#[derive(Debug, Deserialize)]
struct ManifestFileDe {
    source: String,
    backup: String,
}

#[derive(Debug, Deserialize)]
struct ManifestPayloadDe {
    #[serde(default)]
    files: Vec<ManifestFileDe>,
}

pub struct BackupManifest;

impl BackupManifest {
    pub fn write(config: &AppConfig, results: &[FileResult]) -> Result<()> {
        if !config.backup || config.check {
            return Ok(());
        }
        let run_id = std::env::var("FORMATTER_BACKUP_RUN")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(Self::default_run_id);
        let backup_run_dir = config.backup_dir.join(run_id.as_str());
        let manifest_path = backup_run_dir.join("backup_manifest.toml");

        let mut files = Vec::<ManifestFile>::new();
        for result in results {
            let Some(backup_path) = result.backup_path.as_ref() else {
                continue;
            };
            let Ok(stat) = std::fs::metadata(backup_path) else {
                continue;
            };
            let relative_path = result
                .path
                .strip_prefix(config.root.as_path())
                .ok()
                .map(|item| item.to_string_lossy().to_string());
            files.push(ManifestFile {
                source: normalize_path(result.path.as_path()),
                backup: normalize_path(backup_path),
                size: stat.len(),
                mtime_ns: stat
                    .modified()
                    .ok()
                    .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                    .map(|value| {
                        value
                            .as_secs()
                            .saturating_mul(1_000_000_000)
                            .saturating_add(u64::from(value.subsec_nanos()))
                    })
                    .unwrap_or(0),
                relative_path,
            });
        }

        let payload = ManifestPayload {
            meta: ManifestMeta {
                format_version: 1,
                tool: "formatter",
                run_id,
                created_at_unix_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|value| value.as_millis() as u64)
                    .unwrap_or(0),
                root: normalize_path(config.root.as_path()),
                backup_dir: normalize_path(config.backup_dir.as_path()),
                mode: format!("{:?}", config.backup_mode).to_lowercase(),
                suffix: config.backup_suffix.clone(),
                files: files.len(),
            },
            files,
        };
        let content = toml::to_string_pretty(&payload)?;
        AtomicWriter::write_text(manifest_path.as_path(), format!("{content}\n").as_str())?;
        Ok(())
    }

    /// Restore files from a backup run. If `run_id` is `None`, the most recent run
    /// (determined by lexicographic sort of run directory names, which are millisecond
    /// timestamps) is used. Returns the number of files restored.
    pub fn restore(backup_dir: &Path, run_id: Option<&str>) -> Result<usize> {
        let run_dir = if let Some(id) = run_id {
            backup_dir.join(id)
        } else {
            // Only consider directories that contain a backup_manifest.toml, sorted by
            // actual modification time so that old named dirs (e.g. "hazard_run_...", "src")
            // don't appear "newer" than numeric-timestamp run dirs via lexicographic order.
            let mut entries: Vec<(std::time::SystemTime, PathBuf)> =
                std::fs::read_dir(backup_dir)
                    .with_context(|| {
                        format!("failed to read backup directory {:?}", backup_dir)
                    })?
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.path())
                    .filter(|path| path.is_dir() && path.join("backup_manifest.toml").exists())
                    .filter_map(|path| {
                        std::fs::metadata(&path)
                            .ok()
                            .and_then(|meta| meta.modified().ok())
                            .map(|mtime| (mtime, path))
                    })
                    .collect();
            entries.sort_by_key(|(mtime, _)| *mtime);
            entries
                .into_iter()
                .last()
                .map(|(_, path)| path)
                .ok_or_else(|| anyhow::anyhow!("no backup runs found in {:?}", backup_dir))?
        };

        let manifest_path = run_dir.join("backup_manifest.toml");
        let content = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read manifest {:?}", manifest_path))?;
        let payload: ManifestPayloadDe = toml::from_str(&content)
            .with_context(|| format!("failed to parse manifest {:?}", manifest_path))?;

        let mut restored = 0usize;
        for file in &payload.files {
            let backup_path = Path::new(&file.backup);
            let source_path = Path::new(&file.source);
            if !backup_path.exists() {
                warn!("backup not found, skipping: {}", file.backup);
                continue;
            }
            std::fs::copy(backup_path, source_path).with_context(|| {
                format!(
                    "failed to restore {:?} from backup {:?}",
                    source_path, backup_path
                )
            })?;
            println!("restored: {}", file.source);
            restored += 1;
        }
        println!(
            "undo complete: {} file(s) restored from {:?}",
            restored, run_dir
        );
        Ok(restored)
    }

    fn default_run_id() -> String {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_millis())
            .unwrap_or(0);
        millis.to_string()
    }
}

fn normalize_path(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| PathBuf::from(path))
        .to_string_lossy()
        .to_string()
}
