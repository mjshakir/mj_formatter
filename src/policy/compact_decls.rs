use rustc_hash::FxHashMap;
use tree_sitter::Node;

use crate::model::edit::Edit;
use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::model::violation::Violation;
use crate::parser::node_kind;
use crate::parser::ts_traversal;
use crate::parser::query_cache::TsQueryCache;
use crate::policy::Policy;
use std::borrow::Cow;

use crate::policy::text_utils::join_lines_cow;

#[derive(Clone, Debug)]
struct CandidateDecl {
    indent: String,
    type_prefix: String,
    name: String,
}

pub struct CompactDeclarationsPolicy {
    min_group_size: usize,
}

impl CompactDeclarationsPolicy {
    pub fn new(min_group_size: usize) -> Self {
        Self {
            min_group_size: min_group_size.max(2),
        }
    }

    const DECL_QUERY: &str = "(declaration) @decl";

    fn collect_decl_nodes<'a>(
        root: Node<'a>,
        query_cache: Option<&TsQueryCache>,
    ) -> Vec<Node<'a>> {
        ts_traversal::query_or_traverse_collect(
            root,
            Self::DECL_QUERY,
            query_cache,
            &[node_kind::DECLARATION],
        )
    }

    fn candidate_from_node(node: &Node<'_>, source: &[u8]) -> Option<(usize, CandidateDecl)> {
        if node.start_position().row != node.end_position().row {
            return None;
        }
        let declarator = node.child_by_field_name("declarator")?;
        if declarator.kind() != node_kind::IDENTIFIER {
            return None;
        }
        let type_node = node.child_by_field_name("type")?;
        let type_text = type_node.utf8_text(source).ok()?;
        if type_text.is_empty() {
            return None;
        }
        let mut type_prefix_parts: Vec<&str> = Vec::new();
        for i in 0..node.child_count() {
            let Some(child) = node.child(i as u32) else {
                continue;
            };
            if child.id() == declarator.id() || child.kind() == ";" {
                break;
            }
            if child.kind() == "type_qualifier" || child.kind() == "storage_class_specifier" {
                if let Ok(text) = child.utf8_text(source) {
                    type_prefix_parts.push(text);
                }
            } else if child.id() == type_node.id() {
                type_prefix_parts.push(type_text);
            }
        }
        let type_prefix = type_prefix_parts.join(" ");
        if type_prefix.is_empty() {
            return None;
        }
        let name = declarator.utf8_text(source).ok()?.to_string();
        if name.is_empty() {
            return None;
        }
        let line_start = node.start_byte();
        let type_start = if let Some(first_child) = node.child(0) {
            first_child.start_byte()
        } else {
            line_start
        };
        let indent_bytes = &source[line_start..type_start];
        let indent = std::str::from_utf8(indent_bytes).unwrap_or("").to_string();

        let row = node.start_position().row;
        Some((row, CandidateDecl {
            indent,
            type_prefix,
            name,
        }))
    }

    fn build_candidate_map(
        root: Node<'_>,
        source: &[u8],
        query_cache: Option<&TsQueryCache>,
    ) -> FxHashMap<usize, CandidateDecl> {
        let decl_nodes = Self::collect_decl_nodes(root, query_cache);
        let mut map = FxHashMap::default();
        for node in &decl_nodes {
            if let Some((row, candidate)) = Self::candidate_from_node(node, source) {
                map.insert(row, candidate);
            }
        }
        map
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
        if before.len() == after.len() {
            return edits;
        }

        let tail = before.len().max(after.len());
        for idx in shared..tail {
            edits.push(Edit {
                policy: self.name().into(),
                line: idx + 1,
                before: before.get(idx).map(|c| c.to_string()).unwrap_or_default(),
                after: after.get(idx).map(|c| c.to_string()).unwrap_or_default(),
            });
        }
        edits
    }
}

impl Policy for CompactDeclarationsPolicy {
    fn name(&self) -> &str {
        "compact_declarations"
    }

