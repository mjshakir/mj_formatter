use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::parser::clang_types::ClangDeclKey;
use crate::parser::clang_symbol::ClangSymbol;
use crate::parser::clang_types::ClangSymbolKey;
use crate::parser::clang_types::ClangSymbolKind;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ClangParseResult {
    pub success: bool,
    pub diagnostics: Vec<String>,
    pub symbols: Vec<ClangSymbol>,
    diagnostic_summary: ClangDiagnosticSummary,
    diagnostic_entries: Vec<ClangDiagnosticEntry>,
    symbols_by_line: HashMap<usize, Vec<usize>>,
    rename_offsets_by_symbol: HashMap<ClangSymbolKey, Vec<usize>>,
    reference_offsets_by_decl: HashMap<ClangDeclKey, Vec<usize>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum ClangDiagnosticSeverity {
    Ignored,
    Note,
    Warning,
    Error,
    Fatal,
}

impl ClangDiagnosticSeverity {
    pub fn is_error(self) -> bool {
        matches!(self, Self::Error | Self::Fatal)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClangDiagnosticEntry {
    pub line: usize,
    pub column: usize,
    pub severity: ClangDiagnosticSeverity,
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
        symbols: Vec<ClangSymbol>,
        diagnostic_summary: ClangDiagnosticSummary,
        diagnostic_entries: Vec<ClangDiagnosticEntry>,
    ) -> Self {
        Self::with_semantic_offsets(
            success,
            diagnostics,
            symbols,
            HashMap::new(),
            HashMap::new(),
            diagnostic_summary,
            diagnostic_entries,
        )
    }

    #[cfg(test)]
    pub fn with_rename_offsets(
        success: bool,
        diagnostics: Vec<String>,
        symbols: Vec<ClangSymbol>,
        rename_offsets_by_symbol: HashMap<ClangSymbolKey, Vec<usize>>,
        diagnostic_summary: ClangDiagnosticSummary,
        diagnostic_entries: Vec<ClangDiagnosticEntry>,
    ) -> Self {
        Self::with_semantic_offsets(
            success,
            diagnostics,
            symbols,
            rename_offsets_by_symbol,
            HashMap::new(),
            diagnostic_summary,
            diagnostic_entries,
        )
    }

    pub fn with_semantic_offsets(
        success: bool,
        diagnostics: Vec<String>,
        symbols: Vec<ClangSymbol>,
        rename_offsets_by_symbol: HashMap<ClangSymbolKey, Vec<usize>>,
        reference_offsets_by_decl: HashMap<ClangDeclKey, Vec<usize>>,
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
        let mut symbols_by_line: HashMap<usize, Vec<usize>> = HashMap::new();
        for (index, symbol) in symbols.iter().enumerate() {
            symbols_by_line.entry(symbol.line).or_default().push(index);
        }

        Self {
            success,
            diagnostics,
            symbols,
            diagnostic_summary,
            diagnostic_entries,
            symbols_by_line,
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
            .filter(|entry| entry.severity.is_error())
            .filter_map(|entry| (entry.line > 0).then_some(entry.line))
            .collect()
    }

    pub fn symbol_on_line(
        &self,
        name: &str,
        line: usize,
        allowed_kinds: &[ClangSymbolKind],
    ) -> Option<&ClangSymbol> {
        let symbol_indexes = self.symbols_by_line.get(&line)?;
        symbol_indexes
            .iter()
            .filter_map(|index| self.symbols.get(*index))
            .find(|symbol| {
                symbol.name == name
                    && (allowed_kinds.is_empty() || allowed_kinds.contains(&symbol.kind))
            })
    }

    pub fn rename_offsets_on_line(
        &self,
        name: &str,
        line: usize,
        allowed_kinds: &[ClangSymbolKind],
    ) -> Vec<usize> {
        let mut offsets = Vec::new();
        let Some(symbol_indexes) = self.symbols_by_line.get(&line) else {
            return offsets;
        };

        for index in symbol_indexes {
            let Some(symbol) = self.symbols.get(*index) else {
                continue;
            };
            if symbol.name != name
                || (!allowed_kinds.is_empty() && !allowed_kinds.contains(&symbol.kind))
            {
                continue;
            }

            let key = ClangSymbolKey::new(symbol.name.clone(), symbol.kind, symbol.line);
            if let Some(locations) = self.rename_offsets_by_symbol.get(&key) {
                offsets.extend(locations.iter().copied());
            }
        }

        offsets.sort_unstable();
        offsets.dedup();
        offsets
    }

    pub fn has_symbol_name_elsewhere(&self, name: &str, except_line: usize) -> bool {
        self.symbols
            .iter()
            .any(|symbol| symbol.name == name && symbol.line != except_line)
    }

    pub fn reference_offsets_for_decl(&self, decl_key: &ClangDeclKey) -> Vec<usize> {
        self.reference_offsets_by_decl
            .get(decl_key)
            .cloned()
            .unwrap_or_default()
    }

    pub fn reference_offsets_map(&self) -> &HashMap<ClangDeclKey, Vec<usize>> {
        &self.reference_offsets_by_decl
    }

    pub fn rename_offsets_map(&self) -> &HashMap<ClangSymbolKey, Vec<usize>> {
        &self.rename_offsets_by_symbol
    }
}
