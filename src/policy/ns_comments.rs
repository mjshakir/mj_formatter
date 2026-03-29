use rustc_hash::FxHashSet;

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

#[derive(Clone, Debug)]
struct BlockCandidate {
    kind: String,
    label: String,
    open_line: usize,
    close_line: usize,
}

pub struct NsCommentsPolicy {
    blocks_filter: FxHashSet<u16>,
    control_block_kinds: FxHashSet<u16>,
    max_named_lines: usize,
    max_label_length: usize,
}

impl NsCommentsPolicy {
    fn config_name_to_kind_id(name: &str) -> Option<u16> {
        match name {
            "namespace" => Some(ts_cpp_symbols::sym_namespace_definition),
            "class" => Some(ts_cpp_symbols::sym_class_specifier),
            "struct" => Some(ts_cpp_symbols::sym_struct_specifier),
            "function" => Some(ts_cpp_symbols::sym_function_definition),
            "if" => Some(ts_cpp_symbols::sym_if_statement),
            "for" => Some(ts_cpp_symbols::sym_for_statement),
            "while" => Some(ts_cpp_symbols::sym_while_statement),
            "switch" => Some(ts_cpp_symbols::sym_switch_statement),
            "catch" => Some(ts_cpp_symbols::sym_catch_clause),
            _ => None,
        }
    }

