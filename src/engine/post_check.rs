use std::collections::BTreeSet;
use std::path::Path;

use crate::engine::semantic_contract::{
    SemanticContract, SemanticContractSnapshot,
};
use crate::parser::clang_result::{
    ClangDiagnosticEntry, ClangDiagnosticSeverity, ClangDiagnosticSummary, ClangParseResult,
};
use crate::parser::manager::{ParserManager, SemanticCompdbContextKind};
use crate::parser::ts_traversal;
use tree_sitter::Tree;

mod baseline;
mod delta;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum PostEditFailureKind {
    ParserUnavailableTree,
    ParserUnavailableClang,
    TreeErrorRegressed,
    TreeErrorRatioRegressed,
    ClangDiagnosticsIncreased,
    SemanticReadinessRegressed,
    SemanticIdentityRegressed,
    SemanticReferenceIntegrityRegressed,
    SemanticScopeDriftRegressed,
}

#[derive(Clone, Debug)]
pub struct PostEditCheckResult {
    pub accepted: bool,
    pub messages: Vec<String>,
    pub failure_kinds: BTreeSet<PostEditFailureKind>,
    pub culprit_lines: BTreeSet<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct CheckBaseline {
    before_tree_error: Option<bool>,
    before_tree_error_ratio: Option<f64>,
    before_clang_error_count: Option<usize>,
    before_clang_fatal_count: Option<usize>,
    before_clang_summary: Option<ClangDiagnosticSummary>,
    before_clang_diagnostic_entries: Option<Vec<ClangDiagnosticEntry>>,
    before_clang_error_lines: Option<BTreeSet<usize>>,
    before_semantic_snapshot: Option<SemanticContractSnapshot>,
    before_semantic_ready: bool,
    semantic_readiness_note: Option<String>,
    before_tree_unavailable: bool,
    before_clang_unavailable: bool,
    warnings: Vec<String>,
}

pub struct PostEditChecker {
    parser_manager: ParserManager,
    fail_on_parser_unavailable: bool,
    tree_error_ratio_tolerance: f64,
    semantic_contract: SemanticContract,
}

impl PostEditChecker {
    pub fn new(
        parser_manager: ParserManager,
        fail_on_parser_unavailable: bool,
        tree_error_ratio_tolerance: f64,
        semantic_contract: SemanticContract,
    ) -> Self {
        Self {
            parser_manager,
            fail_on_parser_unavailable,
            tree_error_ratio_tolerance: tree_error_ratio_tolerance.clamp(0.0, 1.0),
            semantic_contract,
        }
    }

    pub fn validate_for_edits(
        &self,
        path: &Path,
        before_text: &str,
        after_text: &str,
        edited_lines: Option<&BTreeSet<usize>>,
        adaptive: &crate::engine::certainty_filter::CertaintyFilterState,
    ) -> PostEditCheckResult {
        if before_text == after_text {
            return PostEditCheckResult {
                accepted: true,
                messages: Vec::new(),
                failure_kinds: BTreeSet::new(),
                culprit_lines: BTreeSet::new(),
            };
        }

        let baseline = self.build_baseline(path, before_text);
        self.validate_with_baseline_for_edits(path, after_text, &baseline, edited_lines, adaptive)
    }

    pub fn build_baseline(&self, path: &Path, before_text: &str) -> CheckBaseline {
        baseline::build(self, path, before_text)
    }

    pub fn validate_with_baseline_for_edits(
        &self,
        path: &Path,
        after_text: &str,
        baseline: &CheckBaseline,
        edited_lines: Option<&BTreeSet<usize>>,
        adaptive: &crate::engine::certainty_filter::CertaintyFilterState,
    ) -> PostEditCheckResult {
        delta::validate(self, path, after_text, baseline, edited_lines, adaptive)
    }

    pub fn validate_structural_only(
        &self,
        path: &Path,
        after_text: &str,
        baseline: &CheckBaseline,
    ) -> PostEditCheckResult {
        delta::validate_structural_only(self, path, after_text, baseline)
    }

