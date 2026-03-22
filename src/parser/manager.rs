use std::cell::RefCell;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use moka::sync::Cache;
use tree_sitter::Parser;

const C_EXTENSIONS: &[&str] = &["c", "h"];

use crate::config::enums::ClangArgsMode;
use crate::parser::arg_resolver::ClangArgumentResolver;
use crate::parser::clang_result::ClangParseResult;
use crate::parser::clang_service::{ClangParseHandle, ClangParseService};
use crate::parser::compdb_index::CompdbIndex;
use crate::parser::consensus::ParserConsensusSelector;

thread_local! {
    static C_PARSER: RefCell<Parser> = RefCell::new({
        let mut parser = Parser::new();
        let _ = parser.set_language(&tree_sitter_c::LANGUAGE.into());
        parser
    });
    static CPP_PARSER: RefCell<Parser> = RefCell::new({
        let mut parser = Parser::new();
        let _ = parser.set_language(&tree_sitter_cpp::LANGUAGE.into());
        parser
    });
}

const TREE_SITTER_PARSE_CACHE_VERSION: u8 = 1;
const TREE_SITTER_PARSE_CACHE_SIZE: usize = 4_096;
const CLANG_PARSE_CACHE_VERSION: u8 = 1;
const CLANG_PARSE_CACHE_SIZE: u64 = 4_096;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SemanticCompdbContextKind {
    #[default]
    None,
    Exact,
    PairedSourceHeuristic,
    HeaderConsensus,
    SourceConsensus,
}

#[derive(Clone, Debug)]
pub struct ParserManager {
    tree_sitter_available: bool,
    arg_resolver: ClangArgumentResolver,
    compdb_index: CompdbIndex,
    require_compdb: bool,
    tree_sitter_parse_cache: Cache<u64, tree_sitter::Tree>,
    clang_parse_cache: Cache<u64, std::sync::Arc<ClangParseResult>>,
}

impl ParserManager {
    pub fn new() -> Self {
        Self::with_clang("clang".to_string(), Vec::new())
    }

    pub fn with_clang(clang_binary: String, clang_args: Vec<String>) -> Self {
        Self::with_clang_config(clang_binary, clang_args, None, ClangArgsMode::Merge)
    }

    pub fn with_clang_config(
        clang_binary: String,
        clang_args: Vec<String>,
        clang_compdb_path: Option<PathBuf>,
        clang_args_mode: ClangArgsMode,
    ) -> Self {
        Self::with_clang_config_and_fidelity(
            clang_binary,
            clang_args,
            clang_compdb_path,
            clang_args_mode,
            false,
            true,
        )
    }

    pub fn with_clang_config_and_fidelity(
        clang_binary: String,
        clang_args: Vec<String>,
        clang_compdb_path: Option<PathBuf>,
        clang_args_mode: ClangArgsMode,
        require_compdb: bool,
        allow_inferred_includes: bool,
    ) -> Self {
        Self::with_full_config(
            clang_binary,
            clang_args,
            clang_compdb_path,
            clang_args_mode,
            "c++17".to_string(),
            require_compdb,
            allow_inferred_includes,
        )
    }

    pub fn with_full_config(
        clang_binary: String,
        clang_args: Vec<String>,
        clang_compdb_path: Option<PathBuf>,
        clang_args_mode: ClangArgsMode,
        cpp_standard: String,
        require_compdb: bool,
        allow_inferred_includes: bool,
    ) -> Self {
        Self {
            tree_sitter_available: Self::detect_tree_sitter_availability(),
            arg_resolver: ClangArgumentResolver::new(
                clang_binary,
                clang_args,
                clang_args_mode,
                cpp_standard,
                allow_inferred_includes,
            ),
            compdb_index: CompdbIndex::load(clang_compdb_path),
            require_compdb,
            tree_sitter_parse_cache: Cache::builder()
                .max_capacity(TREE_SITTER_PARSE_CACHE_SIZE as u64)
                .build(),
            clang_parse_cache: Cache::builder()
                .max_capacity(CLANG_PARSE_CACHE_SIZE)
                .build(),
        }
    }

