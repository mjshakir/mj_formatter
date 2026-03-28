use rustc_hash::{FxHashMap, FxHashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::files::file_unit::FileUnitKind;
use crate::parser::manager::SemanticCompdbContextKind;

const MAX_HEADER_CONSENSUS_ARGSETS: usize = 6;
const MAX_SOURCE_CONSENSUS_ARGSETS: usize = 6;
const SOURCE_CONSENSUS_MIN_SCORE: i32 = 12;

#[derive(Clone, Debug)]
pub(crate) struct CompdbCommandEntry {
    pub(crate) source_path: String,
    pub(crate) args: Vec<String>,
}

#[derive(Clone, Debug)]
struct HeaderConsensusCandidate {
    score: i32,
    source_path: String,
    args: Vec<String>,
    paired_source: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CompdbIndex {
    key: Option<String>,
    args_by_path: Option<Arc<FxHashMap<String, Vec<String>>>>,
    entries: Option<Arc<Vec<CompdbCommandEntry>>>,
}

impl CompdbIndex {
    pub(crate) fn load(clang_compdb_path: Option<PathBuf>) -> Self {
        let (key, args_by_path, entries) = Self::load_compdb_args(clang_compdb_path);
        Self {
            key,
            args_by_path,
            entries,
        }
    }

    pub(crate) fn key(&self) -> Option<&String> {
        self.key.as_ref()
    }

    pub(crate) fn args_exact(&self, path: &Path) -> Option<Vec<String>> {
        let items = self.args_by_path.as_ref()?;
        let normalized = Self::normalize_path(path);
        items.get(normalized.as_str()).cloned()
    }

    pub(crate) fn has_exact_entry_for_path(&self, path: &Path) -> bool {
        self.args_exact(path).is_some()
    }

    pub(crate) fn semantic_context_kind_for_path(&self, path: &Path) -> SemanticCompdbContextKind {
        if self.has_exact_entry_for_path(path) {
            return SemanticCompdbContextKind::Exact;
        }
        if Self::is_header_path(path) {
            return self.header_arg_sets_with_context(path).0;
        }
        if !self.source_consensus_arg_sets(path).is_empty() {
            SemanticCompdbContextKind::SourceConsensus
        } else {
            SemanticCompdbContextKind::None
        }
    }

    pub(crate) fn header_arg_sets_with_context(
        &self,
        header_path: &Path,
    ) -> (SemanticCompdbContextKind, Vec<Vec<String>>) {
        let paired = self.header_paired_source_arg_sets(header_path);
        if !paired.is_empty() {
            return (SemanticCompdbContextKind::PairedSourceHeuristic, paired);
        }
        let consensus = self.header_consensus_arg_sets(header_path);
        if !consensus.is_empty() {
            return (SemanticCompdbContextKind::HeaderConsensus, consensus);
        }
        (SemanticCompdbContextKind::None, Vec::new())
    }

    pub(crate) fn header_consensus_arg_sets(&self, header_path: &Path) -> Vec<Vec<String>> {
        Self::header_arg_sets(
            self.header_consensus_candidates(header_path).into_iter(),
            false,
        )
    }

    pub(crate) fn header_paired_source_arg_sets(&self, header_path: &Path) -> Vec<Vec<String>> {
        Self::header_arg_sets(
            self.header_consensus_candidates(header_path).into_iter(),
            true,
        )
    }

    fn header_consensus_candidates(&self, header_path: &Path) -> Vec<HeaderConsensusCandidate> {
        let Some(entries) = self.entries.as_ref() else {
            return Vec::new();
        };
        let paired_source_candidates = FileUnitKind::paired_companion_paths_on_disk(header_path)
            .into_iter()
            .map(|path| Self::normalize_path(path.as_path()))
            .collect::<FxHashSet<_>>();
        let header_parent = header_path
            .parent()
            .map(Self::normalize_path)
            .unwrap_or_default();
        let header_stem = header_path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_string();

        let mut candidates = entries
            .iter()
            .map(|entry| {
                let mut score = 0i32;
                let source = Path::new(entry.source_path.as_str());
                let source_parent = source
                    .parent()
                    .map(Self::normalize_path)
                    .unwrap_or_default();
                if !header_parent.is_empty() && source_parent == header_parent {
                    score += 6;
                }
                let paired_source = !paired_source_candidates.is_empty()
                    && paired_source_candidates.contains(entry.source_path.as_str());
                if paired_source {
                    score += 12;
                }
                let source_stem = source
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or_default();
                if !header_stem.is_empty() && source_stem == header_stem {
                    score += 5;
                }
                if source
                    .extension()
                    .and_then(|value| value.to_str())
                    .is_some_and(crate::files::file_unit::is_implementation_extension)
                {
                    score += 2;
                }
                if entry.args.iter().any(|arg| {
                    arg.starts_with("-I")
                        && !header_parent.is_empty()
                        && arg.contains(header_parent.as_str())
                }) {
                    score += 1;
                }
                HeaderConsensusCandidate {
                    score,
                    source_path: entry.source_path.clone(),
                    args: entry.args.clone(),
                    paired_source,
                }
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.source_path.cmp(&right.source_path))
        });
        candidates
    }

    fn header_arg_sets(
        candidates: impl Iterator<Item = HeaderConsensusCandidate>,
        paired_only: bool,
    ) -> Vec<Vec<String>> {
        let mut unique = Vec::<Vec<String>>::new();
        let mut seen: FxHashSet<String> = FxHashSet::default();
        for candidate in candidates {
            if paired_only && !candidate.paired_source {
                continue;
            }
            if candidate.score <= 0 {
                continue;
            }
            let fingerprint = candidate.args.join("\u{1f}");
            if !seen.insert(fingerprint) {
                continue;
            }
            unique.push(candidate.args);
            if unique.len() >= MAX_HEADER_CONSENSUS_ARGSETS {
                break;
            }
        }
        unique
    }

    pub(crate) fn source_consensus_arg_sets(&self, source_path: &Path) -> Vec<Vec<String>> {
        let Some(entries) = self.entries.as_ref() else {
            return Vec::new();
        };
        let source_parent = source_path
            .parent()
            .map(Self::normalize_path)
            .unwrap_or_default();
        let source_extension = source_path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let source_stem = source_path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        let normalized_source_stem = Self::normalize_stem_for_consensus(source_stem);

        let mut candidates = entries
            .iter()
            .map(|entry| {
                let mut score = 0i32;
                let candidate_path = Path::new(entry.source_path.as_str());
                let candidate_parent = candidate_path
                    .parent()
                    .map(Self::normalize_path)
                    .unwrap_or_default();
                if !source_parent.is_empty() && candidate_parent == source_parent {
                    score += 6;
                }
                let candidate_extension = candidate_path
                    .extension()
                    .and_then(|value| value.to_str())
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if !source_extension.is_empty() && candidate_extension == source_extension {
                    score += 2;
                }
                let candidate_stem = candidate_path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or_default();
                let normalized_candidate_stem = Self::normalize_stem_for_consensus(candidate_stem);
                if !normalized_source_stem.is_empty()
                    && normalized_source_stem == normalized_candidate_stem
                {
                    score += 10;
                } else if !normalized_source_stem.is_empty()
                    && !normalized_candidate_stem.is_empty()
                    && (normalized_candidate_stem.starts_with(normalized_source_stem.as_str())
                        || normalized_source_stem.starts_with(normalized_candidate_stem.as_str()))
                {
                    score += 6;
                }
                if entry.args.iter().any(|arg| {
                    arg.starts_with("-I")
                        && !source_parent.is_empty()
                        && arg.contains(source_parent.as_str())
                }) {
                    score += 1;
                }
                (score, entry.source_path.clone(), entry.args.clone())
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));

        let mut unique = Vec::<Vec<String>>::new();
        let mut seen: FxHashSet<String> = FxHashSet::default();
        for (score, _, args) in candidates {
            if score < SOURCE_CONSENSUS_MIN_SCORE {
                continue;
            }
            let fingerprint = args.join("\u{1f}");
            if !seen.insert(fingerprint) {
                continue;
            }
            unique.push(args);
            if unique.len() >= MAX_SOURCE_CONSENSUS_ARGSETS {
                break;
            }
        }
        unique
    }

    pub(crate) fn sanitize_compdb_args(args: &[String], file_path: &Path) -> Vec<String> {
        let file_norm = Self::normalize_path(file_path);
        let file_name = file_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_string();
        let mut result = Vec::<String>::new();
        let mut skip_next = false;
        for (index, arg) in args.iter().enumerate() {
            if skip_next {
                skip_next = false;
                continue;
            }
            if index == 0
                && !arg.starts_with('-')
                && Self::looks_like_compiler_executable(arg.as_str())
            {
                continue;
            }
            if arg == "-c" || arg == "-o" {
                skip_next = true;
                continue;
            }
            if arg == &file_norm || (!file_name.is_empty() && arg == &file_name) {
                continue;
            }
            result.push(arg.clone());
        }
        result
    }

    pub(crate) fn normalize_path(path: &Path) -> String {
        fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .to_string()
    }

    pub(crate) fn is_header_path(path: &Path) -> bool {
        matches!(
            path.extension()
                .and_then(|value| value.to_str())
                .map(|value| value.to_ascii_lowercase())
                .as_deref(),
            Some("h") | Some("hh") | Some("hpp") | Some("hxx") | Some("ipp") | Some("inl")
        )
    }

    #[allow(clippy::type_complexity)]
    fn load_compdb_args(
        clang_compdb_path: Option<PathBuf>,
    ) -> (
        Option<String>,
        Option<Arc<FxHashMap<String, Vec<String>>>>,
        Option<Arc<Vec<CompdbCommandEntry>>>,
    ) {
        let path = clang_compdb_path.or_else(|| {
            let candidate = PathBuf::from("compile_commands.json");
            candidate.exists().then_some(candidate)
        });
        let Some(path) = path else {
            return (None, None, None);
        };
        let key = Some(Self::normalize_path(path.as_path()));
        let content = match fs::read_to_string(path.as_path()) {
            Ok(value) => value,
            Err(_) => return (key, None, None),
        };
        let parsed = match serde_json::from_str::<serde_json::Value>(content.as_str()) {
            Ok(value) => value,
            Err(_) => return (key, None, None),
        };
        let Some(entries) = parsed.as_array() else {
            return (key, None, None);
        };

        let mut mapping: FxHashMap<String, Vec<String>> = FxHashMap::default();
        let mut command_entries = Vec::<CompdbCommandEntry>::new();
        for entry in entries {
            let Some(table) = entry.as_object() else {
                continue;
            };
            let Some(file_path_raw) = table.get("file").and_then(|value| value.as_str()) else {
                continue;
            };
            let file_path = PathBuf::from(file_path_raw);
            let directory = table
                .get("directory")
                .and_then(|value| value.as_str())
                .map(PathBuf::from);
            let resolved_file_path = if file_path.is_absolute() {
                file_path
            } else if let Some(directory) = directory {
                directory.join(file_path)
            } else {
                file_path
            };
            let args = if let Some(values) =
                table.get("arguments").and_then(|value| value.as_array())
            {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            } else if let Some(command) = table.get("command").and_then(|value| value.as_str()) {
                shell_words::split(command).unwrap_or_default()
            } else {
                Vec::new()
            };
            if args.is_empty() {
                continue;
            }
            let sanitized =
                Self::sanitize_compdb_args(args.as_slice(), resolved_file_path.as_path());
            if sanitized.is_empty() {
                continue;
            }
            let normalized_path = Self::normalize_path(resolved_file_path.as_path());
            mapping.insert(normalized_path.clone(), sanitized.clone());
            command_entries.push(CompdbCommandEntry {
                source_path: normalized_path,
                args: sanitized,
            });
        }
        if mapping.is_empty() {
            (key, None, None)
        } else {
            (
                key,
                Some(Arc::new(mapping)),
                Some(Arc::new(command_entries)),
            )
        }
    }

    fn looks_like_compiler_executable(argument: &str) -> bool {
        let lowercase = argument.to_ascii_lowercase();
        lowercase.ends_with("clang")
            || lowercase.ends_with("clang++")
            || lowercase.ends_with("gcc")
            || lowercase.ends_with("g++")
            || lowercase.ends_with("cc")
            || lowercase.ends_with("c++")
            || lowercase.contains("/clang")
            || lowercase.contains("/g++")
            || lowercase.contains("/gcc")
    }

    fn normalize_stem_for_consensus(stem: &str) -> String {
        let lowercase = stem.trim().to_ascii_lowercase();
        if lowercase.is_empty() {
            return String::new();
        }
        let suffixes = [
            "_benchmark",
            "benchmark",
            "_bench",
            "bench",
            "_test",
            "test",
        ];
        for suffix in suffixes {
            if let Some(trimmed) = lowercase.strip_suffix(suffix) {
                let cleaned = trimmed.trim_matches('_');
                if !cleaned.is_empty() {
                    return cleaned.to_string();
                }
            }
        }
        lowercase
    }
}
