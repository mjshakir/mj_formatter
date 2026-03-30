use std::path::Path;
use std::sync::Arc;

use tree_sitter::Tree;

use crate::engine::semantic_contract::SemanticReadinessInput;
use crate::parser::clang_result::ClangParseResult;
use crate::parser::file_context::SemanticFileContext;

use super::{CheckBaseline, PostEditChecker};

pub(super) fn build(
    checker: &PostEditChecker,
    path: &Path,
    before_text: &str,
) -> CheckBaseline {
    let mut baseline = CheckBaseline::default();
    let mut before_tree = None::<Tree>;
    let mut before_clang = None::<Arc<ClangParseResult>>;
    let run_clang_validation = !checker.should_skip_nonexact_consensus_clang_validation(path);
    let tree_result = checker.parser_manager.parse_tree_sitter(before_text, path);
    let clang_result = if run_clang_validation {
        Some(checker.parser_manager.parse_clang(before_text, path))
    } else {
        None
    };
    match tree_result {
        Ok(tree) => {
            baseline.before_tree_error = Some(tree.root_node().has_error());
            let (ratio, _lines) = PostEditChecker::tree_error_ratio_and_lines(&tree);
            baseline.before_tree_error_ratio = Some(ratio);
            before_tree = Some(tree);
        }
        Err(err) => {
            if checker.fail_on_parser_unavailable {
                baseline.before_tree_unavailable = true;
                baseline.warnings.push(format!(
                    "post-edit check failed: tree-sitter parser unavailable before edit: {err}"
                ));
            } else {
                baseline
                    .warnings
                    .push(format!("post-edit check warning (before tree): {err}"));
            }
        }
    }
    if let Some(clang_result) = clang_result {
        match clang_result {
            Ok(parse) => {
                baseline.before_clang_error_count =
                    Some(PostEditChecker::clang_error_count(&parse));
                baseline.before_clang_fatal_count =
                    Some(PostEditChecker::clang_fatal_count(&parse));
                baseline.before_clang_summary = Some(parse.diagnostic_counts());
                baseline.before_clang_diagnostic_entries =
                    Some(parse.diagnostic_entries().to_vec());
                baseline.before_clang_error_lines = Some(parse.error_diagnostic_lines());
                before_clang = Some(parse);
            }
            Err(err) => {
                if checker.fail_on_parser_unavailable {
                    baseline.before_clang_unavailable = true;
                    baseline.warnings.push(format!(
                        "post-edit check failed: clang parser unavailable before edit: {err}"
                    ));
                } else {
                    baseline
                        .warnings
                        .push(format!("post-edit check warning (before clang): {err}"));
                }
            }
        }
    }
    let semantic_before = SemanticFileContext::from_parses(
        before_text,
        path,
        before_tree.as_ref(),
        before_clang.as_deref(),
    );
    let before_snapshot = checker.semantic_contract.snapshot(&semantic_before);
    let context_assessment = checker.semantic_contract.evaluate_context(&before_snapshot);
    baseline.before_semantic_snapshot = Some(before_snapshot.clone());
    baseline.warnings.extend(context_assessment.warnings);
    let readiness = checker.semantic_contract.evaluate_readiness_with_snapshot(
        SemanticReadinessInput {
            tree_unavailable: baseline.before_tree_unavailable,
            clang_unavailable: baseline.before_clang_unavailable,
        },
        Some(&before_snapshot),
    );
    baseline.before_semantic_ready = readiness.ready;
    if !readiness.ready {
        baseline.semantic_readiness_note = Some(readiness.summary());
    }
    baseline
}
