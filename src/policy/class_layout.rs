use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use tree_sitter::Node;

use crate::model::edit::Edit;
use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::model::violation::Violation;
use crate::parser::manager::ParserManager;
use crate::parser::node_kind;
use crate::parser::query_cache::TsQueryCache;
use crate::parser::ts_traversal;
use crate::policy::Policy;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum AccessLevel {
    Public,
    Protected,
    Private,
}

impl AccessLevel {
    fn from_text(text: &str) -> Option<Self> {
        match text.trim().trim_end_matches(':') {
            "public" => Some(Self::Public),
            "protected" => Some(Self::Protected),
            "private" => Some(Self::Private),
            _ => None,
        }
    }

    fn section_comment(&self) -> &'static str {
        match self {
            Self::Public => "// ── Public Implementation ──",
            Self::Protected => "// ── Protected Implementation ──",
            Self::Private => "// ── Private Implementation ──",
        }
    }
}

#[derive(Clone, Debug)]
struct SourceBlock {
    full_name: String,
    start: usize,
    end: usize,
    start_line: usize,
    end_line: usize,
    text: String,
}

#[derive(Clone, Debug)]
struct HeaderMethodEntry {
    full_name: String,
    access: AccessLevel,
}

pub struct ClassLayoutPolicy {
    source_extensions: HashSet<String>,
    header_extensions: Vec<String>,
    header_search_roots: Vec<String>,
    parser_manager: ParserManager,
}

impl ClassLayoutPolicy {
    pub fn new(
        source_extensions: Vec<String>,
        header_extensions: Vec<String>,
        header_search_roots: Vec<String>,
    ) -> Self {
        let source_extensions = if source_extensions.is_empty() {
            [".cpp", ".cc", ".cxx"]
                .into_iter()
                .map(ToString::to_string)
                .collect()
        } else {
            source_extensions
                .into_iter()
                .map(|value| value.to_lowercase())
                .collect()
        };
        let header_extensions = if header_extensions.is_empty() {
            vec![
                ".hpp".to_string(),
                ".h".to_string(),
                ".hh".to_string(),
                ".hxx".to_string(),
            ]
        } else {
            header_extensions
                .into_iter()
                .map(|value| value.to_lowercase())
                .collect()
        };
        let header_search_roots = if header_search_roots.is_empty() {
            vec!["include".to_string()]
        } else {
            header_search_roots
        };

        Self {
            source_extensions,
            header_extensions,
            header_search_roots,
            parser_manager: ParserManager::new(),
        }
    }