    pub(crate) fn semantic_context_kind_for_path(&self, path: &Path) -> SemanticCompdbContextKind {
        self.parser_manager
            .semantic_compdb_kind(path)
    }

    fn clang_error_count(parse: &ClangParseResult) -> usize {
        parse.error_diagnostic_count()
    }

    fn clang_fatal_count(parse: &ClangParseResult) -> usize {
        parse.diagnostic_summary().fatal
    }

    fn tree_error_ratio_and_lines(tree: &Tree) -> (f64, BTreeSet<usize>) {
        let stats = ts_traversal::tree_error_stats(tree);
        (stats.error_ratio(), stats.error_lines)
    }

    #[cfg(test)]
    fn diagnostic_weighted_score(summary: ClangDiagnosticSummary, adaptive: &crate::engine::certainty_filter::CertaintyFilterState) -> u32 {
        Self::diagnostic_weighted_score_impl(summary, adaptive)
    }

    fn diagnostic_weighted_score_impl(summary: ClangDiagnosticSummary, adaptive: &crate::engine::certainty_filter::CertaintyFilterState) -> u32 {
        let (note_w, warn_w, err_w, fatal_w) = adaptive.diagnostic_weights();
        (summary.note as u32).saturating_mul(note_w)
            .saturating_add((summary.warning as u32).saturating_mul(warn_w))
            .saturating_add((summary.error as u32).saturating_mul(err_w))
            .saturating_add((summary.fatal as u32).saturating_mul(fatal_w))
    }

    fn diagnostic_summary_label(summary: ClangDiagnosticSummary) -> String {
        format!(
            "fatal={} error={} warning={} note={}",
            summary.fatal, summary.error, summary.warning, summary.note
        )
    }

    fn diagnostic_delta_lines(
        before: &[ClangDiagnosticEntry],
        after: &[ClangDiagnosticEntry],
    ) -> BTreeSet<usize> {
        let mut before_counts = std::collections::HashMap::<(usize, usize, u8), usize>::new();
        for entry in before {
            let key = (
                entry.line,
                entry.column,
                Self::diagnostic_severity_bucket(entry.severity),
            );
            *before_counts.entry(key).or_insert(0) += 1;
        }
        let mut after_counts = std::collections::HashMap::<(usize, usize, u8), usize>::new();
        for entry in after {
            let key = (
                entry.line,
                entry.column,
                Self::diagnostic_severity_bucket(entry.severity),
            );
            *after_counts.entry(key).or_insert(0) += 1;
        }
        let mut lines = BTreeSet::<usize>::new();
        for ((line, column, severity), after_count) in after_counts {
            let before_count = before_counts
                .get(&(line, column, severity))
                .copied()
                .unwrap_or(0);
            if after_count > before_count && line > 0 {
                lines.insert(line);
            }
        }
        lines
    }

    fn diagnostic_severity_bucket(severity: ClangDiagnosticSeverity) -> u8 {
        match severity {
            ClangDiagnosticSeverity::Ignored => 0,
            ClangDiagnosticSeverity::Note => 1,
            ClangDiagnosticSeverity::Warning => 2,
            ClangDiagnosticSeverity::Error => 3,
            ClangDiagnosticSeverity::Fatal => 4,
        }
    }

    fn should_tolerate_consensus_nonlocal_diagnostic_delta(
        parser_manager: &ParserManager,
        path: &Path,
        edited_lines: Option<&BTreeSet<usize>>,
        delta_lines: &BTreeSet<usize>,
        severe_delta: usize,
        weighted_delta: u32,
    ) -> bool {
        let exact_compdb = parser_manager.has_exact_compdb(path);
        let context_kind = parser_manager.semantic_compdb_kind(path);
        Self::should_tolerate_nonlocal_diagnostic_delta_for_context(
            context_kind,
            exact_compdb,
            edited_lines,
            delta_lines,
            severe_delta,
            weighted_delta,
        )
    }

