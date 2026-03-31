use std::collections::BTreeSet;

use super::{
    CoordinatedPolicyStage, PartialRollbackInput, PolicyCheckpointResult,
    PolicyPipeline, PipelineState,
};
use crate::model::edit::Edit;

impl PolicyPipeline {
    pub(super) fn checkpoint_policy_stage(
        &self,
        state: &PipelineState<'_>,
        coordinated: &CoordinatedPolicyStage,
        policy_name: &str,
    ) -> PolicyCheckpointResult {
        if !coordinated.text_changed {
            return PolicyCheckpointResult::Accept { validated_tree: None };
        }

        let before_errors = state.parse.tree
            .as_ref()
            .map(|t| crate::parser::ts_traversal::tree_error_stats(t).error_nodes)
            .unwrap_or(0);

        let after_tree = if let Some(old_tree) = state.parse.tree.as_ref() {
            self.parser_manager.reparse_tree_incremental(
                &state.current,
                &coordinated.result.text,
                state.path,
                old_tree,
            )
        } else {
            self.parser_manager.reparse_tree(
                &coordinated.result.text,
                state.path,
                None,
            )
        };

        match after_tree {
            Ok(tree) => {
                let stats = crate::parser::ts_traversal::tree_error_stats(&tree);
                if stats.error_nodes <= before_errors {
                    return PolicyCheckpointResult::Accept {
                        validated_tree: Some(tree),
                    };
                }
                let after_clang_handle = self.parser_manager.dispatch_clang(
                    &coordinated.result.text,
                    state.path,
                ).ok().flatten();
                let before_clang_result = state.parse.clang.as_ref()
                    .map(|c| Ok(std::sync::Arc::clone(c)))
                    .unwrap_or_else(|| self.parser_manager.parse_clang(&state.current, state.path));
                if let Ok(before_clang) = before_clang_result {
                    let before_clang_errors = before_clang.error_diagnostic_count();
                    let after_clang_result = if let Some(handle) = after_clang_handle {
                        self.parser_manager.collect_clang(
                            handle,
                            &coordinated.result.text,
                            state.path,
                            std::time::Instant::now() + std::time::Duration::from_secs(30),
                        )
                    } else {
                        self.parser_manager.parse_clang(
                            &coordinated.result.text,
                            state.path,
                        )
                    };
                    if let Ok(after_clang) = after_clang_result {
                        if after_clang.error_diagnostic_count() <= before_clang_errors {
                            tracing::info!(
                                "checkpoint: '{}' tree-sitter +{} error(s) but clang OK — accepting (tree-sitter false positive)",
                                policy_name,
                                stats.error_nodes.saturating_sub(before_errors),
                            );
                            return PolicyCheckpointResult::Accept {
                                validated_tree: Some(tree),
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
        state: &PipelineState<'_>,
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
        let recovered = Self::apply_edits_lenient(&state.current, &safe_edits);
        let recovered_text = recovered?;
        let recovered_tree = if let Some(old_tree) = state.parse.tree.as_ref() {
            self.parser_manager
                .reparse_tree_incremental(&state.current, &recovered_text, state.path, old_tree)
                .ok()?
        } else {
            self.parser_manager
                .reparse_tree(&recovered_text, state.path, None)
                .ok()?
        };
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
