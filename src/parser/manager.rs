use std::cell::RefCell;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use moka::sync::Cache;
use tree_sitter::{InputEdit, ParseOptions, Parser, Point, Tree};

const C_EXTENSIONS: &[&str] = &["c", "h"];

use crate::config::enums::ClangArgsMode;
use crate::parser::arg_resolver::ClangArgumentResolver;
use crate::parser::clang_result::ClangParseResult;
use crate::parser::clang_service::{ClangParseHandle, ClangParseService};
use crate::parser::compdb_index::CompdbIndex;
use crate::parser::consensus::ParserConsensusSelector;

const PARSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

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

fn parse_with_timeout(
    parser: &mut Parser,
    text: &str,
    old_tree: Option<&Tree>,
) -> Option<Tree> {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let start = std::time::Instant::now();
    let mut callback = |state: &tree_sitter::ParseState| {
        let _ = state;
        if start.elapsed() > PARSE_TIMEOUT {
            std::ops::ControlFlow::Break(())
        } else {
            std::ops::ControlFlow::Continue(())
        }
    };
    let options = ParseOptions::new().progress_callback(&mut callback);
    parser.parse_with_options(
        &mut |i, _| {
            if i < len { &bytes[i..] } else { &[] }
        },
        old_tree,
        Some(options),
    )
}

pub fn compute_input_edit(old_text: &str, new_text: &str) -> InputEdit {
    let old_bytes = old_text.as_bytes();
    let new_bytes = new_text.as_bytes();
    let common_prefix = old_bytes.iter().zip(new_bytes.iter())
        .position(|(a, b)| a != b)
        .unwrap_or(old_bytes.len().min(new_bytes.len()));
    let old_suffix_start = old_bytes.len();
    let new_suffix_start = new_bytes.len();
    let max_suffix = (old_suffix_start - common_prefix).min(new_suffix_start - common_prefix);
    let common_suffix = (0..max_suffix)
        .take_while(|&i| {
            old_bytes[old_suffix_start - 1 - i] == new_bytes[new_suffix_start - 1 - i]
        })
        .count();
    let start_byte = common_prefix;
    let old_end_byte = old_suffix_start - common_suffix;
    let new_end_byte = new_suffix_start - common_suffix;
    InputEdit {
        start_byte,
        old_end_byte,
        new_end_byte,
        start_position: byte_offset_to_point(old_text, start_byte),
        old_end_position: byte_offset_to_point(old_text, old_end_byte),
        new_end_position: byte_offset_to_point(new_text, new_end_byte),
    }
}