    fn should_tolerate_nonlocal_diagnostic_delta_for_context(
        context_kind: SemanticCompdbContextKind,
        exact_compdb: bool,
        edited_lines: Option<&BTreeSet<usize>>,
        delta_lines: &BTreeSet<usize>,
        severe_delta: usize,
        weighted_delta: u32,
    ) -> bool {
        let Some(edited_lines) = edited_lines else {
            return false;
        };
        if edited_lines.is_empty() || delta_lines.is_empty() {
            return false;
        }
        if exact_compdb {
            return false;
        }
        if !matches!(
            context_kind,
            SemanticCompdbContextKind::PairedSourceHeuristic
                | SemanticCompdbContextKind::HeaderConsensus
                | SemanticCompdbContextKind::SourceConsensus
        ) {
            return false;
        }
        if severe_delta > 1 {
            return false;
        }
        let (delta_limit, radius): (usize, usize) = match context_kind {
            SemanticCompdbContextKind::PairedSourceHeuristic => (0, 0),
            SemanticCompdbContextKind::HeaderConsensus => (24, 4),
            SemanticCompdbContextKind::SourceConsensus => (6, 4),
            SemanticCompdbContextKind::Exact | SemanticCompdbContextKind::None => {
                return false;
            }
        };
        if weighted_delta > delta_limit as u32 {
            return false;
        }
        delta_lines
            .iter()
            .all(|line| *line > 0 && !Self::line_near_edited_lines(*line, edited_lines, radius))
    }

    fn should_skip_nonexact_consensus_clang_validation(&self, path: &Path) -> bool {
        let exact_compdb = self.parser_manager.has_exact_compdb(path);
        let context_kind = self
            .parser_manager
            .semantic_compdb_kind(path);
        Self::should_skip_nonexact_consensus_clang_validation_for_context(
            context_kind,
            exact_compdb,
        )
    }