    const CONTROL_FLOW_KINDS: &[u16] = &[
        ts_cpp_symbols::sym_if_statement,
        ts_cpp_symbols::sym_for_statement,
        ts_cpp_symbols::sym_while_statement,
        ts_cpp_symbols::sym_switch_statement,
        ts_cpp_symbols::sym_catch_clause,
    ];

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
                .filter_map(|item| Self::config_name_to_kind_id(item.trim().to_lowercase().as_str()))
                .collect(),
            control_block_kinds: control_block_kinds
                .into_iter()
                .filter_map(|item| Self::config_name_to_kind_id(item.trim().to_lowercase().as_str()))
                .collect(),
            max_named_lines: max_named_lines.max(1),
            max_label_length: max_label_length.max(8),
        }
    }

    fn canonical_kind(kind_id: u16) -> Option<&'static str> {
        match kind_id {
            ts_cpp_symbols::sym_namespace_definition => Some("namespace"),
            ts_cpp_symbols::sym_class_specifier => Some("class"),
            ts_cpp_symbols::sym_struct_specifier => Some("struct"),
            ts_cpp_symbols::sym_function_definition => Some("function"),
            ts_cpp_symbols::sym_if_statement => Some("if"),
            ts_cpp_symbols::sym_for_statement => Some("for"),
            ts_cpp_symbols::sym_while_statement => Some("while"),
            ts_cpp_symbols::sym_switch_statement => Some("switch"),
            ts_cpp_symbols::sym_catch_clause => Some("catch"),
            _ => None,
        }
    }

    fn body_node(node: Node<'_>) -> Option<Node<'_>> {
        if let Some(body) = node.child_by_field_id(ts_cpp_symbols::field_body) {
            return Some(body);
        }
        if let Some(consequence) = node.child_by_field_id(ts_cpp_symbols::field_consequence) {
            return Some(consequence);
        }
        for idx in 0..node.named_child_count() {
            let Some(child) = node.named_child(idx as u32) else {
                continue;
            };
            if ts_cpp_symbols::is_compound_body(child.kind_id()) {
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
                    &[ts_cpp_symbols::alias_sym_namespace_identifier, ts_cpp_symbols::sym_identifier],
                    &[ts_cpp_symbols::sym_declaration_list],
                )?;
                let name = Self::node_text(node, text)?;
                (!name.is_empty()).then_some(format!("namespace {name}"))
            }
            "class" | "struct" => {
                let node = ts_traversal::first_descendant_excluding_root(
                    node,
                    &[ts_cpp_symbols::alias_sym_type_identifier],
                    &[ts_cpp_symbols::sym_field_declaration_list],
                )?;
                let name = Self::node_text(node, text)?;
                (!name.is_empty()).then_some(format!("{canonical_kind} {name}"))
            }
            "function" => {
                let declarator = ts_traversal::first_descendant_excluding_root(
                    node,
                    &[ts_cpp_symbols::sym_function_declarator],
                    &[ts_cpp_symbols::sym_compound_statement],
                )?;
                let name_node = ts_traversal::rightmost_descendant(
                    declarator,
                    &[ts_cpp_symbols::sym_identifier, ts_cpp_symbols::alias_sym_field_identifier, ts_cpp_symbols::alias_sym_type_identifier],
                    &[ts_cpp_symbols::sym_parameter_list, ts_cpp_symbols::sym_template_parameter_list],
                )?;
                let name = Self::node_text(name_node, text)?;
                let short = name.trim();
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

    fn include_block_kind(&self, kind_id: u16) -> bool {
        if self.blocks_filter.is_empty() {
            return kind_id == ts_cpp_symbols::sym_namespace_definition;
        }
        if !self.blocks_filter.contains(&kind_id) {
            return false;
        }
        if Self::CONTROL_FLOW_KINDS.contains(&kind_id)
            && !self.control_block_kinds.is_empty()
            && !self.control_block_kinds.contains(&kind_id)
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
            if let Some(canonical_kind) = Self::canonical_kind(node.kind_id()) {
                if self.include_block_kind(node.kind_id()) {
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

        ts_traversal::query_or_traverse(
            root,
            Self::BLOCK_QUERY,
            query_cache,
            &[
                ts_cpp_symbols::sym_namespace_definition,
                ts_cpp_symbols::sym_class_specifier,
                ts_cpp_symbols::sym_struct_specifier,
                ts_cpp_symbols::sym_function_definition,
                ts_cpp_symbols::sym_if_statement,
                ts_cpp_symbols::sym_for_statement,
                ts_cpp_symbols::sym_while_statement,
                ts_cpp_symbols::sym_switch_statement,
                ts_cpp_symbols::sym_catch_clause,
            ],
            text.as_bytes(),
            |node| {
                process_node(node, &mut blocks);
            },
        );

        blocks
    }

    fn line_edits(&self, before: &[Cow<'_, str>], after: &[Cow<'_, str>]) -> Vec<Edit> {
        let mut edits = Vec::new();
        let shared = before.len().min(after.len());
        for idx in 0..shared {
            if before[idx] == after[idx] {
                continue;
            }
            edits.push(Edit {
                policy: self.name().into(),
                line: idx + 1,
                before: before[idx].to_string(),
                after: after[idx].to_string(),
            });
        }
        edits
    }
}

impl Policy for NsCommentsPolicy {
    fn name(&self) -> &str {
        "namespace_end_comments"
    }
    fn apply(&self, context: &PolicyContext<'_>) -> PolicyResult {
        let Some(tree) = &context.tree_sitter_tree else {
            return PolicyResult::unchanged_with_warning(
                "namespace_end_comments: tree-sitter context unavailable".to_string(),
            );
        };
        let semantic_query = context.semantic_query();
        if !semantic_query.is_available() {
            return PolicyResult::unchanged_with_warning(
                "namespace_end_comments: semantic context unavailable; skipping heuristic edits"
                    .to_string(),
            );
        }

        let root = tree.root_node();
        let blocks = self.collect_blocks(context.text, root, context.query_cache);
        if blocks.is_empty() {
            return PolicyResult::unchanged();
        }

        let shared = context.shared.unwrap();
        let eol = shared.line_ending();
        let mut lines = shared.lines_cow();
        let trailing_newline = shared.trailing_newline();
        let before_lines: Vec<Cow<'_, str>> = lines.clone();
        let mut violations = Vec::new();
        let mut skipped_semantic_unsafe = 0usize;

        for block in blocks {
            if block.close_line == 0 || block.close_line > lines.len() {
                continue;
            }
            let line_idx = block.close_line - 1;
            if shared.is_macro_line(line_idx + 1) {
                skipped_semantic_unsafe = skipped_semantic_unsafe.saturating_add(1);
                continue;
            }
            let original_line = lines[line_idx].to_string();
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
                lines[line_idx] = Cow::Owned(updated);
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
            return PolicyResult::unchanged();
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
            text: join_lines_cow(&lines, eol, trailing_newline),
            changed: true,
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
    use crate::policy::shared_data::PolicySharedData;

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
    fn adds_namespace_comment() {
        let policy = NsCommentsPolicy::new(Vec::new(), Vec::new(), 40, 48, true);
        let text = "namespace demo {\nint x = 1;\n}\n".to_string();
        let tree = parse_cpp(&text);
        let path = PathBuf::from("a.cpp");
        let (_clang, semantic) = semantic_for(text.as_str(), &path, &tree);
        let shared = PolicySharedData::new(text.as_str(), None);
        let ctx = PolicyContext::new(text.as_str(), &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic))
            .with_shared(Some(&shared));
        let result = policy.apply(&ctx);
        assert!(result.text.contains("} // end namespace demo"));
    }

    #[test]
    fn preserves_existing_comment() {
        let policy = NsCommentsPolicy::new(Vec::new(), Vec::new(), 40, 48, true);
        let text = "namespace demo {\nint x = 1;\n} // wrong\n".to_string();
        let tree = parse_cpp(&text);
        let path = PathBuf::from("a.cpp");
        let (_clang, semantic) = semantic_for(text.as_str(), &path, &tree);
        let shared = PolicySharedData::new(text.as_str(), None);
        let ctx = PolicyContext::new(text.as_str(), &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic))
            .with_shared(Some(&shared));
        let result = policy.apply(&ctx);
        assert!(!result.changed);
    }

    #[test]
    fn preserves_detailed_comment() {
        let policy = NsCommentsPolicy::new(Vec::new(), Vec::new(), 40, 48, true);
        let text = "int f(void) {\nreturn 0;\n} // end f(void)\n".to_string();
        let tree = parse_cpp(&text);
        let path = PathBuf::from("a.cpp");
        let (_clang, semantic) = semantic_for(text.as_str(), &path, &tree);
        let shared = PolicySharedData::new(text.as_str(), None);
        let ctx = PolicyContext::new(text.as_str(), &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic))
            .with_shared(Some(&shared));
        let result = policy.apply(&ctx);
        assert!(!result.changed);
    }

    #[test]
    fn skips_generic_comment() {
        let policy = NsCommentsPolicy::new(Vec::new(), Vec::new(), 40, 48, true);
        let text = "int f(void) {\nreturn 0;\n}\n".to_string();
        let tree = parse_cpp(&text);
        let path = PathBuf::from("a.cpp");
        let (_clang, semantic) = semantic_for(text.as_str(), &path, &tree);
        let shared = PolicySharedData::new(text.as_str(), None);
        let ctx = PolicyContext::new(text.as_str(), &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic))
            .with_shared(Some(&shared));
        let result = policy.apply(&ctx);
        assert!(!result.changed);
    }

    #[test]
    fn skips_struct_comments() {
        let policy = NsCommentsPolicy::new(Vec::new(), Vec::new(), 40, 48, true);
        let text = "struct Slot {\nint value;\n};\n".to_string();
        let tree = parse_cpp(&text);
        let path = PathBuf::from("a.cpp");
        let (_clang, semantic) = semantic_for(text.as_str(), &path, &tree);
        let shared = PolicySharedData::new(text.as_str(), None);
        let ctx = PolicyContext::new(text.as_str(), &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic))
            .with_shared(Some(&shared));
        let result = policy.apply(&ctx);
        assert!(!result.changed);
    }
}
