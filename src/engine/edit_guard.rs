use std::collections::BTreeSet;

use tree_sitter::{StreamingIterator, Tree};

use crate::config::enums::TouchContract;
use crate::engine::catalog::PolicyCertainty;
use crate::model::edit::Edit;
use crate::model::violation::Violation;
use crate::parser::query_cache::TsQueryCache;

pub struct EditGuard;

impl EditGuard {
    pub fn validate(
        policy_name: &str,
        contract: &TouchContract,
        edits: &[Edit],
        tree: Option<&Tree>,
        query_cache: Option<&TsQueryCache>,
        certainty: Option<&PolicyCertainty>,
        structural_safe: bool,
    ) -> Vec<Violation> {
        if edits.is_empty() || matches!(contract, TouchContract::Any) {
            return Vec::new();
        }

        if matches!(contract, TouchContract::WhitespaceOnly) {
            return Self::check_whitespace_only(policy_name, edits);
        }

        let changed_lines: BTreeSet<usize> = edits
            .iter()
            .filter_map(|edit| (edit.line > 0).then_some(edit.line))
            .collect();
        if changed_lines.is_empty() {
            return Vec::new();
        }

        let Some(tree) = tree else {
            return vec![Violation {
                policy: "edit_guard".into(),
                message: format!(
                    "Blocked edits from '{}': touch contract requires tree-sitter context",
                    policy_name
                ),
                line: edits[0].line.max(1),
                column: Some(1),
            }];
        };

        let (comments, strings, preprocessor) = Self::collect_protected_lines(tree, query_cache);
        let relax_comment_string =
            structural_safe && crate::engine::fuzzy_inference::fuzzy_guard_relax(certainty);
        match contract {
            TouchContract::CodeOnly => {
                let blocked = changed_lines
                    .iter()
                    .copied()
                    .filter(|line| {
                        preprocessor.contains(line)
                            || (!relax_comment_string
                                && (comments.contains(line) || strings.contains(line)))
                    })
                    .collect::<Vec<_>>();
                if blocked.is_empty() {
                    return Vec::new();
                }
                vec![Violation {
                    policy: "edit_guard".into(),
                    message: format!(
                        "Blocked edits from '{}': code_only contract touched protected lines {}",
                        policy_name,
                        Self::line_preview(blocked.as_slice(), 6)
                    ),
                    line: blocked[0],
                    column: Some(1),
                }]
            }
            TouchContract::PreprocessorOnly => {
                let blocked = changed_lines
                    .iter()
                    .copied()
                    .filter(|line| !preprocessor.contains(line))
                    .collect::<Vec<_>>();
                if blocked.is_empty() {
                    return Vec::new();
                }
                vec![Violation {
                    policy: "edit_guard".into(),
                    message: format!(
                        "Blocked edits from '{}': preprocessor_only contract touched non-preprocessor lines {}",
                        policy_name,
                        Self::line_preview(blocked.as_slice(), 6)
                    ),
                    line: blocked[0],
                    column: Some(1),
                }]
            }
            TouchContract::Any | TouchContract::WhitespaceOnly => Vec::new(),
        }
    }

    fn check_whitespace_only(policy_name: &str, edits: &[Edit]) -> Vec<Violation> {
        for edit in edits {
            if edit.before.trim() != edit.after.trim() {
                return vec![Violation {
                    policy: "edit_guard".into(),
                    message: format!(
                        "Blocked edits from '{}': whitespace_only contract changed non-whitespace content on line {}",
                        policy_name, edit.line
                    ),
                    line: edit.line.max(1),
                    column: Some(1),
                }];
            }
        }
        Vec::new()
    }

