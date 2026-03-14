use std::collections::HashSet;

use tree_sitter::{Node, StreamingIterator};

use crate::model::edit::Edit;
use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::model::violation::Violation;
use crate::parser::node_kind;
use crate::parser::query_cache::TsQueryCache;
use crate::parser::ts_traversal;
use crate::policy::traits::Policy;
use crate::policy::text_utils::{detect_line_ending, join_lines, split_lines};

#[derive(Clone, Debug)]
struct BlockCandidate {
    kind: String,
    label: String,
    open_line: usize,
    close_line: usize,
}

pub struct NamespaceEndCommentsPolicy {
    blocks_filter: HashSet<String>,
    control_block_kinds: HashSet<String>,
    max_named_lines: usize,
    max_label_length: usize,
}

impl NamespaceEndCommentsPolicy {
    pub fn new(
        blocks: Vec<String>,
        control_block_kinds: Vec<String>,
        max_named_lines: usize,
        max_label_length: usize,
        replace_existing: bool,
    ) -> Self {
        let _ = replace_existing;
        Self {
            blocks_filter: blocks
                .into_iter()
                .map(|item| item.trim().to_lowercase())
                .filter(|item| !item.is_empty())
                .collect(),
            control_block_kinds: control_block_kinds
                .into_iter()
                .map(|item| item.trim().to_lowercase())
                .filter(|item| !item.is_empty())
                .collect(),
            max_named_lines: max_named_lines.max(1),
            max_label_length: max_label_length.max(8),
        }
    }

