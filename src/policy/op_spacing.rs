use tree_sitter::Node;

use crate::model::edit::Edit;
use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::model::violation::Violation;
use crate::parser::query_cache::TsQueryCache;
use crate::parser::ts_cpp_symbols;
use crate::parser::ts_traversal;
use crate::policy::Policy;
use std::borrow::Cow;

use crate::policy::text_utils::join_lines_cow;

pub struct OperatorOverloadSpacingPolicy;

struct OperatorSpan {
    byte_start: usize,
    byte_end: usize,
    line: usize,
}

impl OperatorOverloadSpacingPolicy {
    pub fn new() -> Self {
        Self
    }

    const OPERATOR_QUERY: &str = r#"(operator_name) @op"#;

    fn collect_operator_spans(
        root: Node<'_>,
        text: &str,
        query_cache: Option<&TsQueryCache>,
    ) -> Vec<OperatorSpan> {
        let mut spans = Vec::new();

        ts_traversal::query_or_traverse(
            root,
            Self::OPERATOR_QUERY,
            query_cache,
            &[ts_cpp_symbols::sym_operator_name],
            text.as_bytes(),
            |node| {
                spans.push(OperatorSpan {
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    line: node.start_position().row,
                });
            },
        );

        spans
    }

    fn canonical_from_clang(
        context: &PolicyContext<'_>,
        line: usize,
    ) -> Option<String> {
        let semantic = context.semantic_file_context?;
        let decl = semantic.symbol_on_line_by_kinds(line, &[])
            .filter(|d| {
                crate::parser::clang_types::is_function_like_kind(d.kind)
            })?;
        if decl.name.starts_with("operator") {
            Some(decl.name.clone())
        } else {
            None
        }
    }

    fn canonical_from_text(node_text: &str) -> String {
        let Some(rest) = node_text.strip_prefix("operator") else {
            return node_text.to_string();
        };
        let trimmed = rest.trim_start();
        if trimmed.is_empty() {
            return node_text.to_string();
        }
        if trimmed.as_bytes()[0].is_ascii_alphanumeric() || trimmed.as_bytes()[0] == b'_' {
            format!("operator {trimmed}")
        } else {
            format!("operator{trimmed}")
        }
    }
}

impl Policy for OperatorOverloadSpacingPolicy {
    fn name(&self) -> &str {
        "operator_overload_spacing"
    }
    fn apply(&self, context: &PolicyContext<'_>) -> PolicyResult {
        let Some(tree) = context.tree_sitter_tree else {
            return PolicyResult::unchanged_with_warning(
                "operator_overload_spacing: tree-sitter context unavailable".to_string(),
            );
        };
        let semantic_query = context.semantic_query();
        if !semantic_query.is_available() {
            return PolicyResult::unchanged_with_warning(
                "operator_overload_spacing: semantic context unavailable; skipping heuristic edits"
                    .to_string(),
            );
        }

        let shared = context.shared.unwrap();
        let eol = shared.line_ending();
        let mut lines = shared.lines_cow();
        let trailing_newline = shared.trailing_newline();
        let mut edits = Vec::new();
        let mut skipped_semantic_unsafe = 0usize;

        let mut spans =
            Self::collect_operator_spans(tree.root_node(), context.text, context.query_cache);
        spans.sort_by(|a, b| a.byte_start.cmp(&b.byte_start));

        for span in &spans {
            if span.line >= lines.len() {
                continue;
            }
            if shared.is_macro_line(span.line + 1) {
                skipped_semantic_unsafe = skipped_semantic_unsafe.saturating_add(1);
                continue;
            }

            let node_text = &context.text[span.byte_start..span.byte_end];

            let canonical = Self::canonical_from_clang(context, span.line + 1)
                .unwrap_or_else(|| Self::canonical_from_text(node_text));

            if canonical != node_text {
                let original = lines[span.line].to_string();
                let line_start = context.text[..span.byte_start]
                    .rfind('\n')
                    .map(|p| p + 1)
                    .unwrap_or(0);
                let local_start = span.byte_start - line_start;
                let local_end = span.byte_end - line_start;

                let orig_bytes = original.as_bytes();
                if local_end <= orig_bytes.len() {
                    let mut updated = String::with_capacity(original.len());
                    updated.push_str(&original[..local_start]);
                    updated.push_str(&canonical);
                    updated.push_str(&original[local_end..]);

                    if updated != original {
                        lines[span.line] = Cow::Owned(updated.clone());
                        edits.push(Edit {
                            policy: self.name().into(),
                            line: span.line + 1,
                            before: original,
                            after: updated,
                        });
                    }
                }
            }
        }

        if edits.is_empty() {
            return PolicyResult::unchanged();
        }

        let mut warnings = Vec::new();
        if skipped_semantic_unsafe > 0 {
            warnings.push(format!(
                "operator_overload_spacing: skipped {} semantic-unsafe candidate line(s)",
                skipped_semantic_unsafe
            ));
        }

        PolicyResult {
            text: join_lines_cow(&lines, eol, trailing_newline),
            changed: true,
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
    use crate::policy::shared_data::PolicySharedData;

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
    fn normalizes_operator_spacing() {
        let policy = OperatorOverloadSpacingPolicy::new();
        let path = PathBuf::from("op.cpp");
        let source = "struct X { X operator +(const X& rhs) const; };\n";
        let tree = parse_cpp(source);
        let (_clang, semantic) = semantic_for(source, &path, &tree);
        let shared = PolicySharedData::new(source, None);
        let ctx = PolicyContext::new(source, &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic))
            .with_shared(Some(&shared));
        let result = policy.apply(&ctx);
        assert_eq!(
            result.text,
            "struct X { X operator+(const X& rhs) const; };\n"
        );
    }

    #[test]
    fn keeps_conversion_spacing() {
        let policy = OperatorOverloadSpacingPolicy::new();
        let path = PathBuf::from("op.cpp");
        let source = "struct X { explicit operator bool() const; };\n";
        let tree = parse_cpp(source);
        let (_clang, semantic) = semantic_for(source, &path, &tree);
        let shared = PolicySharedData::new(source, None);
        let ctx = PolicyContext::new(source, &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic))
            .with_shared(Some(&shared));
        let result = policy.apply(&ctx);
        assert!(!result.changed);
    }
}
