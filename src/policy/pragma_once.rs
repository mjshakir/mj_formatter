use crate::model::edit::Edit;
use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::model::violation::Violation;
use crate::policy::Policy;
use crate::policy::text_utils::{detect_line_ending, join_lines, split_lines};

pub struct PragmaOnceSpacingPolicy {
    blank_lines_after: usize,
}

impl PragmaOnceSpacingPolicy {
    pub fn new(blank_lines_after: usize) -> Self {
        Self { blank_lines_after }
    }
}

impl Policy for PragmaOnceSpacingPolicy {
    fn name(&self) -> &str {
        "pragma_once_spacing"
    }

    fn apply(&self, context: &PolicyContext<'_>) -> PolicyResult {
        let semantic_query = context.semantic_query();
        if !semantic_query.is_available() {
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits: Vec::new(),
                warnings: vec![
                    "pragma_once_spacing: semantic context unavailable; skipping heuristic edits"
                        .to_string(),
                ],
            };
        }
        let eol = detect_line_ending(context.text);
        let (mut lines, trailing_newline) = split_lines(context.text);
        if lines.is_empty() {
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits: Vec::new(),
                warnings: Vec::new(),
            };
        }

        let pragma_index = lines
            .iter()
            .position(|line| line.trim().eq_ignore_ascii_case("#pragma once"));
        let Some(pragma_index) = pragma_index else {
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits: Vec::new(),
                warnings: Vec::new(),
            };
        };

        let mut scan = pragma_index + 1;
        let mut existing_blank = 0usize;
        while scan < lines.len() && lines[scan].trim().is_empty() {
            existing_blank += 1;
            scan += 1;
        }

        if existing_blank == self.blank_lines_after {
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits: Vec::new(),
                warnings: Vec::new(),
            };
        }

        let start = pragma_index + 1;
        let end = start + existing_blank;
        let before_segment = lines[start..end].to_vec();
        let mut skipped_semantic_unsafe = 0usize;
        if !semantic_query.is_safe_global(pragma_index + 1, 1) {
            skipped_semantic_unsafe = skipped_semantic_unsafe.saturating_add(1);
        }
        if skipped_semantic_unsafe == 0
            && (start + 1..=end.max(start + self.blank_lines_after))
                .any(|line| !semantic_query.is_safe_global(line, 1))
        {
            skipped_semantic_unsafe = skipped_semantic_unsafe.saturating_add(1);
        }
        if skipped_semantic_unsafe > 0 {
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits: Vec::new(),
                warnings: vec![format!(
                    "pragma_once_spacing: skipped {} semantic-unsafe candidate region(s)",
                    skipped_semantic_unsafe
                )],
            };
        }
        lines.splice(
            start..end,
            std::iter::repeat_n(String::new(), self.blank_lines_after),
        );
        let after_segment = lines[start..start + self.blank_lines_after].to_vec();
        let max_lines = before_segment.len().max(after_segment.len());
        let mut edits = Vec::<Edit>::new();
        for idx in 0..max_lines {
            let before_line = before_segment.get(idx).cloned().unwrap_or_default();
            let after_line = after_segment.get(idx).cloned().unwrap_or_default();
            if before_line == after_line {
                continue;
            }
            edits.push(Edit {
                policy: self.name().into(),
                line: start + idx + 1,
                before: before_line,
                after: after_line,
            });
        }
        if edits.is_empty() {
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits,
                warnings: Vec::new(),
            };
        }

        PolicyResult {
            text: join_lines(&lines, eol, trailing_newline),
            violations: vec![Violation {
                policy: self.name().into(),
                message: "normalized blank lines after #pragma once".to_string(),
                line: edits[0].line,
                column: Some(1),
            }],
            edits,
            warnings: Vec::new(),
        }
    }
}
