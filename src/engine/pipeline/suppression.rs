use std::collections::BTreeSet;

use crate::model::policy_result::PolicyResult;
use crate::parser::text_scan;

use super::{PolicyPipeline, ScopeFilterConfig};

impl PolicyPipeline {
    pub(super) fn apply_scope_filter(
        before_text: &str,
        result: PolicyResult,
        allowed_lines: Option<&BTreeSet<usize>>,
        config: ScopeFilterConfig<'_>,
    ) -> PolicyResult {
        let Some(allowed_lines) = allowed_lines else {
            return result;
        };
        if allowed_lines.is_empty() || result.edits.is_empty() {
            return result;
        }
        let blocked_lines = result
            .edits
            .iter()
            .filter_map(|edit| {
                (edit.line > 0 && edit.before != edit.after && !allowed_lines.contains(&edit.line))
                    .then_some(edit.line)
            })
            .collect::<BTreeSet<_>>();
        if blocked_lines.is_empty() {
            return result;
        }
        let dropped = blocked_lines.len();
        if config.capability.semantic_rewrite {
            let mut reverted = result;
            reverted.text = before_text.to_string();
            reverted.edits.clear();
            reverted.warnings.push(format!(
                "retry_scope({scope_stage}) reverted semantic rewrite for '{}' because {} out-of-scope line(s) were required",
                config.policy_name,
                dropped,
                scope_stage = config.scope_stage
            ));
            return reverted;
        }
        let mut filtered = if config.capability.structural_safe {
            Self::suppress_structural(before_text, result, &blocked_lines)
        } else {
            Self::apply_line_suppression(before_text, result, &blocked_lines)
        };
        filtered.warnings.push(format!(
            "retry_scope({scope_stage}) dropped {} out-of-scope line(s) for '{}'",
            dropped,
            config.policy_name,
            scope_stage = config.scope_stage
        ));
        filtered
    }

    pub(super) fn apply_line_suppression(
        before_text: &str,
        result: PolicyResult,
        disabled_lines: &BTreeSet<usize>,
    ) -> PolicyResult {
        Self::suppress_lines_impl(before_text, result, disabled_lines)
    }

    pub(super) fn suppress_structural(
        before_text: &str,
        result: PolicyResult,
        disabled_lines: &BTreeSet<usize>,
    ) -> PolicyResult {
        Self::suppress_lines_impl(before_text, result, disabled_lines)
    }

    pub(super) fn suppress_lines_impl(
        before_text: &str,
        result: PolicyResult,
        disabled_lines: &BTreeSet<usize>,
    ) -> PolicyResult {
        if disabled_lines.is_empty() {
            return result;
        }
        let PolicyResult {
            text: result_text,
            violations: result_violations,
            edits: result_edits,
            mut warnings,
            changed: _,
        } = result;
        let kept_violations = result_violations
            .into_iter()
            .filter(|item| !disabled_lines.contains(&item.line))
            .collect::<Vec<_>>();
        let kept_edits = result_edits
            .into_iter()
            .filter(|item| !disabled_lines.contains(&item.line))
            .collect::<Vec<_>>();

        let before_lines = text_scan::split_lines_as_slices(before_text, true);
        let after_lines = text_scan::split_lines_as_slices(result_text.as_str(), true);
        let before_line_count = before_lines.len();
        let after_line_count = after_lines.len();
        let max_count = before_line_count.min(after_line_count);
        let suppressed_line_touched = disabled_lines.iter().any(|line_no| {
            let index = line_no.saturating_sub(1);
            if index < max_count {
                return !text_scan::TEXT_SCAN.slices_equal(
                    before_lines[index].as_bytes(),
                    after_lines[index].as_bytes(),
                );
            }
            if index < before_line_count {
                return true;
            }
            index < after_line_count
        });
        if !suppressed_line_touched {
            return PolicyResult {
                text: result_text,
                violations: kept_violations,
                edits: kept_edits,
                warnings,
                changed: true,
            };
        }
        let has_non_local_line_edits = before_line_count != after_line_count;
        if has_non_local_line_edits {
            let synthesized_policy = kept_edits
                .first()
                .map(|edit| edit.policy.as_str())
                .unwrap_or("line_suppression_guard");
            let synthesized =
                Self::synthesize_line_edits(before_text, result_text.as_str(), synthesized_policy);
            let filtered = synthesized
                .into_iter()
                .filter(|edit| !disabled_lines.contains(&edit.line))
                .collect::<Vec<_>>();
            let adjusted_text = Self::apply_synthesized_edits(before_text, &filtered)
                .or_else(|| Self::apply_edits_lenient(before_text, &filtered));
            let Some(adjusted_text) = adjusted_text else {
                warnings.push(
                    "line suppression escalated to full rollback due non-local line edits"
                        .to_string(),
                );
                return PolicyResult {
                    text: before_text.to_string(),
                    violations: kept_violations,
                    edits: Vec::new(),
                    warnings,
                    changed: false,
                };
            };
            let synthesized = Self::synthesize_line_edits(
                before_text,
                adjusted_text.as_str(),
                synthesized_policy,
            );
            let leaked_disabled_lines = synthesized
                .iter()
                .any(|edit| disabled_lines.contains(&edit.line));
            if !leaked_disabled_lines {
                warnings.push(format!(
                    "line suppression applied best-effort non-local rollback for {} blocked line(s)",
                    disabled_lines.len()
                ));
                return PolicyResult {
                    text: adjusted_text,
                    violations: kept_violations,
                    edits: synthesized,
                    warnings,
                    changed: true,
                };
            }
            warnings.push(
                "line suppression escalated to full rollback due non-local line edits".to_string(),
            );
            return PolicyResult {
                text: before_text.to_string(),
                violations: kept_violations,
                edits: Vec::new(),
                warnings,
                changed: false,
            };
        }

        if kept_edits.is_empty() {
            return PolicyResult {
                text: before_text.to_string(),
                violations: kept_violations,
                edits: kept_edits,
                warnings,
                changed: false,
            };
        }

        let mut before_lines = before_lines;
        let mut after_lines = after_lines;
        for line_no in disabled_lines {
            let index = line_no.saturating_sub(1);
            if index < max_count {
                after_lines[index] = before_lines[index];
            }
        }
        before_lines.clear();
        let text = after_lines.concat();
        PolicyResult {
            text,
            violations: kept_violations,
            edits: kept_edits,
            warnings,
            changed: true,
        }
    }
}