    fn path_extension(path: &str) -> String {
        Path::new(path)
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| format!(".{}", value.to_lowercase()))
            .unwrap_or_default()
    }

    fn is_source_file(&self, path: &str) -> bool {
        self.source_extensions
            .contains(Self::path_extension(path).as_str())
    }

    fn find_header(&self, source_path: &Path) -> Option<PathBuf> {
        let stem = source_path.file_stem()?.to_str()?;
        let mut candidates = Vec::new();
        for ext in &self.header_extensions {
            candidates.push(source_path.with_extension(ext.trim_start_matches('.')));
        }

        let cwd = std::env::current_dir().ok();
        for root in &self.header_search_roots {
            for ext in &self.header_extensions {
                let filename = format!("{stem}{ext}");
                let mut path = PathBuf::from(root);
                path.push(&filename);
                candidates.push(path.clone());
                if let Some(cwd) = &cwd {
                    candidates.push(cwd.join(path));
                }
            }
        }

        candidates.into_iter().find(|path| path.exists())
    }

    fn node_text(node: Node<'_>, text: &str) -> Option<String> {
        node.utf8_text(text.as_bytes())
            .ok()
            .map(|value| value.trim().to_string())
    }

    fn extract_method_name_from_declarator(node: Node<'_>, text: &str) -> Option<String> {
        let name_node = ts_traversal::rightmost_descendant(
            node,
            &[
                node_kind::IDENTIFIER,
                node_kind::FIELD_IDENTIFIER,
                node_kind::TYPE_IDENTIFIER,
                node_kind::DESTRUCTOR_NAME,
            ],
            &[node_kind::PARAMETER_LIST, node_kind::TEMPLATE_PARAMETER_LIST],
        )?;
        let name = Self::node_text(name_node, text)?;
        let short = name.split("::").last().unwrap_or(name.as_str()).trim();
        (!short.is_empty()).then_some(short.to_string())
    }

    const CLASS_QUERY: &str = r#"[
        (class_specifier) @cls
        (struct_specifier) @cls
    ]"#;

    fn extract_header_order(
        &self,
        text: &str,
        root: Node<'_>,
        query_cache: Option<&TsQueryCache>,
    ) -> Vec<HeaderMethodEntry> {
        let mut order = Vec::new();
        let class_nodes = Self::collect_class_nodes(root, query_cache);

        for node in &class_nodes {
            let node = *node;
            let Some(class_name_node) =
                ts_traversal::first_descendant_excluding_root(
                    node,
                    &[node_kind::TYPE_IDENTIFIER],
                    &[node_kind::FIELD_DECLARATION_LIST],
                )
            else {
                continue;
            };
            let Some(class_name) = Self::node_text(class_name_node, text) else {
                continue;
            };
            let Some(body) =
                ts_traversal::first_child_by_kind(node, node_kind::FIELD_DECLARATION_LIST)
            else {
                continue;
            };

            let is_struct = node.kind() == node_kind::STRUCT_SPECIFIER;
            let mut current_access = if is_struct {
                AccessLevel::Public
            } else {
                AccessLevel::Private
            };

            for idx in 0..body.child_count() {
                let Some(child) = body.child(idx as u32) else {
                    continue;
                };
                if child.kind() == node_kind::ACCESS_SPECIFIER {
                    if let Some(spec_text) = Self::node_text(child, text) {
                        if let Some(level) = AccessLevel::from_text(&spec_text) {
                            current_access = level;
                        }
                    }
                    continue;
                }
                let mut child_stack = vec![child];
                while let Some(walk_node) = child_stack.pop() {
                    if walk_node.kind() == node_kind::FUNCTION_DECLARATOR {
                        if let Some(method_name) =
                            Self::extract_method_name_from_declarator(walk_node, text)
                        {
                            order.push(HeaderMethodEntry {
                                full_name: format!("{class_name}::{method_name}"),
                                access: current_access,
                            });
                        }
                    }
                    for cidx in (0..walk_node.child_count()).rev() {
                        if let Some(grandchild) = walk_node.child(cidx as u32) {
                            child_stack.push(grandchild);
                        }
                    }
                }
            }
        }

        order
    }

    fn collect_class_nodes<'a>(
        root: Node<'a>,
        query_cache: Option<&TsQueryCache>,
    ) -> Vec<Node<'a>> {
        ts_traversal::query_or_traverse_collect(
            root,
            Self::CLASS_QUERY,
            query_cache,
            &[node_kind::CLASS_SPECIFIER, node_kind::STRUCT_SPECIFIER],
        )
    }

    fn extract_source_function_name(node: Node<'_>, text: &str) -> Option<String> {
        let declarator =
            ts_traversal::first_descendant_excluding_root(node, &[node_kind::FUNCTION_DECLARATOR], &[node_kind::COMPOUND_STATEMENT])?;
        let name_node = ts_traversal::rightmost_descendant(
            declarator,
            &[
                node_kind::IDENTIFIER,
                node_kind::FIELD_IDENTIFIER,
                node_kind::TYPE_IDENTIFIER,
                node_kind::DESTRUCTOR_NAME,
            ],
            &[node_kind::PARAMETER_LIST, node_kind::TEMPLATE_PARAMETER_LIST],
        )?;
        let full_name = Self::node_text(name_node, text)?;
        full_name.contains("::").then_some(full_name)
    }

    fn extract_source_blocks(
        &self,
        text: &str,
        root: Node<'_>,
        query_cache: Option<&TsQueryCache>,
    ) -> Vec<SourceBlock> {
        let mut blocks = Vec::new();

        let func_nodes = Self::collect_function_definitions(root, query_cache);

        for node in &func_nodes {
            let node = *node;
            if let Some(full_name) = Self::extract_source_function_name(node, text) {
                let start = node.start_byte();
                let end = node.end_byte();
                if end > start && end <= text.len() {
                    let block_text = text[start..end].to_string();
                    blocks.push(SourceBlock {
                        full_name,
                        start,
                        end,
                        start_line: node.start_position().row + 1,
                        end_line: node.end_position().row + 1,
                        text: block_text,
                    });
                }
            }
        }

        blocks.sort_by_key(|item| item.start);
        blocks
    }

    fn collect_function_definitions<'a>(
        root: Node<'a>,
        query_cache: Option<&TsQueryCache>,
    ) -> Vec<Node<'a>> {
        ts_traversal::query_or_traverse_collect(
            root,
            "(function_definition) @func",
            query_cache,
            &[node_kind::FUNCTION_DEFINITION],
        )
    }
}