fn byte_offset_to_point(text: &str, byte_offset: usize) -> Point {
    let slice = &text.as_bytes()[..byte_offset];
    let row = memchr::memchr_iter(b'\n', slice).count();
    let last_newline = memchr::memrchr(b'\n', slice).map(|i| i + 1).unwrap_or(0);
    Point { row, column: byte_offset - last_newline }
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
        Self::with_clang_cfg(
            clang_binary,
            clang_args,
            clang_compdb_path,
            clang_args_mode,
            false,
            true,
        )
    }

    pub fn with_clang_cfg(
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
                .time_to_live(std::time::Duration::from_secs(1800))
                .build(),
        }
    }

    pub fn tree_sitter_available(&self) -> bool {
        self.tree_sitter_available
    }

    #[tracing::instrument(skip(self, text), fields(file = %path.display()))]
    pub fn parse_tree_sitter(&self, text: &str, path: &Path) -> Result<tree_sitter::Tree> {
        self.reparse_tree(text, path, None)
    }

    pub fn reparse_tree(
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
        self.tree_sitter_parse_cache
            .try_get_with(cache_key, || {
                let parsed = if C_EXTENSIONS.contains(&extension.as_str()) {
                    C_PARSER.with(|parser| parse_with_timeout(&mut parser.borrow_mut(), text, old_tree))
                } else {
                    CPP_PARSER.with(|parser| parse_with_timeout(&mut parser.borrow_mut(), text, old_tree))
                };
                parsed.ok_or_else(|| anyhow!("tree-sitter parse failed"))
            })
            .map_err(|e| anyhow!("{e}"))
    }

    pub fn reparse_tree_incremental(
        &self,
        old_text: &str,
        new_text: &str,
        path: &Path,
        old_tree: &Tree,
    ) -> Result<Tree> {
        if !self.tree_sitter_available {
            return Err(anyhow!("tree-sitter unavailable"));
        }
        let mut tree = old_tree.clone();
        let edit = compute_input_edit(old_text, new_text);
        tree.edit(&edit);
        let extension = path
            .extension()
            .and_then(|v| v.to_str())
            .map(|v| v.to_lowercase())
            .unwrap_or_default();
        let parsed = if C_EXTENSIONS.contains(&extension.as_str()) {
            C_PARSER.with(|parser| parse_with_timeout(&mut parser.borrow_mut(), new_text, Some(&tree)))
        } else {
            CPP_PARSER.with(|parser| parse_with_timeout(&mut parser.borrow_mut(), new_text, Some(&tree)))
        };
        parsed.ok_or_else(|| anyhow!("tree-sitter incremental parse failed"))
    }

    #[tracing::instrument(skip(self, text), fields(file = %path.display()))]
    pub fn parse_clang(&self, text: &str, path: &Path) -> Result<Arc<ClangParseResult>> {
        if self.require_compdb && !self.has_semantic_compdb(path) {
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
        let exact_compdb = self.has_exact_compdb(path);
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
        if self.require_compdb && !self.has_semantic_compdb(path) {
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

    pub fn has_semantic_compdb(&self, path: &Path) -> bool {
        !matches!(
            self.semantic_compdb_kind(path),
            SemanticCompdbContextKind::None
        )
    }

    pub fn has_exact_compdb(&self, path: &Path) -> bool {
        self.compdb_index.has_exact_entry_for_path(path)
    }

    pub fn semantic_compdb_kind(&self, path: &Path) -> SemanticCompdbContextKind {
        self.compdb_index.semantic_context_kind_for_path(path)
    }

    fn build_clang_arguments(&self, path: &Path) -> Vec<String> {
        let compdb = self.compdb_args(path);
        self.build_clang_args(path, compdb)
    }

    fn build_clang_args(
        &self,
        path: &Path,
        compdb: Option<Vec<String>>,
    ) -> Vec<String> {
        self.arg_resolver.build(path, compdb)
    }

    fn compdb_args(&self, path: &Path) -> Option<Vec<String>> {
        self.compdb_args_exact(path)
    }

    fn compdb_args_exact(&self, path: &Path) -> Option<Vec<String>> {
        self.compdb_index.args_exact(path)
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
            .map(|arguments| self.build_clang_args(path, Some(arguments)))
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
            .map(|arguments| self.build_clang_args(path, Some(arguments)))
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
        let mut hasher = rustc_hash::FxHasher::default();
        TREE_SITTER_PARSE_CACHE_VERSION.hash(&mut hasher);
        path.to_string_lossy().hash(&mut hasher);
        text.hash(&mut hasher);
        language_tag.hash(&mut hasher);
        hasher.finish()
    }

    fn clang_cache_key(&self, path: &Path, text: &str) -> u64 {
        let mut hasher = rustc_hash::FxHasher::default();
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
    use rustc_hash::FxHashMap;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{ParserManager, SemanticCompdbContextKind};
    use crate::config::enums::ClangArgsMode;
    use crate::parser::clang_result::{
        ClangDiagnosticEntry, ClangParseResult,
    };
    use crate::parser::file_context::SemanticDeclaration;

    #[test]
    fn parse_collects_symbols() {
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
                && symbol.kind == clang_sys::CXCursor_FunctionDecl));
        assert!(result
            .symbols
            .iter()
            .any(|symbol| symbol.name == "local_value"
                && symbol.kind == clang_sys::CXCursor_VarDecl));
        let has_rename_offsets = result.rename_offsets_map().values().any(|v| v.len() >= 2);
        assert!(
            has_rename_offsets,
            "expected rename offsets with at least 2 locations"
        );
    }

    #[test]
    fn consensus_strict_majority() {
        let fatal_parse = ClangParseResult::new(
            false,
            vec!["fatal".to_string()],
            Vec::new(),
            {
                let mut c: [usize; 5] = [0; 5];
                c[clang_sys::CXDiagnostic_Fatal as usize] = 1;
                c
            },
            vec![ClangDiagnosticEntry {
                line: 10,
                column: 4,
                severity: clang_sys::CXDiagnostic_Fatal as u32,
                warning_option: String::new(),
                fix_its: Vec::new(),
            }],
        );
        let clean_parse = ClangParseResult::new(
            true,
            Vec::new(),
            Vec::new(),
            [0; 5],
            Vec::new(),
        );

        let merged = ParserManager::merge_header_consensus_results(
            vec![fatal_parse, clean_parse],
            Vec::new(),
        );
        assert_eq!(merged.diagnostic_counts()[clang_sys::CXDiagnostic_Fatal as usize], 0);
        assert_eq!(merged.diagnostic_total(), 0);
    }

    #[test]
    fn consensus_ignores_unrecoverable() {
        let failed_symbol = SemanticDeclaration {
            name: "m_data".to_string(),
            kind: clang_sys::CXCursor_FieldDecl,
            line: 7,
            column: 9,
            usr: Some("usr:test:field:m_data".to_string()),
            scope_usr: Some("usr:test:scope:Holder".to_string()),
            ..Default::default()
        };
        let failed_parse = ClangParseResult::with_semantic_offsets(
            false,
            vec!["fatal-1".to_string(), "fatal-2".to_string()],
            vec![failed_symbol],
            FxHashMap::default(),
            FxHashMap::default(),
            {
                let mut c: [usize; 5] = [0; 5];
                c[clang_sys::CXDiagnostic_Fatal as usize] = 2;
                c
            },
            vec![
                ClangDiagnosticEntry {
                    line: 7,
                    column: 9,
                    severity: clang_sys::CXDiagnostic_Fatal as u32,
                    warning_option: String::new(),
                    fix_its: Vec::new(),
                },
                ClangDiagnosticEntry {
                    line: 9,
                    column: 3,
                    severity: clang_sys::CXDiagnostic_Fatal as u32,
                    warning_option: String::new(),
                    fix_its: Vec::new(),
                },
            ],
        );
        let successful_parse = ClangParseResult::new(
            true,
            Vec::new(),
            Vec::new(),
            [0; 5],
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
    fn consensus_accepts_recoverable() {
        let failed_symbol = SemanticDeclaration {
            name: "m_data".to_string(),
            kind: clang_sys::CXCursor_FieldDecl,
            line: 7,
            column: 9,
            usr: Some("usr:test:field:m_data".to_string()),
            scope_usr: Some("usr:test:scope:Holder".to_string()),
            ..Default::default()
        };
        let failed_parse = ClangParseResult::with_semantic_offsets(
            false,
            vec!["fatal".to_string()],
            vec![failed_symbol.clone()],
            FxHashMap::default(),
            FxHashMap::default(),
            {
                let mut c: [usize; 5] = [0; 5];
                c[clang_sys::CXDiagnostic_Fatal as usize] = 1;
                c
            },
            vec![ClangDiagnosticEntry {
                line: 7,
                column: 9,
                severity: clang_sys::CXDiagnostic_Fatal as u32,
                warning_option: String::new(),
                fix_its: Vec::new(),
            }],
        );
        let successful_parse = ClangParseResult::new(
            true,
            Vec::new(),
            Vec::new(),
            [0; 5],
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
    fn fidelity_requires_compdb() {
        let manager = ParserManager::with_clang_cfg(
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
    fn fidelity_uses_compdb() {
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

        let manager = ParserManager::with_clang_cfg(
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
    fn sanitize_removes_compiler() {
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
    fn parse_extracts_paths() {
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
    fn parse_deduplicates_entries() {
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
    fn fidelity_parses_header() {
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

        let manager = ParserManager::with_clang_cfg(
            "clang".to_string(),
            Vec::new(),
            Some(compdb_path.clone()),
            ClangArgsMode::CompdbOnly,
            true,
            false,
        );
        assert!(
            !manager.has_exact_compdb(&header_path),
            "header has no exact compile_commands entry"
        );
        assert_eq!(
            manager.semantic_compdb_kind(&header_path),
            SemanticCompdbContextKind::PairedSourceHeuristic
        );
        assert!(
            manager.has_semantic_compdb(&header_path),
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
    fn fidelity_parses_source() {
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

        let manager = ParserManager::with_clang_cfg(
            "clang".to_string(),
            Vec::new(),
            Some(compdb_path.clone()),
            ClangArgsMode::CompdbOnly,
            true,
            false,
        );
        assert!(
            !manager.has_exact_compdb(&unknown_source),
            "source should not have exact compile_commands entry"
        );
        assert!(
            manager.has_semantic_compdb(&unknown_source),
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
    fn fidelity_rejects_nocompdb() {
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

        let manager = ParserManager::with_clang_cfg(
            "clang".to_string(),
            Vec::new(),
            Some(compdb_path.clone()),
            ClangArgsMode::CompdbOnly,
            true,
            false,
        );
        assert!(
            !manager.has_exact_compdb(&unknown_source),
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
