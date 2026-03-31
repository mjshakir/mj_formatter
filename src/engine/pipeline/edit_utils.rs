use std::collections::BTreeSet;
use std::sync::Arc;

use crate::model::edit::Edit;
use crate::model::policy_result::PolicyResult;
use crate::policy::id::PolicyId;
use crate::parser::text_scan;

use super::PolicyPipeline;

impl PolicyPipeline {
    pub(super) fn stabilize_output_text(
        input_text: &str,
        output_text: Arc<str>,
        edits: &[Edit],
        warnings: &mut Vec<String>,
    ) -> String {
        if edits.is_empty() && !text_scan::TEXT_SCAN.strings_equal(&output_text, input_text) {
            warnings.push(
                "pipeline guard: reverted untracked text delta because no final edits survived"
                    .to_string(),
            );
            return input_text.to_string();
        }
        output_text.to_string()
    }

    pub(super) fn normalize_edit_coverage(
        policy_name: &str,
        before_text: &str,
        mut result: PolicyResult,
    ) -> PolicyResult {
        if !result.changed && result.text.is_empty() {
            return result;
        }
        if text_scan::TEXT_SCAN.strings_equal(&result.text, before_text) {
            if !result.edits.is_empty() {
                result.edits.clear();
                result.warnings.push(format!(
                    "policy output guard: cleared stale edit records for '{}'",
                    policy_name
                ));
            }
            return result;
        }
        if !result.edits.is_empty()
            && Self::apply_synthesized_edits(before_text, result.edits.as_slice()).as_deref()
                == Some(result.text.as_str())
        {
            return result;
        }

        let synthesized =
            Self::synthesize_line_edits(before_text, result.text.as_str(), policy_name);
        if synthesized.is_empty() {
            return result;
        }
        let declared_lines = Self::edit_lines(result.edits.as_slice());
        let actual_lines = Self::edit_lines(synthesized.as_slice());
        if result.edits.is_empty()
            || !actual_lines
                .iter()
                .all(|line| declared_lines.contains(line))
            || declared_lines.len() != actual_lines.len()
        {
            result.warnings.push(format!(
                "policy output guard: normalized edit coverage for '{}' (declared_lines={}, actual_lines={})",
                policy_name,
                declared_lines.len(),
                actual_lines.len()
            ));
            result.edits = synthesized;
        }
        result
    }

    pub(super) fn synthesize_line_edits(before: &str, after: &str, policy_name: &str) -> Vec<Edit> {
        let before_lines = text_scan::split_lines_as_slices(before, true);
        let after_lines = text_scan::split_lines_as_slices(after, true);
        let common_len = before_lines.len().min(after_lines.len());
        let mut prefix = 0usize;
        while prefix < common_len
            && text_scan::TEXT_SCAN
                .slices_equal(before_lines[prefix].as_bytes(), after_lines[prefix].as_bytes())
        {
            prefix = prefix.saturating_add(1);
        }

        let mut before_tail = before_lines.len();
        let mut after_tail = after_lines.len();
        while before_tail > prefix
            && after_tail > prefix
            && text_scan::TEXT_SCAN.slices_equal(
                before_lines[before_tail - 1].as_bytes(),
                after_lines[after_tail - 1].as_bytes(),
            )
        {
            before_tail = before_tail.saturating_sub(1);
            after_tail = after_tail.saturating_sub(1);
        }

        let before_diff = &before_lines[prefix..before_tail];
        let after_diff = &after_lines[prefix..after_tail];
        let max_lines = before_diff.len().max(after_diff.len());
        let mut edits = Vec::<Edit>::new();
        for index in 0..max_lines {
            let left = before_diff.get(index).copied().unwrap_or("");
            let right = after_diff.get(index).copied().unwrap_or("");
            if left == right {
                continue;
            }
            edits.push(Edit {
                policy: PolicyId::from_str_lossy(policy_name),
                line: prefix + index + 1,
                before: left.to_string(),
                after: right.to_string(),
            });
        }
        edits
    }