impl Policy for ClassLayoutPolicy {
    fn name(&self) -> &str {
        "class_layout"
    }
    fn apply(&self, context: &PolicyContext<'_>) -> PolicyResult {
        if !self.is_source_file(context.path_str()) {
            return PolicyResult::unchanged();
        }
        let semantic_query = context.semantic_query();
        if !semantic_query.is_available() {
            return PolicyResult::unchanged_with_warning(
                "class_layout: semantic context unavailable; skipping heuristic reordering"
                    .to_string(),
            );
        }

        let Some(source_tree) = &context.tree_sitter_tree else {
            return PolicyResult::unchanged_with_warning("class_layout: tree-sitter context unavailable".to_string());
        };

        let Some(header_path) = self.find_header(context.path) else {
            return PolicyResult::unchanged();
        };

        let header_text = match fs::read_to_string(&header_path) {
            Ok(text) => text,
            Err(err) => {
                return PolicyResult::unchanged_with_warning(format!("class_layout: failed to read header: {err}"));
            }
        };

        let header_tree = match self
            .parser_manager
            .parse_tree_sitter(&header_text, &header_path)
        {
            Ok(tree) => tree,
            Err(err) => {
                return PolicyResult::unchanged_with_warning(format!("class_layout: header parse failed: {err}"));
            }
        };

        let header_order =
            self.extract_header_order(&header_text, header_tree.root_node(), context.query_cache);
        if header_order.is_empty() {
            return PolicyResult::unchanged();
        }

        let source_blocks = self.extract_source_blocks(
            context.text,
            source_tree.root_node(),
            context.query_cache,
        );
        if source_blocks.len() < 2 {
            return PolicyResult::unchanged();
        }

        let order_set: HashSet<&str> = header_order
            .iter()
            .map(|entry| entry.full_name.as_str())
            .collect();
        let access_map: HashMap<&str, AccessLevel> = header_order
            .iter()
            .map(|entry| (entry.full_name.as_str(), entry.access))
            .collect();
        let mut candidates: Vec<SourceBlock> = source_blocks
            .into_iter()
            .filter(|item| order_set.contains(item.full_name.as_str()))
            .collect();
        if candidates.len() < 2 {
            return PolicyResult::unchanged();
        }
        let shared = context.shared.unwrap();
        if candidates.iter().any(|item| {
            (item.start_line..=item.end_line)
                .any(|line| shared.is_macro_line(line))
        })
        {
            return PolicyResult::unchanged_with_warning(
                "class_layout: skipped macro-region method block(s)".to_string(),
            );
        }
        candidates.sort_by_key(|item| item.start);

        let mut by_name: HashMap<String, VecDeque<SourceBlock>> = HashMap::new();
        for block in &candidates {
            by_name
                .entry(block.full_name.clone())
                .or_default()
                .push_back(block.clone());
        }

        let mut ordered_blocks: Vec<(SourceBlock, AccessLevel)> = Vec::new();
        for entry in &header_order {
            let Some(queue) = by_name.get_mut(&entry.full_name) else {
                continue;
            };
            if let Some(block) = queue.pop_front() {
                ordered_blocks.push((block, entry.access));
            }
        }

        if ordered_blocks.is_empty() {
            return PolicyResult::unchanged();
        }

        let consumed: HashSet<(usize, usize)> = ordered_blocks
            .iter()
            .map(|(item, _)| (item.start, item.end))
            .collect();
        for block in &candidates {
            if !consumed.contains(&(block.start, block.end)) {
                let access = access_map
                    .get(block.full_name.as_str())
                    .copied()
                    .unwrap_or(AccessLevel::Private);
                ordered_blocks.push((block.clone(), access));
            }
        }

        let region_start = candidates.first().map(|item| item.start).unwrap_or(0);
        let region_end = candidates
            .last()
            .map(|item| item.end)
            .unwrap_or(region_start);
        if region_end <= region_start || region_end > context.text.len() {
            return PolicyResult::unchanged();
        }

        let mut replacement = String::new();
        let mut last_access: Option<AccessLevel> = None;
        for (idx, (block, access)) in ordered_blocks.iter().enumerate() {
            if last_access != Some(*access) {
                if idx > 0 {
                    replacement.push('\n');
                }
                replacement.push_str(access.section_comment());
                replacement.push('\n');
                last_access = Some(*access);
            } else if idx > 0 {
                replacement.push_str("\n\n");
            }
            replacement.push_str(block.text.trim_end());
        }

        let original_region = context.text[region_start..region_end].to_string();
        if original_region.trim_end() == replacement.trim_end() {
            return PolicyResult::unchanged();
        }

        let mut updated = String::new();
        updated.push_str(&context.text[..region_start]);
        updated.push_str(replacement.trim_end());
        updated.push_str(&context.text[region_end..]);

        PolicyResult {
            text: updated,
            changed: true,
            violations: vec![Violation {
                policy: self.name().into(),
                message: "Reordered class member implementations to match header declaration order with access-level sections".to_string(),
                line: candidates.first().map(|item| item.start_line).unwrap_or(1),
                column: Some(1),
            }],
            edits: vec![Edit {
                policy: self.name().into(),
                line: candidates.first().map(|item| item.start_line).unwrap_or(1),
                before: original_region,
                after: replacement,
            }],
            warnings: Vec::new(),
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

    fn parse_cpp(text: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("set cpp language");
        parser.parse(text, None).expect("parse tree")
    }

    #[test]
    fn skips_no_header() {
        let policy = ClassLayoutPolicy::new(
            vec![".cpp".to_string()],
            vec![".hpp".to_string()],
            vec!["include".to_string()],
        );
        let text = "int main() { return 0; }\n".to_string();
        let tree = parse_cpp(&text);
        let semantic = SemanticFileContext::default();
        let path = PathBuf::from("missing.cpp");
        let ctx = PolicyContext::new(text.as_str(), &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic));
        let result = policy.apply(&ctx);
        assert!(!result.changed);
        assert!(result.edits.is_empty());
    }
}