    fn canonical_kind(node_kind: &str) -> Option<&'static str> {
        match node_kind {
            node_kind::NAMESPACE_DEFINITION => Some("namespace"),
            node_kind::CLASS_SPECIFIER => Some("class"),
            node_kind::STRUCT_SPECIFIER => Some("struct"),
            node_kind::FUNCTION_DEFINITION => Some("function"),
            node_kind::IF_STATEMENT => Some("if"),
            node_kind::FOR_STATEMENT => Some("for"),
            node_kind::WHILE_STATEMENT => Some("while"),
            node_kind::SWITCH_STATEMENT => Some("switch"),
            node_kind::CATCH_CLAUSE => Some("catch"),
            _ => None,
        }
    }

    fn body_node(node: Node<'_>) -> Option<Node<'_>> {
        if let Some(body) = node.child_by_field_name("body") {
            return Some(body);
        }
        if let Some(consequence) = node.child_by_field_name("consequence") {
            return Some(consequence);
        }
        let body_kinds = [
            node_kind::COMPOUND_STATEMENT,
            node_kind::DECLARATION_LIST,
            node_kind::FIELD_DECLARATION_LIST,
        ];
        for idx in 0..node.child_count() {
            let Some(child) = node.child(idx as u32) else {
                continue;
            };
            if body_kinds.contains(&child.kind()) {
                return Some(child);
            }
        }
        None
    }

    fn node_text(node: Node<'_>, text: &str) -> Option<String> {
        node.utf8_text(text.as_bytes())
            .ok()
            .map(ToString::to_string)
            .map(|value| value.trim().to_string())
    }

    fn extract_named_label(
        &self,
        node: Node<'_>,
        canonical_kind: &str,
        text: &str,
    ) -> Option<String> {
        match canonical_kind {
            "namespace" => {
                let node = ts_traversal::first_descendant_excluding_root(
                    node,
                    &[node_kind::NAMESPACE_IDENTIFIER, node_kind::IDENTIFIER],
                    &[node_kind::DECLARATION_LIST],
                )?;
                let name = Self::node_text(node, text)?;
                (!name.is_empty()).then_some(format!("namespace {name}"))
            }
            "class" | "struct" => {
                let node = ts_traversal::first_descendant_excluding_root(
                    node,
                    &[node_kind::TYPE_IDENTIFIER],
                    &[node_kind::FIELD_DECLARATION_LIST],
                )?;
                let name = Self::node_text(node, text)?;
                (!name.is_empty()).then_some(format!("{canonical_kind} {name}"))
            }
            "function" => {
                let declarator = ts_traversal::first_descendant_excluding_root(
                    node,
                    &[node_kind::FUNCTION_DECLARATOR],
                    &[node_kind::COMPOUND_STATEMENT],
                )?;
                let name_node = ts_traversal::rightmost_descendant(
                    declarator,
                    &[node_kind::IDENTIFIER, node_kind::FIELD_IDENTIFIER, node_kind::TYPE_IDENTIFIER],
                    &[node_kind::PARAMETER_LIST, node_kind::TEMPLATE_PARAMETER_LIST],
                )?;
                let name = Self::node_text(name_node, text)?;
                let short = name.split("::").last().unwrap_or(name.as_str()).trim();
                (!short.is_empty()).then_some(format!("{short}(...)"))
            }
            _ => None,
        }
    }

    fn default_label(canonical_kind: &str) -> String {
        format!("{canonical_kind}(...)")
    }

    fn should_append_when_missing_comment(canonical_kind: &str) -> bool {
        matches!(canonical_kind, "namespace" | "class" | "struct")
    }

    fn select_label(
        &self,
        node: Node<'_>,
        canonical_kind: &str,
        text: &str,
        span_lines: usize,
    ) -> String {
        let default = Self::default_label(canonical_kind);
        let Some(named) = self.extract_named_label(node, canonical_kind, text) else {
            return default;
        };

        if !matches!(canonical_kind, "namespace" | "class" | "struct")
            && span_lines > self.max_named_lines
        {
            return default;
        }
        if named.len() > self.max_label_length
            && !matches!(canonical_kind, "namespace" | "class" | "struct")
        {
            return default;
        }
        named
    }

    fn include_block_kind(&self, canonical_kind: &str) -> bool {
        if self.blocks_filter.is_empty() {
            // Keep default behavior scoped to namespace blocks only.
            // Additional block kinds can be enabled explicitly via config.
            return canonical_kind == "namespace";
        }
        if !self.blocks_filter.contains(canonical_kind) {
            return false;
        }
        if matches!(canonical_kind, "if" | "for" | "while" | "switch" | "catch")
            && !self.control_block_kinds.is_empty()
            && !self.control_block_kinds.contains(canonical_kind)
        {
            return false;
        }
        true
    }

    const BLOCK_QUERY: &str = r#"[
        (namespace_definition) @block
        (class_specifier) @block
        (struct_specifier) @block
        (function_definition) @block
        (if_statement) @block
        (for_statement) @block
        (while_statement) @block
        (switch_statement) @block
        (catch_clause) @block
    ]"#;

    fn collect_blocks(
        &self,
        text: &str,
        root: Node<'_>,
        query_cache: Option<&TsQueryCache>,
    ) -> Vec<BlockCandidate> {
        let mut blocks = Vec::new();

        let process_node = |node: Node<'_>, blocks: &mut Vec<BlockCandidate>| {
            if let Some(canonical_kind) = Self::canonical_kind(node.kind()) {
                if self.include_block_kind(canonical_kind) {
                    if let Some(body) = Self::body_node(node) {
                        let open_line = node.start_position().row + 1;
                        let close_line = body.end_position().row + 1;
                        if close_line >= open_line {
                            let span_lines = close_line - open_line;
                            let label = self.select_label(node, canonical_kind, text, span_lines);
                            blocks.push(BlockCandidate {
                                kind: canonical_kind.to_string(),
                                label,
                                open_line,
                                close_line,
                            });
                        }
                    }
                }
            }
        };

        if let Some(query) = query_cache
            .and_then(|qc| qc.get_or_compile(Self::BLOCK_QUERY).ok())
        {
            let mut cursor = tree_sitter::QueryCursor::new();
            let mut matches = cursor.matches(&query, root, "".as_bytes());
            while let Some(m) = {
                matches.advance();
                matches.get()
            } {
                for capture in m.captures {
                    process_node(capture.node, &mut blocks);
                }
            }
        } else {
            let mut stack = vec![root];
            while let Some(node) = stack.pop() {
                process_node(node, &mut blocks);
                for idx in (0..node.child_count()).rev() {
                    if let Some(child) = node.child(idx as u32) {
                        stack.push(child);
                    }
                }
            }
        }

        blocks
    }

    fn line_edits(&self, before: &[String], after: &[String]) -> Vec<Edit> {
        let mut edits = Vec::new();
        let shared = before.len().min(after.len());
        for idx in 0..shared {
            if before[idx] == after[idx] {
                continue;
            }
            edits.push(Edit {
                policy: self.name().into(),
                line: idx + 1,
                before: before[idx].clone(),
                after: after[idx].clone(),
            });
        }
        edits
    }
}

impl Policy for NamespaceEndCommentsPolicy {
    fn name(&self) -> &str {
        "namespace_end_comments"
    }
    fn apply(&self, context: &PolicyContext<'_>) -> PolicyResult {
        let Some(tree) = &context.tree_sitter_tree else {
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits: Vec::new(),
                warnings: vec![
                    "namespace_end_comments: tree-sitter context unavailable".to_string()
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
                    "namespace_end_comments: semantic context unavailable; skipping heuristic edits"
                        .to_string(),
                ],
            };
        }