    fn should_skip_nonexact_consensus_clang_validation_for_context(
        context_kind: SemanticCompdbContextKind,
        exact_compdb: bool,
    ) -> bool {
        if exact_compdb {
            return false;
        }
        matches!(
            context_kind,
            SemanticCompdbContextKind::HeaderConsensus | SemanticCompdbContextKind::SourceConsensus
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn should_relax_consensus_diagnostic_failure(
        parser_manager: &ParserManager,
        path: &Path,
        edited_lines: Option<&BTreeSet<usize>>,
        failure_kinds: &BTreeSet<PostEditFailureKind>,
        delta_lines: &BTreeSet<usize>,
        severe_delta: usize,
        weighted_delta: u32,
    ) -> bool {
        if !failure_kinds.contains(&PostEditFailureKind::ClangDiagnosticsIncreased) {
            return false;
        }
        if failure_kinds.contains(&PostEditFailureKind::SemanticReadinessRegressed)
            || failure_kinds.contains(&PostEditFailureKind::SemanticIdentityRegressed)
            || failure_kinds.contains(&PostEditFailureKind::SemanticReferenceIntegrityRegressed)
            || failure_kinds.contains(&PostEditFailureKind::SemanticScopeDriftRegressed)
            || failure_kinds.contains(&PostEditFailureKind::TreeErrorRegressed)
            || failure_kinds.contains(&PostEditFailureKind::TreeErrorRatioRegressed)
        {
            return false;
        }
        let exact_compdb = parser_manager.has_exact_compdb(path);
        if exact_compdb {
            return false;
        }
        let context_kind = parser_manager.semantic_compdb_kind(path);
        let context_kind_index = match context_kind {
            SemanticCompdbContextKind::PairedSourceHeuristic => 0u8,
            SemanticCompdbContextKind::HeaderConsensus
            | SemanticCompdbContextKind::SourceConsensus => 1,
            SemanticCompdbContextKind::Exact | SemanticCompdbContextKind::None => return false,
        };
        let (severe_limit, weighted_limit): (usize, u32) = match context_kind_index {
            0 => (8, 48),
            1 => (1, 10),
            _ => (0, 0),
        };
        if severe_delta > severe_limit || weighted_delta > weighted_limit {
            return false;
        }
        if matches!(
            context_kind,
            SemanticCompdbContextKind::PairedSourceHeuristic
        ) {
            return edited_lines.is_some_and(|lines| !lines.is_empty());
        }
        Self::should_tolerate_nonlocal_diagnostic_delta_for_context(
            context_kind,
            exact_compdb,
            edited_lines,
            delta_lines,
            severe_delta,
            weighted_delta,
        )
    }

    fn semantic_transition_tolerances_for_context(
        context_kind: SemanticCompdbContextKind,
        _exact_compdb: bool,
        edited_lines: Option<&BTreeSet<usize>>,
    ) -> (usize, usize) {
        let base_reference_drop_tolerance: usize = match context_kind {
            SemanticCompdbContextKind::Exact => 2,
            _ => 4,
        };
        let base_scope_drift_tolerance: usize = match context_kind {
            SemanticCompdbContextKind::Exact => 1,
            _ => 3,
        };
        let context_extra_ref = match context_kind {
            SemanticCompdbContextKind::Exact => 0,
            SemanticCompdbContextKind::PairedSourceHeuristic => 24,
            SemanticCompdbContextKind::HeaderConsensus => 6,
            SemanticCompdbContextKind::SourceConsensus => 4,
            SemanticCompdbContextKind::None => 2,
        };
        let context_extra_scope = match context_kind {
            SemanticCompdbContextKind::Exact => 0,
            SemanticCompdbContextKind::PairedSourceHeuristic
            | SemanticCompdbContextKind::HeaderConsensus => 4,
            SemanticCompdbContextKind::SourceConsensus => 2,
            SemanticCompdbContextKind::None => 0,
        };
        let _ = edited_lines;
        (
            base_reference_drop_tolerance.saturating_add(context_extra_ref),
            base_scope_drift_tolerance.saturating_add(context_extra_scope),
        )
    }

    fn line_near_edited_lines(line: usize, edited_lines: &BTreeSet<usize>, radius: usize) -> bool {
        if line == 0 || edited_lines.is_empty() {
            return false;
        }
        let start = line.saturating_sub(radius);
        let end = line.saturating_add(radius);
        edited_lines.range(start..=end).next().is_some()
    }

    fn line_hint(lines: impl Iterator<Item = usize>, max: usize) -> String {
        let mut sample = lines.take(max).collect::<Vec<_>>();
        sample.sort_unstable();
        sample.dedup();
        if sample.is_empty() {
            return "none".to_string();
        }
        sample
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join(",")
    }

}

impl CheckBaseline {
    pub fn semantic_ready(&self) -> bool {
        self.before_semantic_ready
    }

    pub fn semantic_readiness_note(&self) -> Option<&str> {
        self.semantic_readiness_note.as_deref()
    }

    pub fn before_semantic_snapshot(&self) -> Option<&SemanticContractSnapshot> {
        self.before_semantic_snapshot.as_ref()
    }

    pub fn before_tree_error_ratio(&self) -> Option<f64> {
        self.before_tree_error_ratio
    }

    pub fn before_clang_summary(&self) -> Option<ClangDiagnosticSummary> {
        self.before_clang_summary
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    use crate::engine::semantic_contract::{
        ScopeStructure, SemanticContract, SemanticContractSnapshot, SemanticScopeCounts,
        SymbolIdentity,
    };
    use crate::parser::clang_result::{ClangDiagnosticEntry, ClangDiagnosticSeverity};
    use crate::parser::manager::ParserManager;
    use crate::parser::file_context::SemanticSummary;

    use crate::engine::certainty_filter::CertaintyFilterState;
    use super::{CheckBaseline, PostEditChecker, PostEditFailureKind};

    #[test]
    fn tree_error_collects() {
        let checker = PostEditChecker::new(
            ParserManager::new(),
            true,
            0.0,
            SemanticContract::new(),
        );
        let path = PathBuf::from("sample.cpp");
        let before = "int main() { return 0; }\n";
        let after = "int main( { return 0; }\n";

        let baseline = checker.build_baseline(&path, before);
        let adaptive = CertaintyFilterState::new();
        let result = checker.validate_with_baseline_for_edits(&path, after, &baseline, None, &adaptive);

        assert!(!result.accepted);
        assert!(result
            .messages
            .iter()
            .any(|item| item.contains("tree-sitter error ratio increased")));
        assert!(!result.culprit_lines.is_empty());
    }

    #[test]
    fn stable_keeps_accepted() {
        let checker = PostEditChecker::new(
            ParserManager::new(),
            true,
            0.0,
            SemanticContract::new(),
        );
        let path = PathBuf::from("sample.cpp");
        let before = "int main() { return 0; }\n";
        let after = "int main() { return 1; }\n";

        let baseline = checker.build_baseline(&path, before);
        let adaptive = CertaintyFilterState::new();
        let result = checker.validate_with_baseline_for_edits(&path, after, &baseline, None, &adaptive);

        assert!(result.accepted);
        assert!(result.culprit_lines.is_empty());
    }

    #[test]
    fn reference_regression_reported() {
        let checker = PostEditChecker::new(
            ParserManager::new(),
            true,
            0.0,
            SemanticContract::new(),
        );
        let path = PathBuf::from("semantic_ref.cpp");
        let mut reference_counts = BTreeMap::new();
        reference_counts.insert("usr:c:@F@value#".to_string(), 12usize);
        let mut reference_lines = BTreeMap::new();
        reference_lines.insert("usr:c:@F@value#".to_string(), 1usize);
        let baseline = CheckBaseline {
            before_tree_error: Some(false),
            before_tree_error_ratio: Some(0.0),
            before_clang_error_count: Some(0),
            before_clang_fatal_count: Some(0),
            before_clang_summary: Some(crate::parser::clang_result::ClangDiagnosticSummary {
                ignored: 0,
                note: 0,
                warning: 0,
                error: 0,
                fatal: 0,
            }),
            before_clang_diagnostic_entries: Some(Vec::new()),
            before_clang_error_lines: Some(BTreeSet::new()),
            before_semantic_snapshot: Some(SemanticContractSnapshot {
                summary: SemanticSummary {
                    usr_backed_declaration_count: 1,
                    ..SemanticSummary::default()
                },
                identity: SymbolIdentity {
                    usr_ref_counts: reference_counts,
                    usr_decl_lines: reference_lines,
                    ..SymbolIdentity::default()
                },
                ..SemanticContractSnapshot::default()
            }),
            before_semantic_ready: true,
            semantic_readiness_note: None,
            before_tree_unavailable: false,
            before_clang_unavailable: false,
            warnings: Vec::new(),
        };
        let after = "int main() { return 0; }\n";
        let adaptive = CertaintyFilterState::new();
        let result = checker.validate_with_baseline_for_edits(&path, after, &baseline, None, &adaptive);

        assert!(!result.accepted);
        assert!(result
            .failure_kinds
            .contains(&PostEditFailureKind::SemanticReferenceIntegrityRegressed));
    }

    #[test]
    fn scope_drift_reported() {
        let checker = PostEditChecker::new(
            ParserManager::new(),
            true,
            0.0,
            SemanticContract::new(),
        );
        let path = PathBuf::from("semantic_scope.cpp");
        let before = "namespace A {\nnamespace B {\nnamespace C {\nnamespace D {\nint value = 0;\n}\n}\n}\n}\n";
        let after = "int value = 0;\n";

        let baseline = checker.build_baseline(&path, before);
        let adaptive = CertaintyFilterState::new();
        let result = checker.validate_with_baseline_for_edits(&path, after, &baseline, None, &adaptive);

        assert!(!result.accepted);
        assert!(result
            .failure_kinds
            .contains(&PostEditFailureKind::SemanticScopeDriftRegressed));
    }

    #[test]
    fn skips_unready_baseline() {
        let checker = PostEditChecker::new(
            ParserManager::new(),
            true,
            0.0,
            SemanticContract::new(),
        );
        let path = PathBuf::from("semantic_skip.cpp");
        let mut reference_counts = BTreeMap::new();
        reference_counts.insert("usr:c:@F@foo#".to_string(), 3usize);
        let mut reference_lines = BTreeMap::new();
        reference_lines.insert("usr:c:@F@foo#".to_string(), 1usize);
        let baseline = CheckBaseline {
            before_semantic_snapshot: Some(SemanticContractSnapshot {
                summary: SemanticSummary {
                    usr_backed_declaration_count: 1,
                    ..SemanticSummary::default()
                },
                identity: SymbolIdentity {
                    usr_ref_counts: reference_counts,
                    usr_decl_lines: reference_lines,
                    ..SymbolIdentity::default()
                },
                scopes: ScopeStructure {
                    counts: SemanticScopeCounts {
                        function: 1,
                        ..SemanticScopeCounts::default()
                    },
                    ..ScopeStructure::default()
                },
                ..SemanticContractSnapshot::default()
            }),
            before_semantic_ready: false,
            semantic_readiness_note: Some("clang fatals 1 exceed 0".to_string()),
            ..CheckBaseline::default()
        };

        let adaptive = CertaintyFilterState::new();
        let result = checker.validate_with_baseline_for_edits(
            &path,
            "int main() { return 0; }\n",
            &baseline,
            None,
            &adaptive,
        );
        assert!(!result
            .failure_kinds
            .contains(&PostEditFailureKind::SemanticReferenceIntegrityRegressed));
        assert!(result
            .messages
            .iter()
            .any(|item| item.contains("semantic integrity checks skipped")));
    }

    #[test]
    fn readiness_regression_reported() {
        let checker = PostEditChecker::new(
            ParserManager::new(),
            true,
            0.0,
            SemanticContract::new(),
        );
        let path = PathBuf::from("semantic_ready.cpp");
        let before = "int main() { return 0; }\n";
        let after = "#include \"missing.hpp\"\nint main() { return 0; }\n";

        let baseline = checker.build_baseline(&path, before);
        assert!(baseline.semantic_ready());
        let adaptive = CertaintyFilterState::new();
        let result = checker.validate_with_baseline_for_edits(&path, after, &baseline, None, &adaptive);

        // Binary readiness: parsers available = ready. Adding an include
        // does not make parsers unavailable, so no readiness regression.
        assert!(!result
            .failure_kinds
            .contains(&PostEditFailureKind::SemanticReadinessRegressed));
    }

    #[test]
    fn weighting_prioritizes_severity() {
        let adaptive = CertaintyFilterState::new();
        let empty = PostEditChecker::diagnostic_weighted_score(
            crate::parser::clang_result::ClangDiagnosticSummary::default(),
            &adaptive,
        );
        let warning = PostEditChecker::diagnostic_weighted_score(
            crate::parser::clang_result::ClangDiagnosticSummary {
                warning: 1,
                ..crate::parser::clang_result::ClangDiagnosticSummary::default()
            },
            &adaptive,
        );
        let error = PostEditChecker::diagnostic_weighted_score(
            crate::parser::clang_result::ClangDiagnosticSummary {
                error: 1,
                ..crate::parser::clang_result::ClangDiagnosticSummary::default()
            },
            &adaptive,
        );
        let fatal = PostEditChecker::diagnostic_weighted_score(
            crate::parser::clang_result::ClangDiagnosticSummary {
                fatal: 1,
                ..crate::parser::clang_result::ClangDiagnosticSummary::default()
            },
            &adaptive,
        );
        assert!(warning > empty);
        assert!(error > warning);
        assert!(fatal > error);
    }

    #[test]
    fn delta_reports_new() {
        let before = vec![ClangDiagnosticEntry {
            line: 4,
            column: 1,
            severity: ClangDiagnosticSeverity::Warning,
        }];
        let after = vec![
            ClangDiagnosticEntry {
                line: 4,
                column: 1,
                severity: ClangDiagnosticSeverity::Warning,
            },
            ClangDiagnosticEntry {
                line: 8,
                column: 1,
                severity: ClangDiagnosticSeverity::Error,
            },
        ];
        let delta = PostEditChecker::diagnostic_delta_lines(before.as_slice(), after.as_slice());
        assert!(delta.contains(&8));
        assert!(!delta.contains(&4));
    }

    #[test]
    fn nonlocal_delta_tolerated() {
        let edited = BTreeSet::from([20usize, 21usize, 22usize]);
        let deltas = BTreeSet::from([320usize, 321usize]);
        assert!(
            PostEditChecker::should_tolerate_nonlocal_diagnostic_delta_for_context(
                crate::parser::manager::SemanticCompdbContextKind::HeaderConsensus,
                false,
                Some(&edited),
                &deltas,
                1,
                8,
            )
        );
    }

    #[test]
    fn exact_delta_rejected() {
        let edited = BTreeSet::from([20usize, 21usize, 22usize]);
        let deltas = BTreeSet::from([320usize, 321usize]);
        assert!(
            !PostEditChecker::should_tolerate_nonlocal_diagnostic_delta_for_context(
                crate::parser::manager::SemanticCompdbContextKind::SourceConsensus,
                true,
                Some(&edited),
                &deltas,
                1,
                8,
            )
        );
    }

    #[test]
    fn nonexact_skips_validation() {
        assert!(
            PostEditChecker::should_skip_nonexact_consensus_clang_validation_for_context(
                crate::parser::manager::SemanticCompdbContextKind::HeaderConsensus,
                false,
            )
        );
        assert!(
            PostEditChecker::should_skip_nonexact_consensus_clang_validation_for_context(
                crate::parser::manager::SemanticCompdbContextKind::SourceConsensus,
                false,
            )
        );
        assert!(
            !PostEditChecker::should_skip_nonexact_consensus_clang_validation_for_context(
                crate::parser::manager::SemanticCompdbContextKind::PairedSourceHeuristic,
                false,
            )
        );
        assert!(
            !PostEditChecker::should_skip_nonexact_consensus_clang_validation_for_context(
                crate::parser::manager::SemanticCompdbContextKind::Exact,
                true,
            )
        );
        assert!(
            !PostEditChecker::should_skip_nonexact_consensus_clang_validation_for_context(
                crate::parser::manager::SemanticCompdbContextKind::None,
                false,
            )
        );
    }

    #[test]
    fn tolerances_fidelity_order() {
        let lines = BTreeSet::from([1usize, 2, 3, 4, 5, 6, 7, 8]);
        let exact = PostEditChecker::semantic_transition_tolerances_for_context(
            crate::parser::manager::SemanticCompdbContextKind::Exact,
            false, Some(&lines),
        );
        let paired = PostEditChecker::semantic_transition_tolerances_for_context(
            crate::parser::manager::SemanticCompdbContextKind::PairedSourceHeuristic,
            false, Some(&lines),
        );
        let source = PostEditChecker::semantic_transition_tolerances_for_context(
            crate::parser::manager::SemanticCompdbContextKind::SourceConsensus,
            false, Some(&lines),
        );
        let header = PostEditChecker::semantic_transition_tolerances_for_context(
            crate::parser::manager::SemanticCompdbContextKind::HeaderConsensus,
            false, Some(&lines),
        );

        assert!(paired.0 > exact.0 && paired.1 > exact.1,
            "paired ({},{}) should exceed exact ({},{})", paired.0, paired.1, exact.0, exact.1);
        assert!(paired.0 > source.0,
            "paired ref ({}) should exceed source ref ({})", paired.0, source.0);
        assert!(header.0 >= source.0,
            "header ref ({}) should be >= source ref ({})", header.0, source.0);
    }
}
