use rustc_hash::FxHashMap;

use crate::parser::clang_types::ClangDeclKey;
use crate::parser::clang_result::{
    ClangDiagnosticEntry, ClangDiagnosticSeverity, ClangDiagnosticSummary, ClangParseResult,
};
use crate::parser::clang_types::ClangSymbolKey;
use crate::parser::file_context::SemanticDeclaration;

pub(crate) struct ParserConsensusSelector;

impl ParserConsensusSelector {
    pub(crate) fn should_replace_source_parse(
        current: Option<&ClangParseResult>,
        candidate: &ClangParseResult,
    ) -> bool {
        let Some(current) = current else {
            return true;
        };
        let candidate_key = (
            !candidate.success,
            Self::diagnostic_weight(candidate.diagnostic_summary()),
            usize::MAX.saturating_sub(candidate.symbols.len()),
        );
        let current_key = (
            !current.success,
            Self::diagnostic_weight(current.diagnostic_summary()),
            usize::MAX.saturating_sub(current.symbols.len()),
        );
        candidate_key < current_key
    }

    #[allow(clippy::type_complexity)]
    pub(crate) fn merge_header_results(
        parses: Vec<ClangParseResult>,
        _failures: Vec<String>,
    ) -> ClangParseResult {
        let parse_count = parses.len();
        let successful_parse_count = parses.iter().filter(|parse| parse.success).count();
        let semantic_vote_parse_count = parses
            .iter()
            .filter(|parse| Self::header_semantic_vote_eligible(parse))
            .count();
        let symbol_threshold = Self::strict_majority_threshold(semantic_vote_parse_count);
        let diagnostic_threshold = Self::strict_majority_threshold(parse_count);

        let mut symbol_votes: FxHashMap<(String, i32, usize, usize, Option<String>, Option<String>), usize> = FxHashMap::default();
        let mut symbol_order: Vec<(String, i32, usize, usize, Option<String>, Option<String>)> = Vec::new();
        let mut symbol_exemplar: FxHashMap<(String, i32, usize, usize, Option<String>, Option<String>), SemanticDeclaration> = FxHashMap::default();
        let mut rename_votes: FxHashMap<(ClangSymbolKey, usize), usize> = FxHashMap::default();
        let mut ref_votes: FxHashMap<(ClangDeclKey, usize), usize> = FxHashMap::default();
        let mut diag_votes: FxHashMap<(usize, usize, ClangDiagnosticSeverity), usize> = FxHashMap::default();
        let mut diag_messages: FxHashMap<(usize, usize, ClangDiagnosticSeverity), String> = FxHashMap::default();

        for parse in &parses {
            if Self::header_semantic_vote_eligible(parse) {
                for symbol in &parse.symbols {
                    let key = (
                        symbol.name.clone(),
                        symbol.kind,
                        symbol.line,
                        symbol.column,
                        symbol.usr.clone(),
                        symbol.scope_usr.clone(),
                    );
                    let count = symbol_votes.entry(key.clone()).or_insert(0usize);
                    *count += 1;
                    if *count == 1 {
                        symbol_order.push(key.clone());
                    }
                    symbol_exemplar.entry(key).or_insert_with(|| symbol.clone());
                }
                for (key, offsets) in parse.rename_offsets_map() {
                    for offset in offsets {
                        *rename_votes.entry((key.clone(), *offset)).or_insert(0usize) += 1;
                    }
                }
                for (key, offsets) in parse.reference_offsets_map() {
                    for offset in offsets {
                        *ref_votes.entry((key.clone(), *offset)).or_insert(0usize) += 1;
                    }
                }
            }
            for (index, entry) in parse.diagnostic_entries().iter().enumerate() {
                let key = (entry.line, entry.column, entry.severity);
                *diag_votes.entry(key).or_insert(0usize) += 1;
                if let Some(message) = parse.diagnostics.get(index) {
                    diag_messages.entry(key).or_insert_with(|| message.clone());
                }
            }
        }

        let mut symbols = Vec::<SemanticDeclaration>::new();
        for key in symbol_order {
            if symbol_votes.get(&key).copied().unwrap_or(0) >= symbol_threshold {
                if let Some(symbol) = symbol_exemplar.get(&key) {
                    symbols.push(symbol.clone());
                }
            }
        }
        symbols.sort_by(|left, right| {
            left.line
                .cmp(&right.line)
                .then(left.column.cmp(&right.column))
                .then(left.name.cmp(&right.name))
        });

        let mut rename_offsets_by_symbol = FxHashMap::<ClangSymbolKey, Vec<usize>>::default();
        for ((key, offset), votes) in rename_votes {
            if votes < symbol_threshold {
                continue;
            }
            rename_offsets_by_symbol
                .entry(key)
                .or_default()
                .push(offset);
        }
        for offsets in rename_offsets_by_symbol.values_mut() {
            offsets.sort_unstable();
            offsets.dedup();
        }

        let mut reference_offsets_by_decl = FxHashMap::<ClangDeclKey, Vec<usize>>::default();
        for ((key, offset), votes) in ref_votes {
            if votes < symbol_threshold {
                continue;
            }
            reference_offsets_by_decl
                .entry(key)
                .or_default()
                .push(offset);
        }
        for offsets in reference_offsets_by_decl.values_mut() {
            offsets.sort_unstable();
            offsets.dedup();
        }

        let mut diagnostic_entries = diag_votes
            .into_iter()
            .filter_map(|((line, column, severity), votes)| {
                (votes >= diagnostic_threshold).then_some(ClangDiagnosticEntry {
                    line,
                    column,
                    severity,
                    warning_option: String::new(),
                    fix_its: Vec::new(),
                })
            })
            .collect::<Vec<_>>();
        diagnostic_entries.sort_by(|left, right| {
            left.line
                .cmp(&right.line)
                .then(left.column.cmp(&right.column))
                .then_with(|| (left.severity as u8).cmp(&(right.severity as u8)))
        });
        let mut diagnostics = Vec::<String>::with_capacity(diagnostic_entries.len());
        let mut diagnostic_summary = ClangDiagnosticSummary::default();
        for entry in &diagnostic_entries {
            let key = (entry.line, entry.column, entry.severity);
            if let Some(message) = diag_messages.get(&key) {
                diagnostics.push(format!("header-consensus: {message}"));
            } else {
                diagnostics.push(format!(
                    "header-consensus:{}:{}:{:?}",
                    entry.line, entry.column, entry.severity
                ));
            }
            match entry.severity {
                ClangDiagnosticSeverity::Ignored => {
                    diagnostic_summary.ignored = diagnostic_summary.ignored.saturating_add(1)
                }
                ClangDiagnosticSeverity::Note => {
                    diagnostic_summary.note = diagnostic_summary.note.saturating_add(1)
                }
                ClangDiagnosticSeverity::Warning => {
                    diagnostic_summary.warning = diagnostic_summary.warning.saturating_add(1)
                }
                ClangDiagnosticSeverity::Error => {
                    diagnostic_summary.error = diagnostic_summary.error.saturating_add(1)
                }
                ClangDiagnosticSeverity::Fatal => {
                    diagnostic_summary.fatal = diagnostic_summary.fatal.saturating_add(1)
                }
            }
        }

        let success = successful_parse_count >= Self::strict_majority_threshold(parse_count)
            && !parses.is_empty();
        ClangParseResult::with_semantic_offsets(
            success,
            diagnostics,
            symbols,
            rename_offsets_by_symbol,
            reference_offsets_by_decl,
            diagnostic_summary,
            diagnostic_entries,
        )
    }

    fn diagnostic_weight(summary: ClangDiagnosticSummary) -> u64 {
        summary
            .fatal
            .saturating_mul(1_000)
            .saturating_add(summary.error.saturating_mul(100))
            .saturating_add(summary.warning.saturating_mul(10))
            .saturating_add(summary.note) as u64
    }

    fn strict_majority_threshold(voter_count: usize) -> usize {
        if voter_count == 0 {
            return 1;
        }
        voter_count / 2 + 1
    }

    fn header_semantic_vote_eligible(parse: &ClangParseResult) -> bool {
        if parse.symbols.is_empty() {
            return false;
        }
        if parse.success {
            return true;
        }
        let summary = parse.diagnostic_summary();
        summary.fatal <= 1 && summary.error_total() <= 6
    }
}
