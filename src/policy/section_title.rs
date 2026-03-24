use std::collections::HashMap;

use crate::model::edit::Edit;
use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::model::violation::Violation;
use crate::policy::Policy;
use std::borrow::Cow;

use crate::policy::text_utils::join_lines_cow;
use crate::parser::text_scan;

pub struct SectionTitleNormalizerPolicy {
    mapping: HashMap<String, String>,
}

impl SectionTitleNormalizerPolicy {
    pub fn new(mapping: HashMap<String, String>) -> Self {
        Self { mapping }
    }
}

impl Policy for SectionTitleNormalizerPolicy {
    fn name(&self) -> &str {
        "section_title_normalizer"
    }

    fn apply(&self, context: &PolicyContext<'_>) -> PolicyResult {
        if self.mapping.is_empty() {
            return PolicyResult::unchanged();
        }
        let semantic_query = context.semantic_query();
        if !semantic_query.is_available() {
            return PolicyResult::unchanged_with_warning(
                "section_title_normalizer: semantic context unavailable; skipping heuristic edits"
                    .to_string(),
            );
        }

        let shared = context.shared.unwrap();
        let eol = shared.line_ending();
        let mut lines = shared.lines_cow();
        let trailing_newline = shared.trailing_newline();
        let mut edits = Vec::new();
        let mut warnings = Vec::new();
        let mut skipped_semantic_unsafe = 0usize;

        for (idx, line) in lines.iter_mut().enumerate() {
            if shared.is_macro_line(idx + 1) {
                skipped_semantic_unsafe = skipped_semantic_unsafe.saturating_add(1);
                continue;
            }
            let original = line.to_string();
            let trimmed = original.trim();
            if !trimmed.starts_with("//") {
                continue;
            }
            let comment = trimmed[2..].trim();
            if comment.is_empty() || text_scan::TEXT_SCAN.all_bytes_equal(comment.as_bytes(), b'-') {
                continue;
            }
            let key = comment.to_lowercase();
            let Some(target) = self.mapping.get(&key) else {
                continue;
            };

            let indent_len = text_scan::TEXT_SCAN.leading_whitespace_byte_count(original.as_bytes());
            let indent: &str = &original[..indent_len];
            let updated = format!("{indent}// {target}");
            if updated != original {
                edits.push(Edit {
                    policy: self.name().into(),
                    line: idx + 1,
                    before: original,
                    after: updated.clone(),
                });
                *line = Cow::Owned(updated);
            }
        }

        if skipped_semantic_unsafe > 0 {
            warnings.push(format!(
                "section_title_normalizer: skipped {} semantic-unsafe candidate line(s)",
                skipped_semantic_unsafe
            ));
            tracing::debug!(
                skipped = skipped_semantic_unsafe,
                "section_title_normalizer: skipped semantic-unsafe candidate line(s)"
            );
        }

        if edits.is_empty() {
            return PolicyResult::unchanged_with_warnings(warnings);
        }

        PolicyResult {
            text: join_lines_cow(&lines, eol, trailing_newline),
            changed: true,
            violations: vec![Violation {
                policy: self.name().into(),
                message: "normalized section title comments".to_string(),
                line: edits[0].line,
                column: Some(1),
            }],
            edits,
            warnings,
            ..Default::default()
        }
    }
}