        let root = tree.root_node();
        let blocks = self.collect_blocks(context.text, root, context.query_cache);
        if blocks.is_empty() {
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits: Vec::new(),
                warnings: Vec::new(),
            };
        }

        let eol = detect_line_ending(context.text);
        let (mut lines, trailing_newline) = split_lines(context.text);
        let before_lines = lines.clone();
        let mut violations = Vec::new();
        let mut skipped_semantic_unsafe = 0usize;

        for block in blocks {
            if block.close_line == 0 || block.close_line > lines.len() {
                continue;
            }
            let line_idx = block.close_line - 1;
            if semantic_query.is_available() && semantic_query.is_macro_region(line_idx + 1, 1) {
                skipped_semantic_unsafe = skipped_semantic_unsafe.saturating_add(1);
                continue;
            }
            let original_line = lines[line_idx].clone();
            if !original_line.contains('}') {
                continue;
            }

            let expected = format!("// end {}", block.label);
            let updated = if original_line.contains("//") {
                original_line.clone()
            } else {
                if !Self::should_append_when_missing_comment(block.kind.as_str()) {
                    continue;
                }
                format!("{} {}", original_line.trim_end(), expected)
            };

            if updated != original_line {
                lines[line_idx] = updated;
                violations.push(Violation {
                    policy: self.name().into(),
                    message: format!(
                        "end comment normalized for {} block (opened at line {})",
                        block.kind, block.open_line
                    ),
                    line: line_idx + 1,
                    column: Some(1),
                });
            }
        }

        if lines == before_lines {
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits: Vec::new(),
                warnings: Vec::new(),
            };
        }

        let edits = self.line_edits(before_lines.as_slice(), lines.as_slice());
        let mut warnings = Vec::new();
        if skipped_semantic_unsafe > 0 {
            warnings.push(format!(
                "namespace_end_comments: skipped {} semantic-unsafe candidate line(s)",
                skipped_semantic_unsafe
            ));
        }
        PolicyResult {
            text: join_lines(&lines, eol, trailing_newline),
            violations,
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
            .expect("set cpp language");
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
    fn adds_namespace_end_comment() {
        let policy = NamespaceEndCommentsPolicy::new(Vec::new(), Vec::new(), 40, 48, true);
        let text = "namespace demo {\nint x = 1;\n}\n".to_string();
        let tree = parse_cpp(&text);
        let path = PathBuf::from("a.cpp");
        let (clang, semantic) = semantic_for(text.as_str(), &path, &tree);
        let ctx = PolicyContext::new(text.as_str(), &path)
            .with_tree_sitter_tree(Some(&tree))
            .with_clang_parse_result(Some(&*clang))
            .with_semantic_file_context(Some(&semantic));
        let result = policy.apply(&ctx);
        assert!(result.text.contains("} // end namespace demo"));
    }

    #[test]
    fn preserves_existing_comment_when_present() {
        let policy = NamespaceEndCommentsPolicy::new(Vec::new(), Vec::new(), 40, 48, true);
        let text = "namespace demo {\nint x = 1;\n} // wrong\n".to_string();
        let tree = parse_cpp(&text);
        let path = PathBuf::from("a.cpp");
        let (clang, semantic) = semantic_for(text.as_str(), &path, &tree);
        let ctx = PolicyContext::new(text.as_str(), &path)
            .with_tree_sitter_tree(Some(&tree))
            .with_clang_parse_result(Some(&*clang))
            .with_semantic_file_context(Some(&semantic));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, text);
    }

    #[test]
    fn preserves_detailed_function_end_comment() {
        let policy = NamespaceEndCommentsPolicy::new(Vec::new(), Vec::new(), 40, 48, true);
        let text = "int f(void) {\nreturn 0;\n} // end f(void)\n".to_string();
        let tree = parse_cpp(&text);
        let path = PathBuf::from("a.cpp");
        let (clang, semantic) = semantic_for(text.as_str(), &path, &tree);
        let ctx = PolicyContext::new(text.as_str(), &path)
            .with_tree_sitter_tree(Some(&tree))
            .with_clang_parse_result(Some(&*clang))
            .with_semantic_file_context(Some(&semantic));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, text);
    }

    #[test]
    fn does_not_add_generic_function_comment_when_missing() {
        let policy = NamespaceEndCommentsPolicy::new(Vec::new(), Vec::new(), 40, 48, true);
        let text = "int f(void) {\nreturn 0;\n}\n".to_string();
        let tree = parse_cpp(&text);
        let path = PathBuf::from("a.cpp");
        let (clang, semantic) = semantic_for(text.as_str(), &path, &tree);
        let ctx = PolicyContext::new(text.as_str(), &path)
            .with_tree_sitter_tree(Some(&tree))
            .with_clang_parse_result(Some(&*clang))
            .with_semantic_file_context(Some(&semantic));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, text);
    }

    #[test]
    fn default_filter_skips_struct_end_comments() {
        let policy = NamespaceEndCommentsPolicy::new(Vec::new(), Vec::new(), 40, 48, true);
        let text = "struct Slot {\nint value;\n};\n".to_string();
        let tree = parse_cpp(&text);
        let path = PathBuf::from("a.cpp");
        let (clang, semantic) = semantic_for(text.as_str(), &path, &tree);
        let ctx = PolicyContext::new(text.as_str(), &path)
            .with_tree_sitter_tree(Some(&tree))
            .with_clang_parse_result(Some(&*clang))
            .with_semantic_file_context(Some(&semantic));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, text);
    }
}
