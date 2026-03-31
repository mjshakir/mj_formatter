use tree_sitter::{StreamingIterator, Tree};

use crate::model::edit::Edit;
use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::model::violation::Violation;
use crate::parser::query_cache::TsQueryCache;
use crate::policy::Policy;
use std::borrow::Cow;

use crate::policy::text_utils::join_lines_cow;

pub struct PragmaOnceSpacingPolicy {
    blank_lines_after: usize,
}

impl PragmaOnceSpacingPolicy {
    pub fn new(blank_lines_after: usize) -> Self {
        Self { blank_lines_after }
    }

    const PRAGMA_ONCE_QUERY: &str = r##"(preproc_call
        directive: (preproc_directive) @d (#eq? @d "#pragma")
        argument: (preproc_arg) @a (#eq? @a "once")
    ) @call"##;

    fn find_pragma_once_line(
        tree: &Tree,
        source: &[u8],
        query_cache: Option<&TsQueryCache>,
    ) -> Option<usize> {
        let query = query_cache?.get_or_compile(Self::PRAGMA_ONCE_QUERY).ok()?;
        let call_idx = query.capture_index_for_name("call")?;
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), source);
        while let Some(m) = { matches.advance(); matches.get() } {
            for capture in m.captures {
                if capture.index == call_idx {
                    return Some(capture.node.start_position().row);
                }
            }
        }
        None
    }
}

impl Policy for PragmaOnceSpacingPolicy {
    fn name(&self) -> &str {
        "pragma_once_spacing"
    }

    fn apply(&self, context: &PolicyContext<'_>) -> PolicyResult {
        let semantic_query = context.semantic_query();
        if !semantic_query.is_available() {
            return PolicyResult::unchanged_with_warning(
                "pragma_once_spacing: semantic context unavailable; skipping heuristic edits"
                    .to_string(),
            );
        }
        let text = context.text;
        let shared = context.shared.unwrap();
        let eol = shared.line_ending();
        let mut lines = shared.lines_cow();
        let trailing_newline = shared.trailing_newline();
        if lines.is_empty() {
            return PolicyResult::unchanged();
        }

        let pragma_index = context
            .tree_sitter_tree
            .and_then(|tree| Self::find_pragma_once_line(tree, text.as_bytes(), context.query_cache));
        let Some(pragma_index) = pragma_index else {
            return PolicyResult::unchanged();
        };

        let mut scan = pragma_index + 1;
        let mut existing_blank = 0usize;
        while scan < lines.len() && lines[scan].trim().is_empty() {
            existing_blank += 1;
            scan += 1;
        }

        if existing_blank == self.blank_lines_after {
            return PolicyResult::unchanged();
        }

        let start = pragma_index + 1;
        let end = start + existing_blank;
        let before_segment: Vec<String> = lines[start..end].iter().map(|c| c.to_string()).collect();
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
            return PolicyResult::unchanged_with_warning(format!(
                "pragma_once_spacing: skipped {} semantic-unsafe candidate region(s)",
                skipped_semantic_unsafe
            ));
        }
        lines.splice(
            start..end,
            std::iter::repeat_n(Cow::Owned(String::new()), self.blank_lines_after),
        );
        let after_segment: Vec<String> = lines[start..start + self.blank_lines_after].iter().map(|c| c.to_string()).collect();
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
            return PolicyResult::unchanged();
        }

        PolicyResult {
            text: join_lines_cow(&lines, eol, trailing_newline),
            changed: true,
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
