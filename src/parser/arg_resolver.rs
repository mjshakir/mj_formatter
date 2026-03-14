use std::collections::HashSet;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, OnceLock};

use dashmap::DashMap;
use moka::sync::Cache;

use crate::config::enums::ClangArgsMode;

const INCLUDE_ARGS_CACHE_SIZE: u64 = 2_048;

fn probe_cache() -> &'static DashMap<String, ClangProbeOutputs> {
    static CACHE: OnceLock<DashMap<String, ClangProbeOutputs>> = OnceLock::new();
    CACHE.get_or_init(DashMap::new)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ClangProbeOutputs {
    resource_dir: Option<String>,
    system_include_args: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ClangArgumentResolver {
    clang_binary: String,
    clang_args: Vec<String>,
    clang_args_mode: ClangArgsMode,
    cpp_standard: String,
    clang_resource_dir: Option<String>,
    clang_system_include_args: Arc<Vec<String>>,
    allow_inferred_includes: bool,
    inferred_include_cache: Cache<String, Arc<Vec<String>>>,
}

impl ClangArgumentResolver {
    pub(crate) fn new(
        clang_binary: String,
        clang_args: Vec<String>,
        clang_args_mode: ClangArgsMode,
        cpp_standard: String,
        allow_inferred_includes: bool,
    ) -> Self {
        let probe_outputs = Self::probe_outputs(clang_binary.as_str());
        Self {
            clang_binary,
            clang_args,
            clang_args_mode,
            cpp_standard,
            clang_resource_dir: probe_outputs.resource_dir,
            clang_system_include_args: Arc::new(probe_outputs.system_include_args),
            allow_inferred_includes,
            inferred_include_cache: Cache::builder()
                .max_capacity(INCLUDE_ARGS_CACHE_SIZE)
                .build(),
        }
    }

    pub(crate) fn build(&self, path: &Path, compdb: Option<Vec<String>>) -> Vec<String> {
        let language = Self::clang_language(path);
        let mut arguments = vec![
            "-fsyntax-only".to_string(),
            "-Wno-everything".to_string(),
            "-ferror-limit=0".to_string(),
        ];
        if compdb.is_none() {
            let standard = self.standard_for_language(language);
            arguments.push(format!("-std={standard}"));
            arguments.push(format!("-x{language}"));
        }

        let mut selected_args = self.selected_clang_args(compdb.clone());
        if let Some(resource_dir) = self.clang_resource_dir.as_ref() {
            let has_resource_dir = selected_args
                .iter()
                .any(|arg| arg.starts_with("-resource-dir"));
            if !has_resource_dir {
                arguments.push(format!("-resource-dir={resource_dir}"));
            }
            let builtin_include = format!("{resource_dir}/include");
            let builtin_include_flag = format!("-isystem{builtin_include}");
            if !selected_args.iter().any(|arg| arg == &builtin_include_flag)
                && !arguments.iter().any(|arg| arg == &builtin_include_flag)
            {
                arguments.push(builtin_include_flag);
            }
        }
        if !selected_args.iter().any(|arg| {
            matches!(arg.as_str(), "-nostdinc" | "-nostdinc++")
                || arg.starts_with("--sysroot")
                || arg.starts_with("-isysroot")
        }) && !self.clang_system_include_args.is_empty()
        {
            for include in self.clang_system_include_args.iter() {
                if !selected_args.iter().any(|arg| arg == include) {
                    arguments.push(include.clone());
                }
            }
        }
        arguments.append(&mut selected_args);

        if self.allow_inferred_includes && compdb.is_none() {
            arguments.extend(self.inferred_include_args_cached(path));
        }

        arguments
    }

    pub(crate) fn parse_system_include_dirs(stderr: &str) -> Vec<String> {
        let mut in_search_list = false;
        let mut args = Vec::<String>::new();
        let mut seen = HashSet::<String>::new();

        for line in stderr.lines() {
            let trimmed = line.trim();
            if trimmed == "#include <...> search starts here:" {
                in_search_list = true;
                continue;
            }
            if !in_search_list {
                continue;
            }
            if trimmed == "End of search list." {
                break;
            }
            if trimmed.is_empty() {
                continue;
            }
            let path = trimmed
                .strip_suffix(" (framework directory)")
                .unwrap_or(trimmed)
                .trim();
            if path.is_empty() {
                continue;
            }
            if seen.insert(path.to_string()) {
                args.push(format!("-isystem{path}"));
            }
        }

        args
    }

    pub(crate) fn hash_cache_inputs<H: Hasher>(&self, hasher: &mut H) {
        self.clang_binary.hash(hasher);
        self.clang_args.hash(hasher);
        self.clang_args_mode.hash(hasher);
        self.cpp_standard.hash(hasher);
        self.clang_resource_dir.hash(hasher);
        self.clang_system_include_args.hash(hasher);
        self.allow_inferred_includes.hash(hasher);
    }

    fn selected_clang_args(&self, compdb: Option<Vec<String>>) -> Vec<String> {
        let compdb = compdb.unwrap_or_default();
        match self.clang_args_mode {
            ClangArgsMode::ArgsOnly => self.clang_args.clone(),
            ClangArgsMode::CompdbOnly => compdb,
            ClangArgsMode::CompdbThenArgs => {
                if compdb.is_empty() {
                    self.clang_args.clone()
                } else {
                    compdb
                }
            }
            ClangArgsMode::Merge => {
                let mut merged = self.clang_args.clone();
                merged.extend(compdb);
                merged
            }
        }
    }

    fn inferred_include_args_cached(&self, path: &Path) -> Vec<String> {
        let key = path
            .parent()
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string_lossy().to_string());
        if let Some(cached) = self.inferred_include_cache.get(&key) {
            return cached.as_ref().clone();
        }
        let computed = Self::inferred_include_args(path);
        self.inferred_include_cache
            .insert(key, Arc::new(computed.clone()));
        computed
    }

    fn inferred_include_args(path: &Path) -> Vec<String> {
        let mut args = Vec::new();
        let mut seen = HashSet::new();
        let mut push = |candidate: &Path| {
            if !candidate.is_dir() {
                return;
            }
            let normalized =
                fs::canonicalize(candidate).unwrap_or_else(|_| candidate.to_path_buf());
            if seen.insert(normalized.clone()) {
                args.push(format!("-I{}", normalized.to_string_lossy()));
            }
        };

        if let Some(parent) = path.parent() {
            push(parent);
            push(parent.join("include").as_path());
            if let Some(grand_parent) = parent.parent() {
                push(grand_parent);
                push(grand_parent.join("include").as_path());
            }
        }
        if let Ok(cwd) = std::env::current_dir() {
            push(cwd.as_path());
            push(cwd.join("include").as_path());
        }

        args
    }

    fn probe_outputs(clang_binary: &str) -> ClangProbeOutputs {
        Self::probe_outputs_with(clang_binary, |binary| ClangProbeOutputs {
            resource_dir: Self::detect_clang_resource_dir(binary),
            system_include_args: Self::detect_clang_system_include_args(binary),
        })
    }

    fn probe_outputs_with<F>(clang_binary: &str, probe: F) -> ClangProbeOutputs
    where
        F: FnOnce(&str) -> ClangProbeOutputs,
    {
        let cache = probe_cache();
        if let Some(cached) = cache.get(clang_binary) {
            return cached.clone();
        }
        let computed = probe(clang_binary);
        cache.insert(clang_binary.to_string(), computed.clone());
        computed
    }

    fn detect_clang_resource_dir(clang_binary: &str) -> Option<String> {
        let output = Command::new(clang_binary)
            .arg("-print-resource-dir")
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let value = String::from_utf8(output.stdout).ok()?;
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    fn detect_clang_system_include_args(clang_binary: &str) -> Vec<String> {
        let output = match Command::new(clang_binary)
            .arg("-E")
            .arg("-x")
            .arg("c++")
            .arg("-")
            .arg("-v")
            .output()
        {
            Ok(value) => value,
            Err(_) => return Vec::new(),
        };

        let stderr = String::from_utf8_lossy(output.stderr.as_slice());
        Self::parse_system_include_dirs(stderr.as_ref())
    }

    fn standard_for_language(&self, language: &str) -> String {
        if language == "c" {
            "gnu11".to_string()
        } else {
            self.cpp_standard.clone()
        }
    }

    fn clang_language(path: &Path) -> &'static str {
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .unwrap_or_default();
        if extension == "c" {
            "c"
        } else {
            "c++"
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{ClangArgumentResolver, ClangProbeOutputs};

    #[test]
    fn probe_outputs_are_cached_per_binary() {
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        let binary = format!("clang-probe-cache-test-{}", std::process::id());

        let first = ClangArgumentResolver::probe_outputs_with(binary.as_str(), |_| {
            CALLS.fetch_add(1, Ordering::SeqCst);
            ClangProbeOutputs {
                resource_dir: Some("/tmp/resource".to_string()),
                system_include_args: vec!["-isystem/tmp/include".to_string()],
            }
        });
        let second = ClangArgumentResolver::probe_outputs_with(binary.as_str(), |_| {
            CALLS.fetch_add(1, Ordering::SeqCst);
            ClangProbeOutputs {
                resource_dir: Some("/tmp/other".to_string()),
                system_include_args: vec!["-isystem/tmp/other".to_string()],
            }
        });

        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(first, second);
    }

    #[test]
    fn probe_outputs_are_isolated_by_binary() {
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        let first_binary = format!("clang-probe-a-{}", std::process::id());
        let second_binary = format!("clang-probe-b-{}", std::process::id());

        let first = ClangArgumentResolver::probe_outputs_with(first_binary.as_str(), |_| {
            CALLS.fetch_add(1, Ordering::SeqCst);
            ClangProbeOutputs {
                resource_dir: Some("first".to_string()),
                system_include_args: vec!["-isystemfirst".to_string()],
            }
        });
        let second = ClangArgumentResolver::probe_outputs_with(second_binary.as_str(), |_| {
            CALLS.fetch_add(1, Ordering::SeqCst);
            ClangProbeOutputs {
                resource_dir: Some("second".to_string()),
                system_include_args: vec!["-isystemsecond".to_string()],
            }
        });

        assert_eq!(CALLS.load(Ordering::SeqCst), 2);
        assert_ne!(first, second);
    }
}