    pub(super) fn has_nonlocal_change(before_text: &str, after_text: &str) -> bool {
        text_scan::TEXT_SCAN.has_line_count_changed(before_text, after_text)
    }

    pub(super) fn edit_lines(edits: &[Edit]) -> BTreeSet<usize> {
        edits
            .iter()
            .filter_map(|edit| (edit.line > 0 && edit.before != edit.after).then_some(edit.line))
            .collect::<BTreeSet<_>>()
    }

    pub(super) fn line_hint<I>(lines: I, line_count: usize, max_lines: usize) -> String
    where
        I: Iterator<Item = usize>,
    {
        let mut sample = lines
            .take(max_lines)
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        if line_count > sample.len() {
            sample.push(format!("+{}", line_count - sample.len()));
        }
        sample.join(",")
    }

    pub(super) fn apply_synthesized_edits(before_text: &str, edits: &[Edit]) -> Option<String> {
        if edits.is_empty() {
            return Some(before_text.to_string());
        }
        let lines = text_scan::TEXT_SCAN.split_lines_as_slices(before_text, true);
        let mut ordered = edits
            .iter()
            .filter(|edit| edit.line > 0)
            .collect::<Vec<_>>();
        ordered.sort_by_key(|edit| edit.line);
        let mut result = String::with_capacity(before_text.len());
        let mut src = 0usize;
        for edit in ordered {
            let idx = edit.line.saturating_sub(1);
            if idx < src {
                return None;
            }
            while src < idx {
                if src >= lines.len() {
                    return None;
                }
                result.push_str(lines[src]);
                src += 1;
            }
            let insertion = edit.before.is_empty() && !edit.after.is_empty();
            let deletion = !edit.before.is_empty() && edit.after.is_empty();
            if insertion {
                if idx > lines.len() {
                    return None;
                }
                result.push_str(&edit.after);
            } else {
                if src >= lines.len() {
                    return None;
                }
                if lines[src] != edit.before {
                    return None;
                }
                if deletion {
                    src += 1;
                } else {
                    result.push_str(&edit.after);
                    src += 1;
                }
            }
        }
        while src < lines.len() {
            result.push_str(lines[src]);
            src += 1;
        }
        Some(result)
    }

    pub(super) fn apply_edits_lenient(before_text: &str, edits: &[Edit]) -> Option<String> {
        if edits.is_empty() {
            return Some(before_text.to_string());
        }
        let lines = text_scan::TEXT_SCAN.split_lines_as_slices(before_text, true);
        let mut ordered = edits
            .iter()
            .filter(|edit| edit.line > 0)
            .collect::<Vec<_>>();
        ordered.sort_by_key(|edit| edit.line);

        let mut result = String::with_capacity(before_text.len());
        let mut src = 0usize;
        let mut offset = 0isize;

        for edit in &ordered {
            let base_index = edit.line.saturating_sub(1) as isize + offset;
            if base_index < 0 {
                continue;
            }
            let index = base_index as usize;
            let insertion = edit.before.is_empty() && !edit.after.is_empty();
            let deletion = !edit.before.is_empty() && edit.after.is_empty();

            if insertion {
                if index > lines.len() {
                    continue;
                }
                while src < index && src < lines.len() {
                    result.push_str(lines[src]);
                    src += 1;
                }
                result.push_str(&edit.after);
                offset = offset.saturating_add(1);
                continue;
            }

            if index >= lines.len() {
                continue;
            }

            while src < index && src < lines.len() {
                result.push_str(lines[src]);
                src += 1;
            }

            if deletion {
                if src < lines.len() && lines[src] == edit.before {
                    src += 1;
                    offset = offset.saturating_sub(1);
                }
            } else if src < lines.len() && lines[src] == edit.before {
                result.push_str(&edit.after);
                src += 1;
            }
        }

        while src < lines.len() {
            result.push_str(lines[src]);
            src += 1;
        }
        Some(result)
    }
}