    pub fn tree_sitter_available(&self) -> bool {
        self.tree_sitter_available
    }

    pub fn parse_tree_sitter(&self, text: &str, path: &Path) -> Result<tree_sitter::Tree> {
        self.parse_tree_sitter_with_old(text, path, None)
    }

    pub fn parse_tree_sitter_with_old(
        &self,
        text: &str,
        path: &Path,
        old_tree: Option<&tree_sitter::Tree>,
    ) -> Result<tree_sitter::Tree> {
        if !self.tree_sitter_available {
            return Err(anyhow!("tree-sitter unavailable"));
        }

        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_lowercase())
            .unwrap_or_default();
        let language_tag = if C_EXTENSIONS.contains(&extension.as_str()) {
            0u8
        } else {
            1u8
        };
        let cache_key = Self::tree_sitter_cache_key(path, text, language_tag);
        if let Some(cached) = self.tree_sitter_parse_cache.get(&cache_key) {
            return Ok(cached);
        }

        let parsed = if C_EXTENSIONS.contains(&extension.as_str()) {
            C_PARSER.with(|parser| parser.borrow_mut().parse(text, old_tree))
        } else {
            CPP_PARSER.with(|parser| parser.borrow_mut().parse(text, old_tree))
        };

        let Some(tree) = parsed else {
            return Err(anyhow!("tree-sitter parse failed"));
        };

        self.tree_sitter_parse_cache.insert(cache_key, tree.clone());

