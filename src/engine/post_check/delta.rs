use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use tree_sitter::Tree;

use crate::engine::semantic_contract::SemanticReadinessInput;
use crate::parser::clang_result::{ClangDiagnosticSeverity, ClangParseResult};
use crate::parser::file_context::SemanticFileContext;

use super::{CheckBaseline, PostEditCheckResult, PostEditChecker, PostEditFailureKind};

pub(super) fn validate(
    checker: &PostEditChecker,
    path: &Path,
    after_text: &str,
    baseline: &CheckBaseline,
    edited_lines: Option<&BTreeSet<usize>>,
) -> PostEditCheckResult {
    let mut messages = baseline.warnings.clone();
    let mut failure_kinds = BTreeSet::<PostEditFailureKind>::new();
    let mut culprit_lines = BTreeSet::new();
    let mut after_tree = None::<Tree>;
    let mut after_clang = None::<Arc<ClangParseResult>>;
    let mut after_tree_unavailable = false;
    let mut after_clang_unavailable = false;
    let mut clang_delta_lines = BTreeSet::<usize>::new();
    let mut clang_weighted_delta = 0u32;
    let mut clang_severe_delta = 0usize;
    if baseline.before_tree_unavailable {
        failure_kinds.insert(PostEditFailureKind::ParserUnavailableTree);
    }
    if baseline.before_clang_unavailable {
        failure_kinds.insert(PostEditFailureKind::ParserUnavailableClang);
    }

    let run_clang_validation = !checker.should_skip_nonexact_consensus_clang_validation(path);
    let tree_result = checker.parser_manager.parse_tree_sitter(after_text, path);
    let clang_result = if run_clang_validation {
        Some(checker.parser_manager.parse_clang(after_text, path))
    } else {
        None
    };

    match tree_result {
        Ok(tree) => {
            let (after_ratio, tree_error_lines) =
                PostEditChecker::tree_error_ratio_and_lines(&tree);
            if matches!(baseline.before_tree_error, Some(false)) && tree.root_node().has_error() {
                failure_kinds.insert(PostEditFailureKind::TreeErrorRegressed);
                messages.push(
                    "post-edit check failed: tree-sitter parse quality regressed".to_string(),
                );
                culprit_lines.extend(tree_error_lines.iter().copied());
            }
            if let Some(before_ratio) = baseline.before_tree_error_ratio {
                let adaptive_tolerance = checker.tree_error_ratio_tolerance;
                let allowed = (before_ratio + adaptive_tolerance).clamp(0.0, 1.0);
                if after_ratio > allowed {
                    failure_kinds.insert(PostEditFailureKind::TreeErrorRatioRegressed);
                    messages.push(format!(
                        "post-edit check failed: tree-sitter error ratio increased ({before_ratio:.4} -> {after_ratio:.4})"
                    ));
                    culprit_lines.extend(tree_error_lines.iter().copied());
                }
            }
            after_tree = Some(tree);
        }
        Err(err) => {
            after_tree_unavailable = true;
            if checker.fail_on_parser_unavailable {
                failure_kinds.insert(PostEditFailureKind::ParserUnavailableTree);
                messages.push(format!(
                    "post-edit check failed: tree-sitter parser unavailable after edit: {err}"
                ));
            } else {
                messages.push(format!("post-edit check warning (after tree): {err}"));
            }
        }
    }

    if let Some(clang_result) = clang_result {
        match clang_result {
            Ok(parse) => {
                let after_count = PostEditChecker::clang_error_count(&parse);
                let after_summary = parse.diagnostic_summary();
                if let Some(before_summary) = baseline.before_clang_summary {
                    let before_weight = PostEditChecker::diagnostic_weighted_score_impl(before_summary);
                    let after_weight = PostEditChecker::diagnostic_weighted_score_impl(after_summary);
                    let weighted_delta = after_weight.saturating_sub(before_weight);
                    let severe_delta = after_summary
                        .error_total()
                        .saturating_sub(before_summary.error_total());
                    clang_weighted_delta = weighted_delta;
                    clang_severe_delta = severe_delta;
                    if weighted_delta > 0 || severe_delta > 0 {
                        let mut delta_lines = if let Some(before_entries) =
                            baseline.before_clang_diagnostic_entries.as_ref()
                        {
                            PostEditChecker::diagnostic_delta_lines(
                                before_entries.as_slice(),
                                parse.diagnostic_entries(),
                            )
                        } else {
                            BTreeSet::new()
                        };
                        if delta_lines.is_empty() {
                            let after_lines = parse.error_diagnostic_lines();
                            if let Some(before_lines) = baseline.before_clang_error_lines.as_ref() {
                                delta_lines.extend(after_lines.difference(before_lines).copied());
                            } else {
                                delta_lines.extend(after_lines.iter().copied());
                            }
                        }
                        if delta_lines.is_empty() {
                            for entry in parse.diagnostic_entries() {
                                if matches!(
                                    entry.severity,
                                    ClangDiagnosticSeverity::Warning
                                        | ClangDiagnosticSeverity::Error
                                        | ClangDiagnosticSeverity::Fatal
                                ) && entry.line > 0
                                {
                                    delta_lines.insert(entry.line);
                                }
                            }
                        }
                        clang_delta_lines = delta_lines.clone();
                        if PostEditChecker::should_tolerate_consensus_nonlocal_diagnostic_delta(
                            &checker.parser_manager,
                            path,
                            edited_lines,
                            &delta_lines,
                            severe_delta,
                            weighted_delta,
                        ) {
                            messages.push(format!(
                                "post-edit check warning: tolerated non-local clang diagnostic delta under consensus semantic context (weight {} -> {}, summary {} -> {}, lines={})",
                                before_weight,
                                after_weight,
                                PostEditChecker::diagnostic_summary_label(before_summary),
                                PostEditChecker::diagnostic_summary_label(after_summary),
                                PostEditChecker::line_hint(delta_lines.iter().copied(), 6)
                            ));
                        } else {
                            failure_kinds.insert(PostEditFailureKind::ClangDiagnosticsIncreased);
                            messages.push(format!(
                                "post-edit check failed: clang diagnostic weight increased ({} -> {}, summary {} -> {})",
                                before_weight,
                                after_weight,
                                PostEditChecker::diagnostic_summary_label(before_summary),
                                PostEditChecker::diagnostic_summary_label(after_summary)
                            ));
                            culprit_lines.extend(delta_lines);
                        }
                    }
                } else if let Some(before_count) = baseline.before_clang_error_count {
                    if after_count > before_count {
                        failure_kinds.insert(PostEditFailureKind::ClangDiagnosticsIncreased);
                        messages.push(format!(
                            "post-edit check failed: clang diagnostics increased ({} -> {})",
                            before_count, after_count
                        ));
                        culprit_lines.extend(parse.error_diagnostic_lines().iter().copied());
                    }
                }
                after_clang = Some(parse);
            }
            Err(err) => {
                after_clang_unavailable = true;
                if checker.fail_on_parser_unavailable {
                    failure_kinds.insert(PostEditFailureKind::ParserUnavailableClang);
                    messages.push(format!(
                        "post-edit check failed: clang parser unavailable after edit: {err}"
                    ));
                } else {
                    messages.push(format!("post-edit check warning (after clang): {err}"));
                }
            }
        }
    }

    let after_semantic = SemanticFileContext::from_parses(
        after_text,
        path,
        after_tree.as_ref(),
        after_clang.as_deref(),
    );
    let after_snapshot = checker.semantic_contract.snapshot(&after_semantic);
    let after_context_assessment = checker.semantic_contract.evaluate_context(&after_snapshot);
    messages.extend(after_context_assessment.warnings);
    let after_readiness = checker.semantic_contract.evaluate_readiness_with_snapshot(
        SemanticReadinessInput {
            tree_unavailable: after_tree_unavailable,
            clang_unavailable: after_clang_unavailable,
        },
        Some(&after_snapshot),
    );
    if baseline.before_semantic_ready {
        if !after_readiness.ready {
            failure_kinds.insert(PostEditFailureKind::SemanticReadinessRegressed);
            messages.push(format!(
                "post-edit check failed: semantic readiness regressed after edit ({})",
                after_readiness.summary()
            ));
        } else if let Some(before_semantic) = baseline.before_semantic_snapshot.as_ref() {
            let exact_compdb = checker.parser_manager.has_exact_compdb(path);
            let context_kind = checker
                .parser_manager
                .semantic_compdb_kind(path);
            let (reference_drop_tolerance, scope_drift_tolerance) =
                PostEditChecker::semantic_transition_tolerances_for_context(
                    context_kind,
                    exact_compdb,
                    edited_lines,
                );
            let identity_shift_tol = 4usize;
            let transition = checker.semantic_contract.evaluate_transition(
                before_semantic,
                &after_snapshot,
                reference_drop_tolerance,
                scope_drift_tolerance,
                identity_shift_tol,
                edited_lines,
            );
            if transition.identity_integrity_regressed {
                failure_kinds.insert(PostEditFailureKind::SemanticIdentityRegressed);
            }
            if transition.reference_integrity_regressed {
                failure_kinds.insert(PostEditFailureKind::SemanticReferenceIntegrityRegressed);
            }
            if transition.scope_integrity_regressed {
                failure_kinds.insert(PostEditFailureKind::SemanticScopeDriftRegressed);
            }
            messages.extend(transition.failure_messages);
            messages.extend(transition.warning_messages);
            culprit_lines.extend(transition.culprit_lines);
        }
    } else if let Some(note) = baseline.semantic_readiness_note() {
        messages.push(format!(
            "post-edit check warning: semantic integrity checks skipped; baseline readiness unmet ({note})"
        ));
    }

    if PostEditChecker::should_relax_consensus_diagnostic_failure(
        &checker.parser_manager,
        path,
        edited_lines,
        &failure_kinds,
        &clang_delta_lines,
        clang_severe_delta,
        clang_weighted_delta,
    ) {
        failure_kinds.remove(&PostEditFailureKind::ClangDiagnosticsIncreased);
        messages.retain(|item| {
            !item.starts_with("post-edit check failed: clang diagnostic weight increased")
                && !item.starts_with("post-edit check failed: clang diagnostics increased")
        });
        for line in &clang_delta_lines {
            culprit_lines.remove(line);
        }
        messages.push(format!(
            "post-edit check warning: relaxed clang diagnostic regression under consensus context because semantic invariants remained stable (lines={})",
            PostEditChecker::line_hint(clang_delta_lines.iter().copied(), 6)
        ));
    }
    let accepted = failure_kinds.is_empty()
        && !messages.iter().any(|m| {
            m.contains("usage-role consistency") || m.contains("touch contract")
        });
    PostEditCheckResult {
        accepted,
        messages,
        failure_kinds,
        culprit_lines,
    }
}

