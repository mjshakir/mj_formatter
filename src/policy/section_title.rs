use std::collections::HashMap;

use crate::model::edit::Edit;
use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::model::violation::Violation;
use crate::policy::Policy;
use crate::policy::text_utils::{detect_line_ending, join_lines, split_lines};
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
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits: Vec::new(),
                warnings: Vec::new(),
            };
        }
        let semantic_query = context.semantic_query();
        if !semantic_query.is_available() {
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits: Vec::new(),
                warnings: vec![
                    "section_title_normalizer: semantic context unavailable; skipping heuristic edits"
                        .to_string(),
                ],
            };
        }

        let eol = detect_line_ending(context.text);
        let (mut lines, trailing_newline) = split_lines(context.text);
        let mut edits = Vec::new();
        let mut skipped_semantic_unsafe = 0usize;

        for (idx, line) in lines.iter_mut().enumerate() {
            if semantic_query.is_available() && semantic_query.is_macro_region(idx + 1, 1) {
                skipped_semantic_unsafe = skipped_semantic_unsafe.saturating_add(1);
                continue;
            }
            let original = line.clone();
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
            let indent: String = original[..indent_len].to_string();
            let updated = format!("{indent}// {target}");
            if updated != original {
                edits.push(Edit {
                    policy: self.name().into(),
                    line: idx + 1,
                    before: original,
                    after: updated.clone(),
                });
                *line = updated;
            }
        }

        if edits.is_empty() {
            let mut warnings = Vec::new();
            if skipped_semantic_unsafe > 0 {
                warnings.push(format!(
                    "section_title_normalizer: skipped {} semantic-unsafe candidate line(s)",
                    skipped_semantic_unsafe
                ));
            }
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits: Vec::new(),
                warnings,
            };
        }

        let mut warnings = Vec::new();
        if skipped_semantic_unsafe > 0 {
            warnings.push(format!(
                "section_title_normalizer: skipped {} semantic-unsafe candidate line(s)",
                skipped_semantic_unsafe
            ));
        }
        PolicyResult {
            text: join_lines(&lines, eol, trailing_newline),
            violations: vec![Violation {
                policy: self.name().into(),
                message: "normalized section title comments".to_string(),
                line: edits[0].line,
                column: Some(1),
            }],
            edits,
            warnings,
        }
    }
}
