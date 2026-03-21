use std::collections::BTreeSet;

use super::{
    CoordinatedPolicyStage, PartialRollbackInput, PolicyCheckpointResult,
    PolicyPipeline, PolicyPipelineRunState,
};
use crate::model::edit::Edit;

impl PolicyPipeline {
    pub(super) fn checkpoint_policy_stage(
        &self,
        state: &PolicyPipelineRunState<'_>,
        coordinated: &CoordinatedPolicyStage,
        policy_name: &str,
    ) -> PolicyCheckpointResult {
        if !coordinated.text_changed {
            return PolicyCheckpointResult::Accept { validated_tree: None };
        }

        let before_errors = state
            .tree_for_text
            .as_ref()
            .map(|t| crate::parser::ts_traversal::tree_error_stats(t).error_nodes)
            .unwrap_or(0);

        let after_tree = self
            .parser_manager
            .parse_tree_sitter(
                &coordinated.result.text,
                state.path,
            );

        match after_tree {
            Ok(tree) => {
                let stats = crate::parser::ts_traversal::tree_error_stats(&tree);
                if stats.error_nodes <= before_errors {
                    return PolicyCheckpointResult::Accept {
                        validated_tree: Some(tree),
                    };
                }
                if let Some(before_clang) = state.clang_for_text.as_ref() {
                    let before_clang_errors = before_clang.error_diagnostic_count();
                    if let Ok(after_clang) = self.parser_manager.parse_clang(
                        &coordinated.result.text,
                        state.path,
                    ) {
                        if after_clang.error_diagnostic_count() <= before_clang_errors {
                            return PolicyCheckpointResult::SensorDisagreementAccept {
                                validated_tree: Some(tree),
                                warning: format!(
                                    "checkpoint: '{}' sensor disagreement — tree-sitter +{} error(s) but clang OK, accepting",
                                    policy_name,
                                    stats.error_nodes.saturating_sub(before_errors),
                                ),
                            };
                        }
                    }
                }
                let new_error_lines: BTreeSet<usize> = stats.error_lines;
                let rollback_attempt = self.attempt_partial_rollback(
                    state,
                    PartialRollbackInput {
                        coordinated,
                        policy_name,
                        before_errors,
                        new_error_lines: &new_error_lines,
                    },
                );
                if let Some(partial) = rollback_attempt {
                    return partial;
                }
                PolicyCheckpointResult::Rollback {
                    reason: format!(
                        "checkpoint: '{}' introduced {} new error node(s) — rolling back",
                        policy_name,
                        stats.error_nodes.saturating_sub(before_errors),
                    ),
                    after_error_count: stats.error_nodes,
                }
            }
            Err(_) => {
                PolicyCheckpointResult::Accept { validated_tree: None }
            }
        }
    }

    pub(super) fn attempt_partial_rollback(
        &self,
        state: &PolicyPipelineRunState<'_>,
        input: PartialRollbackInput<'_>,
    ) -> Option<PolicyCheckpointResult> {
        let PartialRollbackInput {
            coordinated,
            policy_name,
            before_errors,
            new_error_lines,
        } = input;
        if coordinated.result.edits.is_empty() {
            return None;
        }
        let safe_edits: Vec<Edit> = coordinated
            .result
            .edits
            .iter()
            .filter(|edit| !new_error_lines.contains(&edit.line))
            .cloned()
            .collect();
        if safe_edits.is_empty() {
            return None;
        }
        let recovered = Self::apply_synthesized_edits_best_effort(&state.current, &safe_edits);
        let recovered_text = recovered?;
        let recovered_tree = self
            .parser_manager
            .parse_tree_sitter(&recovered_text, state.path)
            .ok()?;
        let recovered_errors =
            crate::parser::ts_traversal::tree_error_stats(&recovered_tree).error_nodes;
        if recovered_errors > before_errors {
            return None;
        }
        let warning = format!(
            "checkpoint: '{}' partial rollback recovered {} of {} edits ({} error lines excluded)",
            policy_name,
            safe_edits.len(),
            coordinated.result.edits.len(),
            new_error_lines.len(),
        );
        Some(PolicyCheckpointResult::PartialRollback {
            recovered_text,
            recovered_edits: safe_edits,
            validated_tree: Some(recovered_tree),
            warning,
        })
    }
}
