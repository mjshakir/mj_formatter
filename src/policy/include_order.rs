use std::path::Path;

use rustc_hash::{FxHashMap, FxHashSet};

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IncludeQuote {
    Angle,
    Double,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IncludeGroup {
    Main = 0,
    Standard = 1,
    ThirdParty = 2,
    Project = 3,
    Local = 4,
}

impl IncludeGroup {
    fn from_str(value: &str) -> Option<Self> {
        match value {
            "main" => Some(Self::Main),
            "standard" => Some(Self::Standard),
            "third_party" => Some(Self::ThirdParty),
            "project" => Some(Self::Project),
            "local" => Some(Self::Local),
            _ => None,
        }
    }

}

#[derive(Clone, Debug)]
struct IncludeEntry {
    header: String,
    quote: IncludeQuote,
}

pub struct IncludeOrderPolicy {
    order_header: Vec<String>,
    order_source: Vec<String>,
    standard_headers: FxHashSet<String>,
    standard_prefixes: Vec<String>,
    project_headers: FxHashSet<String>,
    project_prefixes: Vec<String>,
    main_header_extensions: Vec<String>,
    separator_length: usize,
    emit_group_comments: bool,
    group_titles: FxHashMap<String, String>,
    third_party_labels: FxHashMap<String, String>,
}

impl IncludeOrderPolicy {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        order_header: Vec<String>,
        order_source: Vec<String>,
        standard_headers: FxHashSet<String>,
        standard_prefixes: Vec<String>,
        project_headers: FxHashSet<String>,
        project_prefixes: Vec<String>,
        main_header_extensions: Vec<String>,
        separator_length: usize,
        emit_group_comments: bool,
        group_titles: FxHashMap<String, String>,
        third_party_labels: FxHashMap<String, String>,
    ) -> Self {
        Self {
            order_header: if order_header.is_empty() {
                vec![
                    "standard".to_string(),
                    "third_party".to_string(),
                    "project".to_string(),
                    "local".to_string(),
                ]
            } else {
                order_header
            },
            order_source: if order_source.is_empty() {
                vec![
                    "main".to_string(),
                    "standard".to_string(),
                    "third_party".to_string(),
                    "project".to_string(),
                    "local".to_string(),
                ]
            } else {
                order_source
            },
            standard_headers,
            standard_prefixes,
            project_headers,
            project_prefixes,
            main_header_extensions: if main_header_extensions.is_empty() {
                crate::files::file_unit::HEADER_EXTENSIONS
                    .iter()
                    .map(|e| format!(".{e}"))
                    .collect()
            } else {
                main_header_extensions
            },
            separator_length: separator_length.max(2),
            emit_group_comments,
            group_titles,
            third_party_labels,
        }
    }

    fn parse_include_from_node(node: &Node<'_>, source: &[u8]) -> Option<IncludeEntry> {
        let path_node = node.child_by_field_id(ts_cpp_symbols::field_path)?;
        let text = path_node.utf8_text(source).ok()?;
        match path_node.kind_id() {
            ts_cpp_symbols::sym_system_lib_string => {
                let header = text.strip_prefix('<')?.strip_suffix('>')?.trim().to_string();
                if header.is_empty() {
                    return None;
                }
                Some(IncludeEntry {
                    header,
                    quote: IncludeQuote::Angle,
                })
            }
            ts_cpp_symbols::sym_string_literal => {
                let header = text.strip_prefix('"')?.strip_suffix('"')?.trim().to_string();
                if header.is_empty() {
                    return None;
                }
                Some(IncludeEntry {
                    header,
                    quote: IncludeQuote::Double,
                })
            }
            _ => None,
        }
    }

    fn choose_include_cluster(&self, include_lines: &[usize]) -> Option<Vec<usize>> {
        if include_lines.is_empty() {
            return None;
        }

        let mut clusters: Vec<Vec<usize>> = Vec::new();
        let mut current = vec![include_lines[0]];
        for idx in include_lines.iter().copied().skip(1) {
            let prev = *current.last().unwrap_or(&idx);
            if idx == prev.saturating_add(1) {
                current.push(idx);
            } else {
                clusters.push(current);
                current = vec![idx];
            }
        }
        clusters.push(current);
        clusters.into_iter().next()
    }

    fn collect_include_entries_from_tree(
        root: Node<'_>,
        source: &[u8],
        line_count: usize,
        query_cache: Option<&TsQueryCache>,
    ) -> Vec<(usize, IncludeEntry)> {
        let mut entries = Vec::new();

        ts_traversal::query_or_traverse(
            root,
            "(preproc_include) @inc",
            query_cache,
            &[ts_cpp_symbols::sym_preproc_include],
            source,
            |node| {
                let line_idx = node.start_position().row;
                if line_idx < line_count {
                    if let Some(entry) = Self::parse_include_from_node(&node, source) {
                        entries.push((line_idx, entry));
                    }
                }
            },
        );

        entries.sort_by_key(|(idx, _)| *idx);
        entries.dedup_by_key(|(idx, _)| *idx);
        entries
    }

    fn is_header_file(path: &str) -> bool {
        Path::new(path)
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(crate::files::file_unit::is_header_extension)
    }

    fn main_header_candidates(&self, path: &str) -> FxHashSet<String> {
        let stem = Path::new(path)
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        self.main_header_extensions
            .iter()
            .map(|ext| format!("{stem}{ext}").to_lowercase())
            .collect()
    }

    fn is_standard_header(&self, header: &str) -> bool {
        let normalized = header.to_lowercase();
        if self.standard_headers.contains(&normalized) {
            return true;
        }
        if self
            .standard_prefixes
            .iter()
            .any(|prefix| normalized.starts_with(prefix))
        {
            return true;
        }
        !header.contains('/')
    }

    fn is_project_header(&self, header: &str) -> bool {
        let normalized = header.to_lowercase();
        if self.project_headers.contains(&normalized) {
            return true;
        }
        self.project_prefixes
            .iter()
            .any(|prefix| normalized.starts_with(prefix))
    }

    fn classify_group(
        &self,
        entry: &IncludeEntry,
        is_header_file: bool,
        main_candidates: &FxHashSet<String>,
    ) -> IncludeGroup {
        let header = entry.header.to_lowercase();
        if !is_header_file && main_candidates.contains(&header) {
            return IncludeGroup::Main;
        }
        match entry.quote {
            IncludeQuote::Angle => {
                if self.is_standard_header(header.as_str()) {
                    IncludeGroup::Standard
                } else {
                    IncludeGroup::ThirdParty
                }
            }
            IncludeQuote::Double => {
                if self.is_project_header(header.as_str()) {
                    IncludeGroup::Project
                } else {
                    IncludeGroup::Local
                }
            }
        }
    }

    fn group_title(&self, group: &str, items: &[IncludeEntry]) -> String {
        let mut title = self
            .group_titles
            .get(group)
            .cloned()
            .unwrap_or_else(|| group.to_string());
        if group != "third_party" {
            return title;
        }

        let mut labels = FxHashSet::default();
        for item in items {
            let prefix = item.header.split('/').next().unwrap_or("").to_lowercase();
            if prefix.is_empty() {
                continue;
            }
            let label = self
                .third_party_labels
                .get(&prefix)
                .cloned()
                .unwrap_or(prefix);
            labels.insert(label);
        }
        if labels.is_empty() {
            return title;
        }

        let mut sorted_labels: Vec<String> = labels.into_iter().collect();
        sorted_labels.sort();
        title.push_str(": ");
        title.push_str(sorted_labels.join(", ").as_str());
        title
    }

    fn heading_lines(&self, group: &str, items: &[IncludeEntry]) -> [String; 3] {
        let separator = format!("//{}", "-".repeat(self.separator_length.saturating_sub(2)));
        let title = self.group_title(group, items);
        [separator.clone(), format!("// {title}"), separator]
    }

    fn order_for_path(&self, path: &str) -> &[String] {
        if Self::is_header_file(path) {
            &self.order_header
        } else {
            &self.order_source
        }
    }
}