    const PROTECTED_QUERY: &str = r#"[
        (comment) @comment
        (string_literal) @string
        (raw_string_literal) @string
        (char_literal) @string
        (system_lib_string) @string
        (concatenated_string) @string
        (preproc_if) @preproc
        (preproc_ifdef) @preproc
        (preproc_elif) @preproc
        (preproc_else) @preproc
        (preproc_include) @preproc
        (preproc_def) @preproc
        (preproc_function_def) @preproc
    ]"#;

    fn collect_protected_lines(
        tree: &Tree,
        query_cache: Option<&TsQueryCache>,
    ) -> (BTreeSet<usize>, BTreeSet<usize>, BTreeSet<usize>) {
        let mut comments = BTreeSet::<usize>::new();
        let mut strings = BTreeSet::<usize>::new();
        let mut preprocessor = BTreeSet::<usize>::new();

        let query = query_cache
            .and_then(|qc| qc.get_or_compile(Self::PROTECTED_QUERY).ok());

        let Some(query) = query else {
            return Self::collect_protected_lines_dfs(tree);
        };

        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), "".as_bytes());

        let comment_idx = query.capture_index_for_name("comment");
        let string_idx = query.capture_index_for_name("string");
        let preproc_idx = query.capture_index_for_name("preproc");

        while let Some(m) = {
            matches.advance();
            matches.get()
        } {
            for capture in m.captures {
                let start = capture.node.start_position().row.saturating_add(1);
                let end = capture.node.end_position().row.saturating_add(1).max(start);
                let target = if Some(capture.index) == comment_idx {
                    &mut comments
                } else if Some(capture.index) == string_idx {
                    &mut strings
                } else if Some(capture.index) == preproc_idx {
                    &mut preprocessor
                } else {
                    continue;
                };
                for line in start..=end {
                    target.insert(line);
                }
            }
        }

        (comments, strings, preprocessor)
    }

    fn collect_protected_lines_dfs(
        tree: &Tree,
    ) -> (BTreeSet<usize>, BTreeSet<usize>, BTreeSet<usize>) {
        use crate::parser::node_kind;
        let mut comments = BTreeSet::<usize>::new();
        let mut strings = BTreeSet::<usize>::new();
        let mut preprocessor = BTreeSet::<usize>::new();
        let mut stack = vec![tree.root_node()];

        while let Some(node) = stack.pop() {
            let kind = node.kind();
            let start = node.start_position().row.saturating_add(1);
            let end = node.end_position().row.saturating_add(1).max(start);

            if kind == node_kind::COMMENT {
                for line in start..=end {
                    comments.insert(line);
                }
            } else if node_kind::is_string_like(kind) {
                for line in start..=end {
                    strings.insert(line);
                }
            } else if kind.starts_with("preproc_") && kind != node_kind::PREPROC_CALL {
                for line in start..=end {
                    preprocessor.insert(line);
                }
            }

            for index in (0..node.child_count()).rev() {
                if let Some(child) = node.child(index as u32) {
                    stack.push(child);
                }
            }
        }

        (comments, strings, preprocessor)
    }

    fn line_preview(lines: &[usize], limit: usize) -> String {
        if lines.len() <= limit {
            return lines
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(", ");
        }
        let mut head = lines
            .iter()
            .take(limit)
            .map(usize::to_string)
            .collect::<Vec<_>>();
        head.push("...".to_string());
        head.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use tree_sitter::Parser;

    use crate::config::enums::TouchContract;
    use crate::model::edit::Edit;

    use super::EditGuard;

    #[test]
    fn whitespace_blocks_nonws() {
        let edits = vec![Edit {
            policy: "p".into(),
            line: 3,
            before: "int A".to_string(),
            after: "int B".to_string(),
        }];
        let violations = EditGuard::validate(
            "policy_x",
            &TouchContract::WhitespaceOnly,
            edits.as_slice(),
            None,
            None,
            None,
            false,
        );
        assert_eq!(violations.len(), 1);
        assert!(violations[0].message.contains("whitespace_only"));
    }

    #[test]
    fn blocks_comment_edits() {
        let source = "// comment\nint x = 1;\n";
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("cpp language");
        let tree = parser.parse(source, None).expect("parse tree");
        let edits = vec![Edit {
            policy: "p".into(),
            line: 1,
            before: "// comment".to_string(),
            after: "// changed".to_string(),
        }];
        let violations = EditGuard::validate(
            "policy_x",
            &TouchContract::CodeOnly,
            edits.as_slice(),
            Some(&tree),
            None,
            None,
            false,
        );
        assert_eq!(violations.len(), 1);
        assert!(violations[0].message.contains("code_only"));
    }
}
