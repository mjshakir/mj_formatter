use crate::model::edit::Edit;
use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::model::violation::Violation;
use crate::policy::Policy;
use std::borrow::Cow;

use crate::policy::text_utils::join_lines_cow;
use crate::parser::text_scan;

pub struct DashCommentNormalizerPolicy {
    mode_auto: bool,
    long_length: usize,
    short_length: usize,
    long_threshold: usize,
    min_length: usize,
}

impl DashCommentNormalizerPolicy {
    pub fn new(
        mode_auto: bool,
        long_length: usize,
        short_length: usize,
        long_threshold: usize,
        min_length: usize,
    ) -> Self {
        Self {
            mode_auto,
            long_length,
            short_length,
            long_threshold,
            min_length,
        }
    }

    fn is_dash_comment(line: &str) -> bool {
        let trimmed = line.trim();
        trimmed.starts_with("//")
            && trimmed.len() > 2
            && text_scan::TEXT_SCAN.all_bytes_equal(&trimmed.as_bytes()[2..], b'-')
    }

    fn adjacent_title_length(lines: &[Cow<'_, str>], index: usize) -> Option<usize> {
        let neighbors = [index.checked_sub(1), Some(index + 1)];
        for neighbor in neighbors.into_iter().flatten() {
            if neighbor >= lines.len() {
                continue;
            }
            let line = lines[neighbor].trim();
            if !line.starts_with("//") {
                continue;
            }
            let comment = line[2..].trim();
            if comment.is_empty() || text_scan::TEXT_SCAN.all_bytes_equal(comment.as_bytes(), b'-') {
                continue;
            }
            return Some(line.len());
        }
        None
    }
}

impl Policy for DashCommentNormalizerPolicy {
    fn name(&self) -> &str {
        "dash_comment_normalizer"
    }

    fn apply(&self, context: &PolicyContext<'_>) -> PolicyResult {
        let semantic_query = context.semantic_query();
        if !semantic_query.is_available() {
            return PolicyResult::unchanged_with_warning(
                "dash_comment_normalizer: semantic context unavailable; skipping heuristic edits"
                    .to_string(),
            );
        }
        let shared = context.shared.unwrap();
        let eol = shared.line_ending();
        let mut lines = shared.lines_cow();
        let trailing_newline = shared.trailing_newline();
        if lines.is_empty() {
            return PolicyResult::unchanged();
        }

        let mut edits = Vec::new();
        let mut warnings = Vec::new();
        let mut skipped_semantic_unsafe = 0usize;
        for idx in 0..lines.len() {
            if shared.is_macro_line(idx + 1) {
                skipped_semantic_unsafe = skipped_semantic_unsafe.saturating_add(1);
                continue;
            }
            let original = lines[idx].to_string();
            if !Self::is_dash_comment(&original) {
                continue;
            }

            let indent_len = text_scan::TEXT_SCAN.leading_whitespace_byte_count(original.as_bytes());
            let indent: &str = &original[..indent_len];
            let current_len = original.trim().len();

            let mut target_len = if current_len >= self.long_threshold {
                self.long_length
            } else {
                self.short_length
            };

            if self.mode_auto {
                if let Some(title_len) = Self::adjacent_title_length(&lines, idx) {
                    target_len = target_len.max(title_len).max(self.min_length);
                }
            }
            if target_len < 2 {
                continue;
            }

            let updated = format!("{}//{}", indent, "-".repeat(target_len.saturating_sub(2)));
            if updated != original {
                edits.push(Edit {
                    policy: self.name().into(),
                    line: idx + 1,
                    before: original,
                    after: updated.clone(),
                });
                lines[idx] = Cow::Owned(updated);
            }
        }

        if skipped_semantic_unsafe > 0 {
            warnings.push(format!(
                "dash_comment_normalizer: skipped {} semantic-unsafe candidate line(s)",
                skipped_semantic_unsafe
            ));
        }

        if edits.is_empty() {
            if skipped_semantic_unsafe > 0 {
                tracing::debug!(
                    skipped = skipped_semantic_unsafe,
                    "dash_comment_normalizer: skipped semantic-unsafe candidate line(s)"
                );
            }
            return PolicyResult::unchanged_with_warnings(warnings);
        }

        PolicyResult {
            text: join_lines_cow(&lines, eol, trailing_newline),
            changed: true,
            violations: vec![Violation {
                policy: self.name().into(),
                message: "normalized dashed comment separators".to_string(),
                line: edits[0].line,
                column: Some(1),
            }],
            edits,
            warnings,
            ..Default::default()
        }
    }
}
