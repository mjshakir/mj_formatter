use std::collections::HashSet;

use tree_sitter::{Node, StreamingIterator};

use crate::model::edit::Edit;
use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::model::violation::Violation;
use crate::parser::query_cache::TsQueryCache;
use crate::policy::traits::Policy;
use crate::policy::text_utils::{detect_line_ending, join_lines, split_lines};
use crate::text_scan;

pub struct OperatorOverloadSpacingPolicy;

impl OperatorOverloadSpacingPolicy {
    pub fn new() -> Self {
        Self
    }

    fn is_identifier_char(ch: u8) -> bool {
        ch.is_ascii_alphanumeric() || ch == b'_'
    }

    fn is_operator_token_start(ch: u8) -> bool {
        matches!(
            ch,
            b'<' | b'>'
                | b'-'
                | b'='
                | b'!'
                | b'&'
                | b'|'
                | b'+'
                | b'*'
                | b'/'
                | b'%'
                | b'^'
                | b'~'
                | b'('
                | b')'
                | b'['
                | b']'
                | b','
        )
    }

    fn consume_operator_token(bytes: &[u8], mut start: usize) -> usize {
        if start >= bytes.len() {
            return start;
        }
        if bytes[start] == b'(' && start + 1 < bytes.len() && bytes[start + 1] == b')' {
            return start + 2;
        }
        if bytes[start] == b'[' && start + 1 < bytes.len() && bytes[start + 1] == b']' {
            return start + 2;
        }
        if !Self::is_operator_token_start(bytes[start]) {
            return start;
        }

        start += 1;
        while start < bytes.len() && Self::is_operator_token_start(bytes[start]) {
            start += 1;
        }
        start
    }

    fn normalize_line(line: &str) -> String {
        if !line.contains("operator") {
            return line.to_string();
        }

        const OPERATOR: &[u8] = b"operator";
        let bytes = line.as_bytes();
        let mut out = String::with_capacity(line.len());
        let mut cursor = 0usize;

        while let Some(op_start) = text_scan::find_subslice_from(bytes, OPERATOR, cursor) {
            let op_end = op_start + "operator".len();

            if op_start > 0 && Self::is_identifier_char(bytes[op_start - 1]) {
                out.push_str(&line[cursor..op_end]);
                cursor = op_end;
                continue;
            }

            let mut token_start = op_end;
            while token_start < bytes.len() && bytes[token_start].is_ascii_whitespace() {
                token_start += 1;
            }
            let token_end = Self::consume_operator_token(bytes, token_start);
            if token_end == token_start {
                out.push_str(&line[cursor..op_end]);
                cursor = op_end;
                continue;
            }

            out.push_str(&line[cursor..op_end]);
            out.push_str(&line[token_start..token_end]);
            cursor = token_end;
        }

        out.push_str(&line[cursor..]);
        out
    }

    const OPERATOR_QUERY: &str = r#"[
        (function_declarator) @decl
        (operator_name) @decl
        (operator_cast) @decl
    ]"#;

    fn collect_candidate_lines(
        root: Node<'_>,
        text: &str,
        query_cache: Option<&TsQueryCache>,
    ) -> HashSet<usize> {
        let mut result = HashSet::new();

        if let Some(query) = query_cache
            .and_then(|qc| qc.get_or_compile(Self::OPERATOR_QUERY).ok())
        {
            let mut cursor = tree_sitter::QueryCursor::new();
            let mut matches = cursor.matches(&query, root, "".as_bytes());
            while let Some(m) = {
                matches.advance();
                matches.get()
            } {
                for capture in m.captures {
                    let snippet = capture.node.utf8_text(text.as_bytes()).unwrap_or("");
                    if snippet.contains("operator") {
                        let start = capture.node.start_position().row;
                        let end = capture.node.end_position().row;
                        for line_idx in start..=end {
                            result.insert(line_idx);
                        }
                    }
                }
            }
        } else {
            let mut stack = vec![root];
            while let Some(node) = stack.pop() {
                if matches!(
                    node.kind(),
                    "function_declarator" | "operator_name" | "operator_cast"
                ) {
                    let snippet = node.utf8_text(text.as_bytes()).unwrap_or("");
                    if snippet.contains("operator") {
                        let start = node.start_position().row;
                        let end = node.end_position().row;
                        for line_idx in start..=end {
                            result.insert(line_idx);
                        }
                    }
                }
                for child_idx in (0..node.child_count()).rev() {
                    if let Some(child) = node.child(child_idx as u32) {
                        stack.push(child);
                    }
                }
            }
        }

        result
    }
}