pub(super) fn validate_structural_only(
    checker: &PostEditChecker,
    path: &Path,
    after_text: &str,
    baseline: &CheckBaseline,
) -> PostEditCheckResult {
    let mut messages = baseline.warnings.clone();
    let mut failure_kinds = BTreeSet::<PostEditFailureKind>::new();
    let mut culprit_lines = BTreeSet::new();

    let tree_result = checker.parser_manager.parse_tree_sitter(after_text, path);
    match tree_result {
        Ok(tree) => {
            let (after_ratio, tree_error_lines) =
                PostEditChecker::tree_error_ratio_and_lines(&tree);
            if matches!(baseline.before_tree_error, Some(false)) && tree.root_node().has_error() {
                failure_kinds.insert(PostEditFailureKind::TreeErrorRegressed);
                messages.push(
                    "post-edit check failed: tree-sitter parse quality regressed".to_string(),
                );
                culprit_lines.extend(tree_error_lines.iter().copied());
            }
            if let Some(before_ratio) = baseline.before_tree_error_ratio {
                let adaptive_tolerance = checker.tree_error_ratio_tolerance;
                let allowed = (before_ratio + adaptive_tolerance).clamp(0.0, 1.0);
                if after_ratio > allowed {
                    failure_kinds.insert(PostEditFailureKind::TreeErrorRatioRegressed);
                    messages.push(format!(
                        "post-edit check failed: tree-sitter error ratio increased ({before_ratio:.4} -> {after_ratio:.4})"
                    ));
                    culprit_lines.extend(tree_error_lines.iter().copied());
                }
            }
        }
        Err(err) => {
            if checker.fail_on_parser_unavailable {
                failure_kinds.insert(PostEditFailureKind::ParserUnavailableTree);
                messages.push(format!(
                    "post-edit check failed: tree-sitter parser unavailable after edit: {err}"
                ));
            } else {
                messages.push(format!("post-edit check warning (after tree): {err}"));
            }
        }
    }

    let accepted = failure_kinds.is_empty();
    PostEditCheckResult {
        accepted,
        messages,
        failure_kinds,
        culprit_lines,
    }
}