    fn apply(&self, context: &PolicyContext<'_>) -> PolicyResult {
        let semantic_query = context.semantic_query();
        if !semantic_query.is_available() {
            return PolicyResult::unchanged_with_warning(
                "compact_declarations: semantic context unavailable; skipping heuristic edits"
                    .to_string(),
            );
        }
        let Some(tree) = context.tree_sitter_tree else {
            return PolicyResult::unchanged_with_warning(
                "compact_declarations: tree-sitter context unavailable".to_string(),
            );
        };
        let shared = context.shared.unwrap();
        let eol = shared.line_ending();
        let before_lines = shared.lines_cow();
        let trailing_newline = shared.trailing_newline();
        if before_lines.len() < self.min_group_size {
            return PolicyResult::unchanged();
        }

        let source = context.text.as_bytes();
        let candidates = Self::build_candidate_map(tree.root_node(), source, context.query_cache);

        let mut after_lines: Vec<Cow<'_, str>> = Vec::with_capacity(before_lines.len());
        let mut violations = Vec::new();
        let mut warnings = Vec::new();
        let mut skipped_semantic_unsafe = 0usize;
        let mut idx = 0usize;

        while idx < before_lines.len() {
            let Some(first) = candidates.get(&idx).cloned() else {
                after_lines.push(before_lines[idx].clone());
                idx += 1;
                continue;
            };

            let mut group = vec![first];
            let mut group_end = idx + 1;
            while group_end < before_lines.len() {
                let Some(next) = candidates.get(&group_end).cloned() else {
                    break;
                };
                if next.indent != group[0].indent || next.type_prefix != group[0].type_prefix {
                    break;
                }
                group.push(next);
                group_end += 1;
            }

            if group.len() >= self.min_group_size {
                let group_safe = (idx..group_end)
                    .all(|line_idx| !shared.is_macro_line(line_idx + 1));
                if !group_safe {
                    skipped_semantic_unsafe = skipped_semantic_unsafe.saturating_add(group.len());
                    after_lines.push(before_lines[idx].clone());
                    idx += 1;
                    continue;
                }
                let names: Vec<String> = group.iter().map(|item| item.name.clone()).collect();
                after_lines.push(Cow::Owned(format!(
                    "{}{} {};",
                    group[0].indent,
                    group[0].type_prefix,
                    names.join(", ")
                )));
                violations.push(Violation {
                    policy: self.name().into(),
                    message: "Compacted adjacent same-type declarations".to_string(),
                    line: idx + 1,
                    column: Some(1),
                });
                idx = group_end;
                continue;
            }

            after_lines.push(before_lines[idx].clone());
            idx += 1;
        }

        if before_lines == after_lines {
            return PolicyResult::unchanged();
        }

        let edits = self.line_edits(before_lines.as_slice(), after_lines.as_slice());
        if skipped_semantic_unsafe > 0 {
            warnings.push(format!(
                "compact_declarations: skipped {} semantic-unsafe declaration line(s)",
                skipped_semantic_unsafe
            ));
        }
        PolicyResult {
            text: join_lines_cow(&after_lines, eol, trailing_newline),
            changed: true,
            violations,
            edits,
            warnings,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tree_sitter::Parser;

    use super::*;
    use crate::model::policy_context::PolicyContext;
    use crate::parser::file_context::SemanticFileContext;
    use crate::policy::shared_data::PolicySharedData;

    fn parse_cpp(text: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("cpp language");
        parser.parse(text, None).expect("parse tree")
    }

    #[test]
    fn compacts_adjacent_declarations() {
        let policy = CompactDeclarationsPolicy::new(3);
        let text = "int a;\nint b;\nint c;\n".to_string();
        let tree = parse_cpp(&text);
        let path = PathBuf::from("sample.cpp");
        let semantic = SemanticFileContext::default();
        let shared = PolicySharedData::new(text.as_str(), None);
        let ctx = PolicyContext::new(text.as_str(), &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic))
            .with_shared(Some(&shared));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "int a, b, c;\n");
        assert_eq!(result.violations.len(), 1);
    }
}
