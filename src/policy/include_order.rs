use std::collections::{HashMap, HashSet};
use std::path::Path;

use tree_sitter::{Node, StreamingIterator};

use crate::model::edit::Edit;
use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::model::violation::Violation;
use crate::parser::query_cache::TsQueryCache;
use crate::policy::traits::Policy;
use crate::policy::text_utils::{detect_line_ending, join_lines, split_lines};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IncludeQuote {
    Angle,
    Double,
}

#[derive(Clone, Debug)]
struct IncludeEntry {
    header: String,
    quote: IncludeQuote,
}

pub struct IncludeOrderPolicy {
    order_header: Vec<String>,
    order_source: Vec<String>,
    standard_headers: HashSet<String>,
    standard_prefixes: Vec<String>,
    project_headers: HashSet<String>,
    project_prefixes: Vec<String>,
    main_header_extensions: Vec<String>,
    separator_length: usize,
    emit_group_comments: bool,
    group_titles: HashMap<String, String>,
    third_party_labels: HashMap<String, String>,
}

impl IncludeOrderPolicy {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        order_header: Vec<String>,
        order_source: Vec<String>,
        standard_headers: HashSet<String>,
        standard_prefixes: Vec<String>,
        project_headers: HashSet<String>,
        project_prefixes: Vec<String>,
        main_header_extensions: Vec<String>,
        separator_length: usize,
        emit_group_comments: bool,
        group_titles: HashMap<String, String>,
        third_party_labels: HashMap<String, String>,
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
                vec![
                    ".hpp".to_string(),
                    ".h".to_string(),
                    ".hh".to_string(),
                    ".hxx".to_string(),
                ]
            } else {
                main_header_extensions
            },
            separator_length: separator_length.max(2),
            emit_group_comments,
            group_titles,
            third_party_labels,
        }
    }

    fn parse_include(&self, line: &str) -> Option<IncludeEntry> {
        let mut cursor = line.trim_start();
        cursor = cursor.strip_prefix('#')?.trim_start();
        cursor = cursor.strip_prefix("include")?.trim_start();

        if let Some(rest) = cursor.strip_prefix('<') {
            let end = rest.find('>')?;
            return Some(IncludeEntry {
                header: rest[..end].trim().to_string(),
                quote: IncludeQuote::Angle,
            });
        }

        let rest = cursor.strip_prefix('"')?;
        let end = rest.find('"')?;
        Some(IncludeEntry {
            header: rest[..end].trim().to_string(),
            quote: IncludeQuote::Double,
        })
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

    fn collect_include_lines_from_tree(
        &self,
        root: Node<'_>,
        lines: &[String],
        query_cache: Option<&TsQueryCache>,
    ) -> Vec<usize> {
        let mut include_lines = Vec::new();

        if let Some(query) = query_cache
            .and_then(|qc| qc.get_or_compile("(preproc_include) @inc").ok())
        {
            let mut cursor = tree_sitter::QueryCursor::new();
            let mut matches = cursor.matches(&query, root, "".as_bytes());
            while let Some(m) = {
                matches.advance();
                matches.get()
            } {
                for capture in m.captures {
                    let line_idx = capture.node.start_position().row;
                    if line_idx < lines.len()
                        && self.parse_include(&lines[line_idx]).is_some()
                    {
                        include_lines.push(line_idx);
                    }
                }
            }
        } else {
            let mut stack = vec![root];
            while let Some(node) = stack.pop() {
                if node.kind() == "preproc_include" {
                    let line_idx = node.start_position().row;
                    if line_idx < lines.len()
                        && self.parse_include(&lines[line_idx]).is_some()
                    {
                        include_lines.push(line_idx);
                    }
                }
                for child_idx in (0..node.child_count()).rev() {
                    if let Some(child) = node.child(child_idx as u32) {
                        stack.push(child);
                    }
                }
            }
        }

        include_lines.sort_unstable();
        include_lines.dedup();
        include_lines
    }

    fn is_header_file(path: &str) -> bool {
        let ext = Path::new(path)
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| format!(".{}", value.to_lowercase()))
            .unwrap_or_default();
        matches!(ext.as_str(), ".h" | ".hpp" | ".hh" | ".hxx")
    }

    fn main_header_candidates(&self, path: &str) -> HashSet<String> {
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
        main_candidates: &HashSet<String>,
    ) -> &'static str {
        let header = entry.header.to_lowercase();
        if !is_header_file && main_candidates.contains(&header) {
            return "main";
        }
        match entry.quote {
            IncludeQuote::Angle => {
                if self.is_standard_header(header.as_str()) {
                    "standard"
                } else {
                    "third_party"
                }
            }
            IncludeQuote::Double => {
                if self.is_project_header(header.as_str()) {
                    "project"
                } else {
                    "local"
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

        let mut labels = HashSet::new();
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
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits: Vec::new(),
                warnings: vec!["include_order: tree-sitter context unavailable".to_string()],
            };
        };

        let eol = detect_line_ending(context.text);
        let (lines, trailing_newline) = split_lines(context.text);
        if lines.is_empty() {
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits: Vec::new(),
                warnings: Vec::new(),
            };
        }

        let include_lines =
            self.collect_include_lines_from_tree(tree.root_node(), &lines, context.query_cache);
        let Some(cluster) = self.choose_include_cluster(&include_lines) else {
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits: Vec::new(),
                warnings: Vec::new(),
            };
        };

        let first_include = *cluster.first().unwrap_or(&0);
        let last_include = *cluster.last().unwrap_or(&0);
        let start = first_include;
        let end = last_include.saturating_add(1);
        if start >= end || end > lines.len() {
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits: Vec::new(),
                warnings: Vec::new(),
            };
        }
        let semantic_query = context.semantic_query();
        if !semantic_query.is_available() {
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits: Vec::new(),
                warnings: vec![
                    "include_order: semantic context unavailable; skipping heuristic edits"
                        .to_string(),
                ],
            };
        }
        // Structural-safe policy: per-policy checkpoint validates tree-sitter after edits.
        // Only macro regions are skipped; diagnostic error lines are not a concern.

        let mut groups: HashMap<&'static str, Vec<IncludeEntry>> = HashMap::new();
        groups.insert("main", Vec::new());
        groups.insert("standard", Vec::new());
        groups.insert("third_party", Vec::new());
        groups.insert("project", Vec::new());
        groups.insert("local", Vec::new());

        let is_header = Self::is_header_file(context.path_str());
        let main_candidates = self.main_header_candidates(context.path_str());
        for idx in &cluster {
            let Some(entry) = self.parse_include(&lines[*idx]) else {
                continue;
            };
            let group = self.classify_group(&entry, is_header, &main_candidates);
            if let Some(items) = groups.get_mut(group) {
                items.push(entry);
            }
        }

        let mut ordered_block = Vec::new();
        let mut emitted_group = false;
        for group in self.order_for_path(context.path_str()) {
            let normalized = group.trim().to_lowercase();
            let Some(items) = groups.get_mut(normalized.as_str()) else {
                continue;
            };
            if items.is_empty() {
                continue;
            }

            items.sort_by(|left, right| {
                left.header.to_lowercase().cmp(&right.header.to_lowercase())
            });
            if self.emit_group_comments {
                if emitted_group {
                    ordered_block.push(String::new());
                }
                let heading = self.heading_lines(normalized.as_str(), items.as_slice());
                ordered_block.extend(heading);
            }
            for item in items.iter() {
                match item.quote {
                    IncludeQuote::Angle => {
                        ordered_block.push(format!("#include <{}>", item.header));
                    }
                    IncludeQuote::Double => {
                        ordered_block.push(format!("#include \"{}\"", item.header));
                    }
                }
            }
            emitted_group = true;
        }

        let original_block = &lines[start..end];
        if original_block == ordered_block.as_slice() {
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits: Vec::new(),
                warnings: Vec::new(),
            };
        }

        let mut rebuilt = Vec::with_capacity(lines.len() + ordered_block.len());
        rebuilt.extend_from_slice(&lines[..start]);
        rebuilt.extend(ordered_block.clone());
        rebuilt.extend_from_slice(&lines[end..]);
        let max_lines = original_block.len().max(ordered_block.len());
        let mut edits = Vec::<Edit>::new();
        for idx in 0..max_lines {
            let before_line = original_block.get(idx).cloned().unwrap_or_default();
            let after_line = ordered_block.get(idx).cloned().unwrap_or_default();
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
            text: join_lines(&rebuilt, eol, trailing_newline),
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
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;

    use tree_sitter::Parser;

    use super::*;
    use crate::model::policy_context::PolicyContext;
    use crate::parser::file_context::SemanticFileContext;

    fn parse_cpp(text: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("cpp language");
        parser.parse(text, None).expect("parse tree")
    }

    #[test]
    fn reorders_source_include_groups() {
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
            HashSet::new(),
            Vec::new(),
            HashSet::new(),
            project_prefixes,
            vec![".hpp".to_string(), ".h".to_string()],
            64,
            false,
            HashMap::from([
                ("main".to_string(), "Main header".to_string()),
                ("standard".to_string(), "Standard Cpp Libraries".to_string()),
                ("third_party".to_string(), "Third-party headers".to_string()),
                ("project".to_string(), "Project headers".to_string()),
                ("local".to_string(), "User Defined Headers".to_string()),
            ]),
            HashMap::new(),
        );

        let text = "#include \"project/foo.h\"\n#include <vector>\n#include \"main.hpp\"\n#include <fmt/format.h>\n\nint main() { return 0; }\n".to_string();
        let path = PathBuf::from("main.cpp");
        let tree = parse_cpp(text.as_str());
        let semantic = SemanticFileContext::default();
        let ctx = PolicyContext::new(text.as_str(), &path)
            .with_tree_sitter_tree(Some(&tree))
            .with_semantic_file_context(Some(&semantic));
        let result = policy.apply(&ctx);
        assert!(result.text.contains("#include \"main.hpp\""));
        assert!(result.text.contains("#include <vector>"));
        assert!(result.text.contains("#include <fmt/format.h>"));
        assert!(!result.edits.is_empty());
    }
}