impl Policy for IncludeOrderPolicy {
    fn name(&self) -> &str {
        "include_order"
    }
    fn apply(&self, context: &PolicyContext<'_>) -> PolicyResult {
        let Some(tree) = context.tree_sitter_tree else {
            return PolicyResult::unchanged_with_warning(
                "include_order: tree-sitter context unavailable".to_string(),
            );
        };

        let shared = context.shared.unwrap();
        let eol = shared.line_ending();
        let lines = shared.lines_cow();
        let trailing_newline = shared.trailing_newline();
        if lines.is_empty() {
            return PolicyResult::unchanged();
        }

        let source = context.text.as_bytes();
        let include_entries =
            Self::collect_include_entries_from_tree(tree.root_node(), source, lines.len(), context.query_cache);
        let include_lines: Vec<usize> = include_entries.iter().map(|(idx, _)| *idx).collect();
        let entry_map: FxHashMap<usize, &IncludeEntry> =
            include_entries.iter().map(|(idx, entry)| (*idx, entry)).collect();
        let Some(cluster) = self.choose_include_cluster(&include_lines) else {
            return PolicyResult::unchanged();
        };

        let first_include = *cluster.first().unwrap_or(&0);
        let last_include = *cluster.last().unwrap_or(&0);
        let start = first_include;
        let end = last_include.saturating_add(1);
        if start >= end || end > lines.len() {
            return PolicyResult::unchanged();
        }
        let semantic_query = context.semantic_query();
        if !semantic_query.is_available() {
            return PolicyResult::unchanged_with_warning(
                "include_order: semantic context unavailable; skipping heuristic edits"
                    .to_string(),
            );
        }
        // Structural-safe policy: per-policy checkpoint validates tree-sitter after edits.
        // Only macro regions are skipped; diagnostic error lines are not a concern.

        let mut groups: [Vec<IncludeEntry>; 5] = Default::default();

        let is_header = Self::is_header_file(context.path_str());
        let main_candidates = self.main_header_candidates(context.path_str());
        for idx in &cluster {
            let Some(entry) = entry_map.get(idx) else {
                continue;
            };
            let group = self.classify_group(entry, is_header, &main_candidates);
            groups[group as usize].push((*entry).clone());
        }

        let mut ordered_block: Vec<Cow<'_, str>> = Vec::new();
        let mut emitted_group = false;
        for group in self.order_for_path(context.path_str()) {
            let normalized = group.trim().to_lowercase();
            let Some(group_idx) = IncludeGroup::from_str(normalized.as_str()) else {
                continue;
            };
            let items = &mut groups[group_idx as usize];
            if items.is_empty() {
                continue;
            }

            items.sort_by_cached_key(|item| item.header.to_lowercase());
            if self.emit_group_comments {
                if emitted_group {
                    ordered_block.push(Cow::Owned(String::new()));
                }
                let heading = self.heading_lines(normalized.as_str(), items.as_slice());
                ordered_block.extend(heading.into_iter().map(Cow::Owned));
            }
            for item in items.iter() {
                match item.quote {
                    IncludeQuote::Angle => {
                        ordered_block.push(Cow::Owned(format!("#include <{}>", item.header)));
                    }
                    IncludeQuote::Double => {
                        ordered_block.push(Cow::Owned(format!("#include \"{}\"", item.header)));
                    }
                }
            }
            emitted_group = true;
        }

        let original_block = &lines[start..end];
        if original_block == ordered_block.as_slice() {
            return PolicyResult::unchanged();
        }

        let mut rebuilt: Vec<Cow<'_, str>> = Vec::with_capacity(lines.len() + ordered_block.len());
        rebuilt.extend_from_slice(&lines[..start]);
        rebuilt.extend(ordered_block.clone());
        rebuilt.extend_from_slice(&lines[end..]);
        let max_lines = original_block.len().max(ordered_block.len());
        let mut edits = Vec::<Edit>::new();
        for idx in 0..max_lines {
            let before_line = original_block.get(idx).map(|c| c.to_string()).unwrap_or_default();
            let after_line = ordered_block.get(idx).map(|c| c.to_string()).unwrap_or_default();
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
            text: join_lines_cow(&rebuilt, eol, trailing_newline),
            changed: true,
            violations: vec![Violation {
                policy: self.name().into(),
                message: "Includes are not ordered".to_string(),
                line: edits[0].line,
                column: Some(1),
            }],
            edits,
            warnings: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use rustc_hash::{FxHashMap, FxHashSet};
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
    fn reorders_include_groups() {
        let project_prefixes = vec!["project/".to_string()];
        let policy = IncludeOrderPolicy::new(
            vec![
                "standard".to_string(),
                "third_party".to_string(),
                "project".to_string(),
                "local".to_string(),
            ],
            vec![
                "main".to_string(),
                "standard".to_string(),
                "third_party".to_string(),
                "project".to_string(),
                "local".to_string(),
            ],
            FxHashSet::default(),
            Vec::new(),
            FxHashSet::default(),
            project_prefixes,
            vec![".hpp".to_string(), ".h".to_string()],
            64,
            false,
            [
                ("main".to_string(), "Main header".to_string()),
                ("standard".to_string(), "Standard Cpp Libraries".to_string()),
                ("third_party".to_string(), "Third-party headers".to_string()),
                ("project".to_string(), "Project headers".to_string()),
                ("local".to_string(), "User Defined Headers".to_string()),
            ].into_iter().collect(),
            FxHashMap::default(),
        );

        let text = "#include \"project/foo.h\"\n#include <vector>\n#include \"main.hpp\"\n#include <fmt/format.h>\n\nint main() { return 0; }\n".to_string();
        let path = PathBuf::from("main.cpp");
        let tree = parse_cpp(text.as_str());
        let semantic = SemanticFileContext::default();
        let shared = PolicySharedData::new(text.as_str(), None);
        let ctx = PolicyContext::new(text.as_str(), &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic))
            .with_shared(Some(&shared));
        let result = policy.apply(&ctx);
        assert!(result.text.contains("#include \"main.hpp\""));
        assert!(result.text.contains("#include <vector>"));
        assert!(result.text.contains("#include <fmt/format.h>"));
        assert!(!result.edits.is_empty());
    }
}