        Ok(tree)
    }

    pub fn parse_clang(&self, text: &str, path: &Path) -> Result<Arc<ClangParseResult>> {
        if self.require_compdb && !self.has_semantic_compdb_context_for_path(path) {
            return Err(anyhow!(
                "semantic parse fidelity requires compile_commands entry for {}",
                path.display()
            ));
        }
        let cache_key = self.clang_cache_key(path, text);
        if let Some(cached) = self.clang_parse_cache.get(&cache_key) {
            return Ok(Arc::clone(&cached));
        }

        let service = ClangParseService::global()?;
        let exact_compdb = self.has_exact_compdb_entry_for_path(path);
        let result = if Self::is_header_path(path) && !exact_compdb {
            self.parse_clang_header_consensus(&service, text, path)?
        } else if !exact_compdb {
            let arg_sets = self.source_consensus_arg_sets(path);
            if arg_sets.is_empty() {
                let source_path = path.to_string_lossy().to_string();
                let arguments = self.build_clang_arguments(path);
                service.parse(source_path, text.to_string(), arguments)?
            } else {
                self.parse_clang_source_consensus(&service, text, path, arg_sets)?
            }
        } else {
            let source_path = path.to_string_lossy().to_string();
            let arguments = self.build_clang_arguments(path);
            service.parse(source_path, text.to_string(), arguments)?
        };
        let arc = Arc::new(result);
        self.clang_parse_cache.insert(cache_key, Arc::clone(&arc));
        Ok(arc)
    }

    pub fn dispatch_clang(&self, text: &str, path: &Path) -> Result<Option<ClangParseHandle>> {
        if self.require_compdb && !self.has_semantic_compdb_context_for_path(path) {
            return Ok(None);
        }
        let cache_key = self.clang_cache_key(path, text);
        if self.clang_parse_cache.get(&cache_key).is_some() {
            return Ok(None);
        }
        let service = ClangParseService::global()?;
        let source_path = path.to_string_lossy().to_string();
        let arguments = self.build_clang_arguments(path);
        Ok(Some(service.dispatch(source_path, text.to_string(), arguments)?))
    }

    pub fn collect_clang(
        &self,
        handle: ClangParseHandle,
        text: &str,
        path: &Path,
        deadline: std::time::Instant,
    ) -> Result<Arc<ClangParseResult>> {
        let cache_key = self.clang_cache_key(path, text);
        if let Some(cached) = self.clang_parse_cache.get(&cache_key) {
            return Ok(Arc::clone(&cached));
        }
        let result = handle.collect_deadline(deadline)?;
        let arc = Arc::new(result);
        self.clang_parse_cache.insert(cache_key, Arc::clone(&arc));
        Ok(arc)
    }

    pub fn has_semantic_compdb_context_for_path(&self, path: &Path) -> bool {
        !matches!(
            self.semantic_compdb_context_kind_for_path(path),
            SemanticCompdbContextKind::None
        )
    }

    pub fn has_exact_compdb_entry_for_path(&self, path: &Path) -> bool {
        self.compdb_index.has_exact_entry_for_path(path)
    }

    pub fn semantic_compdb_context_kind_for_path(&self, path: &Path) -> SemanticCompdbContextKind {
        self.compdb_index.semantic_context_kind_for_path(path)
    }

    fn build_clang_arguments(&self, path: &Path) -> Vec<String> {
        let compdb = self.compdb_args_for_path(path);
        self.build_clang_arguments_with_compdb(path, compdb)
    }

    fn build_clang_arguments_with_compdb(
        &self,
        path: &Path,
        compdb: Option<Vec<String>>,
    ) -> Vec<String> {
        self.arg_resolver.build(path, compdb)
    }

    fn compdb_args_for_path(&self, path: &Path) -> Option<Vec<String>> {
        self.compdb_args_exact_for_path(path)
    }

    fn compdb_args_exact_for_path(&self, path: &Path) -> Option<Vec<String>> {
        self.compdb_index.args_exact_for_path(path)
    }

    fn parse_clang_header_consensus(
        &self,
        service: &ClangParseService,
        text: &str,
        path: &Path,
    ) -> Result<ClangParseResult> {
        let (_, arg_sets) = self.header_context_arg_sets(path);
        if arg_sets.is_empty() {
            return Err(anyhow!(
                "semantic parse fidelity requires compile_commands entry for {}",
                path.display()
            ));
        }
        let source_path = path.to_string_lossy().to_string();
        let resolved_argument_sets = arg_sets
            .into_iter()
            .map(|arguments| self.build_clang_arguments_with_compdb(path, Some(arguments)))
            .collect::<Vec<_>>();
        let mut failures = Vec::<String>::new();
        let mut parses = Vec::<ClangParseResult>::new();
        for result in service.parse_batch(source_path, text.to_string(), resolved_argument_sets)? {
            match result {
                Ok(parse) => parses.push(parse),
                Err(err) => failures.push(err.to_string()),
            }
        }
        if parses.is_empty() {
            let detail = if failures.is_empty() {
                "no successful consensus parses".to_string()
            } else {
                failures.join(" | ")
            };
            return Err(anyhow!(
                "header consensus parse failed for {}: {}",
                path.display(),
                detail
            ));
        }
        Ok(Self::merge_header_consensus_results(parses, failures))
    }

    fn parse_clang_source_consensus(
        &self,
        service: &ClangParseService,
        text: &str,
        path: &Path,
        arg_sets: Vec<Vec<String>>,
    ) -> Result<ClangParseResult> {
        if arg_sets.is_empty() {
            return Err(anyhow!(
                "semantic parse fidelity requires compile_commands entry for {}",
                path.display()
            ));
        }
        let source_path = path.to_string_lossy().to_string();
        let resolved_argument_sets = arg_sets
            .into_iter()
            .map(|arguments| self.build_clang_arguments_with_compdb(path, Some(arguments)))
            .collect::<Vec<_>>();
        let mut failures = Vec::<String>::new();
        let mut selected = None::<ClangParseResult>;
        for result in service.parse_batch(source_path, text.to_string(), resolved_argument_sets)? {
            match result {
                Ok(parse) => {
                    if Self::should_replace_source_consensus_parse(selected.as_ref(), &parse) {
                        selected = Some(parse);
                    }
                }
                Err(err) => failures.push(err.to_string()),
            }
        }
        if let Some(parse) = selected {
            return Ok(parse);
        }
        let detail = if failures.is_empty() {
            "no successful source-consensus parses".to_string()
        } else {
            failures.join(" | ")
        };
        Err(anyhow!(
            "source consensus parse failed for {}: {}",
            path.display(),
            detail
        ))
    }

    #[cfg(test)]
    fn header_consensus_arg_sets(&self, header_path: &Path) -> Vec<Vec<String>> {
        self.compdb_index.header_consensus_arg_sets(header_path)
    }

    fn header_context_arg_sets(
        &self,
        header_path: &Path,
    ) -> (SemanticCompdbContextKind, Vec<Vec<String>>) {
        self.compdb_index.header_arg_sets_with_context(header_path)
    }

    fn source_consensus_arg_sets(&self, source_path: &Path) -> Vec<Vec<String>> {
        self.compdb_index.source_consensus_arg_sets(source_path)
    }

    fn merge_header_consensus_results(
        parses: Vec<ClangParseResult>,
        failures: Vec<String>,
    ) -> ClangParseResult {
        ParserConsensusSelector::merge_header_results(parses, failures)
    }

    fn should_replace_source_consensus_parse(
        current: Option<&ClangParseResult>,
        candidate: &ClangParseResult,
    ) -> bool {
        ParserConsensusSelector::should_replace_source_parse(current, candidate)
    }

    #[cfg(test)]
    fn parse_system_include_dirs(stderr: &str) -> Vec<String> {
        ClangArgumentResolver::parse_system_include_dirs(stderr)
    }

    #[cfg(test)]
    fn sanitize_compdb_args(args: &[String], file_path: &Path) -> Vec<String> {
        CompdbIndex::sanitize_compdb_args(args, file_path)
    }

    fn tree_sitter_cache_key(path: &Path, text: &str, language_tag: u8) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        TREE_SITTER_PARSE_CACHE_VERSION.hash(&mut hasher);
        path.to_string_lossy().hash(&mut hasher);
        text.hash(&mut hasher);
        language_tag.hash(&mut hasher);
        hasher.finish()
    }

    fn clang_cache_key(&self, path: &Path, text: &str) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        CLANG_PARSE_CACHE_VERSION.hash(&mut hasher);
        path.to_string_lossy().hash(&mut hasher);
        text.hash(&mut hasher);
        self.arg_resolver.hash_cache_inputs(&mut hasher);
        self.compdb_index.key().hash(&mut hasher);
        self.require_compdb.hash(&mut hasher);
        if let Ok(cwd) = std::env::current_dir() {
            cwd.to_string_lossy().hash(&mut hasher);
        }
        hasher.finish()
    }

    fn is_header_path(path: &Path) -> bool {
        CompdbIndex::is_header_path(path)
    }

    fn detect_tree_sitter_availability() -> bool {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .is_ok()
            || parser.set_language(&tree_sitter_c::LANGUAGE.into()).is_ok()
    }
}
#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{ParserManager, SemanticCompdbContextKind};
    use crate::config::enums::ClangArgsMode;
    use crate::parser::clang_result::{
        ClangDiagnosticEntry, ClangDiagnosticSeverity, ClangDiagnosticSummary, ClangParseResult,
    };
    use crate::parser::clang_symbol::ClangSymbol;
    use crate::parser::clang_types::ClangSymbolKind;

    #[test]
    fn clang_parse_collects_semantic_symbols() {
        let manager = ParserManager::with_clang("clang".to_string(), Vec::new());
        let path = PathBuf::from("sample.cpp");
        let source = "int BadName() { int local_value = 0; return local_value; }\n";
        let result = manager
            .parse_clang(source, &path)
            .expect("clang parse should succeed");

        assert!(result.success);
        assert!(result
            .symbols
            .iter()
            .any(|symbol| symbol.name == "BadName"
                && matches!(symbol.kind, ClangSymbolKind::Function)));
        assert!(result
            .symbols
            .iter()
            .any(|symbol| symbol.name == "local_value"
                && matches!(symbol.kind, ClangSymbolKind::Variable)));
        let offsets = result.rename_offsets_on_line("local_value", 1, &[ClangSymbolKind::Variable]);
        assert!(
            offsets.len() >= 2,
            "expected declaration and reference offsets for local_value"
        );
    }

    #[test]
    fn header_consensus_diagnostics_require_strict_majority() {
        let fatal_parse = ClangParseResult::new(
            false,
            vec!["fatal".to_string()],
            Vec::new(),
            ClangDiagnosticSummary {
                fatal: 1,
                ..ClangDiagnosticSummary::default()
            },
            vec![ClangDiagnosticEntry {
                line: 10,
                column: 4,
                severity: ClangDiagnosticSeverity::Fatal,
            }],
        );
        let clean_parse = ClangParseResult::new(
            true,
            Vec::new(),
            Vec::new(),
            ClangDiagnosticSummary::default(),
            Vec::new(),
        );

        let merged = ParserManager::merge_header_consensus_results(
            vec![fatal_parse, clean_parse],
            Vec::new(),
        );
        assert_eq!(merged.diagnostic_summary().fatal, 0);
        assert_eq!(merged.diagnostic_total(), 0);
    }

    #[test]
    fn header_consensus_ignores_symbol_votes_from_unrecoverable_failed_parses() {
        let failed_symbol = ClangSymbol {
            name: "m_data".to_string(),
            kind: ClangSymbolKind::Field,
            line: 7,
            column: 9,
            usr: Some("usr:test:field:m_data".to_string()),
            scope_usr: Some("usr:test:scope:Holder".to_string()),
        };
        let failed_parse = ClangParseResult::with_semantic_offsets(
            false,
            vec!["fatal-1".to_string(), "fatal-2".to_string()],
            vec![failed_symbol],
            HashMap::new(),
            HashMap::new(),
            ClangDiagnosticSummary {
                fatal: 2,
                ..ClangDiagnosticSummary::default()
            },
            vec![
                ClangDiagnosticEntry {
                    line: 7,
                    column: 9,
                    severity: ClangDiagnosticSeverity::Fatal,
                },
                ClangDiagnosticEntry {
                    line: 9,
                    column: 3,
                    severity: ClangDiagnosticSeverity::Fatal,
                },
            ],
        );
        let successful_parse = ClangParseResult::new(
            true,
            Vec::new(),
            Vec::new(),
            ClangDiagnosticSummary::default(),
            Vec::new(),
        );

        let merged = ParserManager::merge_header_consensus_results(
            vec![failed_parse, successful_parse],
            Vec::new(),
        );
        assert!(
            merged.symbols.is_empty(),
            "unrecoverable failed parses must not vote semantic symbols into header consensus"
        );
    }

    #[test]
    fn header_consensus_accepts_symbol_votes_from_recoverable_failed_parses() {
        let failed_symbol = ClangSymbol {
            name: "m_data".to_string(),
            kind: ClangSymbolKind::Field,
            line: 7,
            column: 9,
            usr: Some("usr:test:field:m_data".to_string()),
            scope_usr: Some("usr:test:scope:Holder".to_string()),
        };
        let failed_parse = ClangParseResult::with_semantic_offsets(
            false,
            vec!["fatal".to_string()],
            vec![failed_symbol.clone()],
            HashMap::new(),
            HashMap::new(),
            ClangDiagnosticSummary {
                fatal: 1,
                ..ClangDiagnosticSummary::default()
            },
            vec![ClangDiagnosticEntry {
                line: 7,
                column: 9,
                severity: ClangDiagnosticSeverity::Fatal,
            }],
        );
        let successful_parse = ClangParseResult::new(
            true,
            Vec::new(),
            Vec::new(),
            ClangDiagnosticSummary::default(),
            Vec::new(),
        );

        let merged = ParserManager::merge_header_consensus_results(
            vec![failed_parse, successful_parse],
            Vec::new(),
        );
        assert!(merged
            .symbols
            .iter()
            .any(|symbol| symbol.name == failed_symbol.name
                && symbol.line == failed_symbol.line
                && symbol.column == failed_symbol.column
                && symbol.kind == failed_symbol.kind));
    }

    #[test]
    fn strict_fidelity_requires_compile_commands_entry() {
        let manager = ParserManager::with_clang_config_and_fidelity(
            "clang".to_string(),
            Vec::new(),
            None,
            ClangArgsMode::Merge,
            true,
            false,
        );
        let path = PathBuf::from("no_compdb.cpp");
        let source = "int x = 0;\n";
        let err = manager
            .parse_clang(source, &path)
            .expect_err("strict fidelity should reject missing compile_commands entry");
        assert!(err
            .to_string()
            .contains("semantic parse fidelity requires compile_commands entry"));
    }

    #[test]
    fn strict_fidelity_uses_compdb_args_without_inferred_or_fallback_flags() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock drift")
            .as_nanos();
        let temp_root = std::env::temp_dir().join(format!(
            "mjf_compdb_args_lock_test_{}_{}",
            std::process::id(),
            nonce
        ));
        fs::create_dir_all(&temp_root).expect("create temp root");
        let source_path = temp_root.join("sample.cpp");
        let compdb_path = temp_root.join("compile_commands.json");
        fs::write(&source_path, "int source_symbol() { return 0; }\n").expect("write source");
        let compile_commands = serde_json::json!([{
            "directory": temp_root.to_string_lossy().to_string(),
            "file": source_path.to_string_lossy().to_string(),
            "arguments": [
                "/usr/bin/clang++",
                "-std=gnu++17",
                "-DMJF_PARSE_FIDELITY_LOCK=1",
                "-c",
                source_path.to_string_lossy().to_string()
            ]
        }]);
        fs::write(&compdb_path, compile_commands.to_string()).expect("write compdb");

        let manager = ParserManager::with_clang_config_and_fidelity(
            "clang".to_string(),
            Vec::new(),
            Some(compdb_path.clone()),
            ClangArgsMode::CompdbOnly,
            true,
            true,
        );
        let args = manager.build_clang_arguments(source_path.as_path());

        assert!(args.iter().any(|arg| arg == "-std=gnu++17"));
        assert!(args.iter().any(|arg| arg == "-DMJF_PARSE_FIDELITY_LOCK=1"));
        assert!(!args.iter().any(|arg| arg == "-std=gnu++20"));
        assert!(!args.iter().any(|arg| arg.starts_with("-x")));
        let normalized_root = fs::canonicalize(&temp_root).unwrap_or(temp_root.clone());
        let inferred_root_flag = format!("-I{}", normalized_root.to_string_lossy());
        assert!(
            !args.iter().any(|arg| arg == &inferred_root_flag),
            "inferred include args should be suppressed when compile_commands args are available"
        );

        let _ = fs::remove_dir_all(temp_root);
    }

    #[test]
    fn sanitize_compdb_args_removes_compiler_and_build_outputs() {
        let file_path = PathBuf::from("/tmp/sample.cpp");
        let args = vec![
            "/usr/bin/clang++".to_string(),
            "-std=gnu++20".to_string(),
            "-O2".to_string(),
            "-c".to_string(),
            "/tmp/sample.cpp".to_string(),
            "-o".to_string(),
            "sample.o".to_string(),
        ];
        let sanitized = ParserManager::sanitize_compdb_args(args.as_slice(), file_path.as_path());
        assert_eq!(sanitized, vec!["-std=gnu++20", "-O2"]);
    }

    #[test]
    fn parse_system_include_dirs_extracts_paths() {
        let stderr = r#"
clang version 19.1.0
#include <...> search starts here:
 /usr/lib/clang/19/include
 /usr/local/include
 /usr/include/c++/14
 /usr/include
End of search list.
"#;
        let parsed = ParserManager::parse_system_include_dirs(stderr);
        assert_eq!(
            parsed,
            vec![
                "-isystem/usr/lib/clang/19/include",
                "-isystem/usr/local/include",
                "-isystem/usr/include/c++/14",
                "-isystem/usr/include"
            ]
        );
    }

    #[test]
    fn parse_system_include_dirs_deduplicates_entries() {
        let stderr = r#"
#include <...> search starts here:
 /usr/include
 /usr/include
End of search list.
"#;
        let parsed = ParserManager::parse_system_include_dirs(stderr);
        assert_eq!(parsed, vec!["-isystem/usr/include"]);
    }

    #[test]
    fn strict_fidelity_parses_header_with_compdb_consensus_context() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock drift")
            .as_nanos();
        let temp_root = std::env::temp_dir().join(format!(
            "mjf_compdb_header_test_{}_{}",
            std::process::id(),
            nonce
        ));
        fs::create_dir_all(&temp_root).expect("create temp root");
        let source_path = temp_root.join("sample.cpp");
        let peer_source_path = temp_root.join("sample_impl.cpp");
        let header_path = temp_root.join("sample.hpp");
        let compdb_path = temp_root.join("compile_commands.json");
        fs::write(&source_path, "int source_symbol() { return 0; }\n").expect("write source");
        fs::write(
            &peer_source_path,
            "int source_peer_symbol() { return 1; }\n",
        )
        .expect("write peer source");
        fs::write(&header_path, "int header_symbol();\n").expect("write header");
        let compile_commands = serde_json::json!([
            {
                "directory": temp_root.to_string_lossy().to_string(),
                "file": source_path.to_string_lossy().to_string(),
                "arguments": [
                    "/usr/bin/clang++",
                    "-std=gnu++20",
                    "-DMJF_HEADER_TU_A=1",
                    "-c",
                    source_path.to_string_lossy().to_string()
                ]
            },
            {
                "directory": temp_root.to_string_lossy().to_string(),
                "file": peer_source_path.to_string_lossy().to_string(),
                "arguments": [
                    "/usr/bin/clang++",
                    "-std=gnu++20",
                    "-DMJF_HEADER_TU_B=1",
                    "-c",
                    peer_source_path.to_string_lossy().to_string()
                ]
            }
        ]);
        fs::write(&compdb_path, compile_commands.to_string()).expect("write compdb");

        let manager = ParserManager::with_clang_config_and_fidelity(
            "clang".to_string(),
            Vec::new(),
            Some(compdb_path.clone()),
            ClangArgsMode::CompdbOnly,
            true,
            false,
        );
        assert!(
            !manager.has_exact_compdb_entry_for_path(&header_path),
            "header has no exact compile_commands entry"
        );
        assert_eq!(
            manager.semantic_compdb_context_kind_for_path(&header_path),
            SemanticCompdbContextKind::PairedSourceHeuristic
        );
        assert!(
            manager.has_semantic_compdb_context_for_path(&header_path),
            "header should have consensus context from real TUs"
        );
        let arg_sets = manager.header_consensus_arg_sets(header_path.as_path());
        assert!(
            arg_sets.len() >= 2,
            "expected multiple TU arg sets for header consensus"
        );
        let (active_kind, active_arg_sets) = manager.header_context_arg_sets(header_path.as_path());
        assert_eq!(
            active_kind,
            SemanticCompdbContextKind::PairedSourceHeuristic
        );
        assert_eq!(
            active_arg_sets.len(),
            1,
            "paired-source heuristic should prefer the direct companion TU"
        );
        let result = manager
            .parse_clang("int header_symbol();\n", &header_path)
            .expect("header parse should succeed via consensus");
        assert!(
            !result.symbols.is_empty() || result.diagnostic_total() > 0,
            "consensus parse should return semantic symbols or diagnostics"
        );

        let _ = fs::remove_dir_all(temp_root);
    }

    #[test]
    fn strict_fidelity_parses_source_with_compdb_derived_source_consensus() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock drift")
            .as_nanos();
        let temp_root = std::env::temp_dir().join(format!(
            "mjf_compdb_source_consensus_test_{}_{}",
            std::process::id(),
            nonce
        ));
        fs::create_dir_all(&temp_root).expect("create temp root");
        let known_source = temp_root.join("RetireMapTest.cpp");
        let unknown_source = temp_root.join("RetireMap.cpp");
        let compdb_path = temp_root.join("compile_commands.json");
        fs::write(&known_source, "int known_symbol() { return 0; }\n").expect("write source");
        fs::write(&unknown_source, "int unknown_symbol() { return 1; }\n").expect("write source");
        let compile_commands = serde_json::json!([{
            "directory": temp_root.to_string_lossy().to_string(),
            "file": known_source.to_string_lossy().to_string(),
            "arguments": [
                "/usr/bin/clang++",
                "-std=gnu++20",
                "-I.",
                "-c",
                known_source.to_string_lossy().to_string()
            ]
        }]);
        fs::write(&compdb_path, compile_commands.to_string()).expect("write compdb");

        let manager = ParserManager::with_clang_config_and_fidelity(
            "clang".to_string(),
            Vec::new(),
            Some(compdb_path.clone()),
            ClangArgsMode::CompdbOnly,
            true,
            false,
        );
        assert!(
            !manager.has_exact_compdb_entry_for_path(&unknown_source),
            "source should not have exact compile_commands entry"
        );
        assert!(
            manager.has_semantic_compdb_context_for_path(&unknown_source),
            "source should receive semantic context from related compdb TU entries"
        );
        let result = manager
            .parse_clang("int unknown_symbol() { return 1; }\n", &unknown_source)
            .expect("source parse should succeed via source consensus");
        assert!(
            result.success || result.diagnostic_total() > 0,
            "source consensus parse should produce success or diagnostics"
        );

        let _ = fs::remove_dir_all(temp_root);
    }

    #[test]
    fn strict_fidelity_rejects_source_without_exact_compdb_entry() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock drift")
            .as_nanos();
        let temp_root = std::env::temp_dir().join(format!(
            "mjf_compdb_source_fallback_test_{}_{}",
            std::process::id(),
            nonce
        ));
        fs::create_dir_all(&temp_root).expect("create temp root");
        let known_source = temp_root.join("known.cpp");
        let unknown_source = temp_root.join("unknown.cpp");
        let compdb_path = temp_root.join("compile_commands.json");
        fs::write(&known_source, "int known_symbol() { return 0; }\n").expect("write source");
        fs::write(&unknown_source, "int unknown_symbol() { return 1; }\n").expect("write source");
        let compile_commands = serde_json::json!([{
            "directory": temp_root.to_string_lossy().to_string(),
            "file": known_source.to_string_lossy().to_string(),
            "arguments": [
                "/usr/bin/clang++",
                "-std=gnu++20",
                "-c",
                known_source.to_string_lossy().to_string()
            ]
        }]);
        fs::write(&compdb_path, compile_commands.to_string()).expect("write compdb");

        let manager = ParserManager::with_clang_config_and_fidelity(
            "clang".to_string(),
            Vec::new(),
            Some(compdb_path.clone()),
            ClangArgsMode::CompdbOnly,
            true,
            false,
        );
        assert!(
            !manager.has_exact_compdb_entry_for_path(&unknown_source),
            "source without exact compile_commands entry must be rejected"
        );
        let err = manager
            .parse_clang("int unknown_symbol() { return 1; }\n", &unknown_source)
            .expect_err("source parse must fail without exact compile_commands entry");
        assert!(err
            .to_string()
            .contains("semantic parse fidelity requires compile_commands entry"));

        let _ = fs::remove_dir_all(temp_root);
    }
}
