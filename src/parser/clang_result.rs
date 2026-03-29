use std::collections::BTreeSet;

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use crate::parser::clang_types::ClangDeclKey;
use crate::parser::clang_types::ClangSymbolKey;
use crate::parser::file_context::SemanticDeclaration;

#[derive(Clone, Debug)]
pub struct ClangParseResult {
    pub success: bool,
    pub diagnostics: Vec<String>,
    pub(crate) symbols: Vec<SemanticDeclaration>,
    diagnostic_summary: ClangDiagnosticSummary,
    diagnostic_entries: Vec<ClangDiagnosticEntry>,
    rename_offsets_by_symbol: FxHashMap<ClangSymbolKey, Vec<usize>>,
    reference_offsets_by_decl: FxHashMap<ClangDeclKey, Vec<usize>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClangFixIt {
    pub replacement: String,
    pub start_line: usize,
    pub start_column: usize,
    pub end_line: usize,
    pub end_column: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClangDiagnosticEntry {
    pub line: usize,
    pub column: usize,
    pub severity: u32,
    pub warning_option: String,
    pub fix_its: Vec<ClangFixIt>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClangDiagnosticSummary {
    pub ignored: usize,
    pub note: usize,
    pub warning: usize,
    pub error: usize,
    pub fatal: usize,
}

impl ClangDiagnosticSummary {
    pub fn total(self) -> usize {
        self.ignored
            .saturating_add(self.note)
            .saturating_add(self.warning)
            .saturating_add(self.error)
            .saturating_add(self.fatal)
    }

    pub fn error_total(self) -> usize {
        self.error.saturating_add(self.fatal)
    }
}

impl Default for ClangParseResult {
    fn default() -> Self {
        Self::new(
            false,
            Vec::new(),
            Vec::new(),
            ClangDiagnosticSummary::default(),
            Vec::new(),
        )
    }
}

impl ClangParseResult {
    pub fn new(
        success: bool,
        diagnostics: Vec<String>,
        symbols: Vec<SemanticDeclaration>,
        diagnostic_summary: ClangDiagnosticSummary,
        diagnostic_entries: Vec<ClangDiagnosticEntry>,
    ) -> Self {
        Self::with_semantic_offsets(
            success,
            diagnostics,
            symbols,
            FxHashMap::default(),
            FxHashMap::default(),
            diagnostic_summary,
            diagnostic_entries,
        )
    }

    #[cfg(test)]
    pub fn with_rename_offsets(
        success: bool,
        diagnostics: Vec<String>,
        symbols: Vec<SemanticDeclaration>,
        rename_offsets_by_symbol: FxHashMap<ClangSymbolKey, Vec<usize>>,
        diagnostic_summary: ClangDiagnosticSummary,
        diagnostic_entries: Vec<ClangDiagnosticEntry>,
    ) -> Self {
        Self::with_semantic_offsets(
            success,
            diagnostics,
            symbols,
            rename_offsets_by_symbol,
            FxHashMap::default(),
            diagnostic_summary,
            diagnostic_entries,
        )
    }

    pub fn with_semantic_offsets(
        success: bool,
        diagnostics: Vec<String>,
        symbols: Vec<SemanticDeclaration>,
        rename_offsets_by_symbol: FxHashMap<ClangSymbolKey, Vec<usize>>,
        reference_offsets_by_decl: FxHashMap<ClangDeclKey, Vec<usize>>,
        diagnostic_summary: ClangDiagnosticSummary,
        diagnostic_entries: Vec<ClangDiagnosticEntry>,
    ) -> Self {
        assert_eq!(
            diagnostic_summary.total(),
            diagnostics.len(),
            "clang diagnostic summary count must match diagnostic payload size"
        );
        assert_eq!(
            diagnostic_entries.len(),
            diagnostics.len(),
            "clang diagnostic entries count must match diagnostic payload size"
        );
        Self {
            success,
            diagnostics,
            symbols,
            diagnostic_summary,
            diagnostic_entries,
            rename_offsets_by_symbol,
            reference_offsets_by_decl,
        }
    }

    #[cfg(test)]
    pub fn diagnostic_total(&self) -> usize {
        self.diagnostic_summary.total()
    }

    pub fn diagnostic_summary(&self) -> ClangDiagnosticSummary {
        self.diagnostic_summary
    }

    pub fn diagnostic_entries(&self) -> &[ClangDiagnosticEntry] {
        self.diagnostic_entries.as_slice()
    }

    pub fn error_diagnostic_count(&self) -> usize {
        self.diagnostic_summary.error_total()
    }

    pub fn error_diagnostic_lines(&self) -> BTreeSet<usize> {
        self.diagnostic_entries
            .iter()
            .filter(|entry| entry.severity == clang_sys::CXDiagnostic_Error as u32 || entry.severity == clang_sys::CXDiagnostic_Fatal as u32)
            .filter_map(|entry| (entry.line > 0).then_some(entry.line))
            .collect()
    }

    pub(crate) fn ref_offsets(&self, decl_key: &ClangDeclKey) -> Vec<usize> {
        self.reference_offsets_by_decl
            .get(decl_key)
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn reference_offsets_map(&self) -> &FxHashMap<ClangDeclKey, Vec<usize>> {
        &self.reference_offsets_by_decl
    }

    pub(crate) fn rename_offsets_map(&self) -> &FxHashMap<ClangSymbolKey, Vec<usize>> {
        &self.rename_offsets_by_symbol
    }
}