impl Policy for OperatorOverloadSpacingPolicy {
    fn name(&self) -> &str {
        "operator_overload_spacing"
    }
    fn apply(&self, context: &PolicyContext<'_>) -> PolicyResult {
        let Some(tree) = context.tree_sitter_tree else {
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits: Vec::new(),
                warnings: vec![
                    "operator_overload_spacing: tree-sitter context unavailable".to_string()
                ],
            };
        };
        let semantic_query = context.semantic_query();
        if !semantic_query.is_available() {
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits: Vec::new(),
                warnings: vec![
                    "operator_overload_spacing: semantic context unavailable; skipping heuristic edits"
                        .to_string(),
                ],
            };
        }

        let eol = detect_line_ending(context.text);
        let (mut lines, trailing_newline) = split_lines(context.text);
        let mut edits = Vec::new();
        let mut skipped_semantic_unsafe = 0usize;

        let mut candidate_lines =
            Self::collect_candidate_lines(tree.root_node(), context.text, context.query_cache)
            .into_iter()
            .collect::<Vec<_>>();
        candidate_lines.sort_unstable();

        for idx in candidate_lines {
            if idx >= lines.len() {
                continue;
            }
            if semantic_query.is_available() && semantic_query.is_macro_region(idx + 1, 1) {
                skipped_semantic_unsafe = skipped_semantic_unsafe.saturating_add(1);
                continue;
            }
            let original = lines[idx].clone();
            if !original.contains("operator") {
                continue;
            }
            let updated = Self::normalize_line(&original);
            if updated != original {
                lines[idx] = updated.clone();
                edits.push(Edit {
                    policy: self.name().into(),
                    line: idx + 1,
                    before: original,
                    after: updated,
                });
            }
        }

        if edits.is_empty() {
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits,
                warnings: Vec::new(),
            };
        }

        let mut warnings = Vec::new();
        if skipped_semantic_unsafe > 0 {
            warnings.push(format!(
                "operator_overload_spacing: skipped {} semantic-unsafe candidate line(s)",
                skipped_semantic_unsafe
            ));
        }

        PolicyResult {
            text: join_lines(&lines, eol, trailing_newline),
            violations: vec![Violation {
                policy: self.name().into(),
                message: "normalized operator overload spacing".to_string(),
                line: edits[0].line,
                column: Some(1),
            }],
            edits,
            warnings,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use tree_sitter::Parser;

    use super::*;
    use crate::model::policy_context::PolicyContext;
    use crate::parser::manager::ParserManager;
    use crate::parser::file_context::SemanticFileContext;

    fn parse_cpp(text: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("cpp language");
        parser.parse(text, None).expect("parse tree")
    }

    fn semantic_for(
        text: &str,
        path: &Path,
        tree: &tree_sitter::Tree,
    ) -> (
        std::sync::Arc<crate::parser::clang_result::ClangParseResult>,
        SemanticFileContext,
    ) {
        let parser_manager = ParserManager::with_clang("clang".to_string(), Vec::new());
        let clang = parser_manager
            .parse_clang(text, path)
            .expect("clang parse for test context");
        let semantic = SemanticFileContext::from_parses(text, path, Some(tree), Some(&clang));
        (clang, semantic)
    }

    #[test]
    fn normalizes_symbol_operator_spacing() {
        let policy = OperatorOverloadSpacingPolicy::new();
        let path = PathBuf::from("op.cpp");
        let source = "struct X { X operator +(const X& rhs) const; };\n";
        let tree = parse_cpp(source);
        let (clang, semantic) = semantic_for(source, &path, &tree);
        let ctx = PolicyContext::new(source, &path)
            .with_tree_sitter_tree(Some(&tree))
            .with_clang_parse_result(Some(&*clang))
            .with_semantic_file_context(Some(&semantic));
        let result = policy.apply(&ctx);
        assert_eq!(
            result.text,
            "struct X { X operator+(const X& rhs) const; };\n"
        );
    }

    #[test]
    fn does_not_change_conversion_operator_spacing() {
        let policy = OperatorOverloadSpacingPolicy::new();
        let path = PathBuf::from("op.cpp");
        let source = "struct X { explicit operator bool() const; };\n";
        let tree = parse_cpp(source);
        let (clang, semantic) = semantic_for(source, &path, &tree);
        let ctx = PolicyContext::new(source, &path)
            .with_tree_sitter_tree(Some(&tree))
            .with_clang_parse_result(Some(&*clang))
            .with_semantic_file_context(Some(&semantic));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, source);
    }
}
