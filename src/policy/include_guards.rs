use std::collections::HashSet;

use tree_sitter::{Node, Tree};

use crate::model::edit::Edit;
use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::model::violation::Violation;
use crate::policy::Policy;
use std::borrow::Cow;

use crate::policy::text_utils::join_lines_cow;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IncludeGuardMode {
    PragmaOnce,
    IncludeGuard,
    Both,
}

impl IncludeGuardMode {
    pub fn from_value(value: &str) -> Self {
        match value.trim().to_lowercase().as_str() {
            "include_guard" => Self::IncludeGuard,
            "both" => Self::Both,
            _ => Self::PragmaOnce,
        }
    }
}

pub struct IncludeGuardsPolicy {
    mode: IncludeGuardMode,
    header_extensions: HashSet<String>,
}

impl IncludeGuardsPolicy {
    pub fn new(mode: IncludeGuardMode, header_extensions: HashSet<String>) -> Self {
        Self {
            mode,
            header_extensions,
        }
    }

    fn is_header(&self, path: &str) -> bool {
        let lower = path.to_lowercase();
        self.header_extensions
            .iter()
            .any(|ext| lower.ends_with(ext.as_str()))
    }

    fn has_pragma_once_ts(tree: &Tree, source: &[u8]) -> bool {
        let root = tree.root_node();
        for i in 0..root.child_count() {
            let Some(child) = root.child(i as u32) else {
                continue;
            };
            if child.kind() != "preproc_call" {
                continue;
            }
            if let Some(directive) = child.child_by_field_name("directive") {
                if let Ok(text) = directive.utf8_text(source) {
                    if text == "#pragma" {
                        if let Some(argument) = child.child_by_field_name("argument") {
                            if let Ok(arg) = argument.utf8_text(source) {
                                if arg.trim() == "once" {
                                    return true;
                                }
                            }
                        }
                    }
                }
            }
        }
        false
    }

    fn has_pragma_once(tree: Option<&Tree>, source: &[u8]) -> bool {
        if let Some(tree) = tree {
            Self::has_pragma_once_ts(tree, source)
        } else {
            false
        }
    }

    fn find_top_insert(lines: &[Cow<'_, str>]) -> usize {
        let mut idx = 0usize;
        while idx < lines.len() {
            let trimmed = lines[idx].trim();
            let is_comment =
                trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*');
            if trimmed.is_empty() || is_comment || trimmed.starts_with("#!") {
                idx += 1;
                continue;
            }
            break;
        }
        idx
    }

    fn derive_macro(path: &str) -> String {
        let mut value = String::with_capacity(path.len() + 8);
        let mut last_sep = false;
        for ch in path.chars() {
            if ch.is_ascii_alphanumeric() {
                value.push(ch.to_ascii_uppercase());
                last_sep = false;
            } else if !last_sep {
                value.push('_');
                last_sep = true;
            }
        }
        let mut macro_name = value.trim_matches('_').to_string();
        if macro_name.is_empty() {
            macro_name = "HEADER".to_string();
        }
        if macro_name
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_digit())
        {
            macro_name.insert_str(0, "H_");
        }
        if !macro_name.ends_with('_') {
            macro_name.push('_');
        }
        macro_name
    }

    fn has_include_guard_ts(tree: &Tree, source: &[u8]) -> bool {
        let root = tree.root_node();
        for i in 0..root.child_count() {
            let Some(child) = root.child(i as u32) else {
                continue;
            };
            if child.kind() != "preproc_ifdef" {
                continue;
            }
            let Some(name_node) = child.child_by_field_name("name") else {
                continue;
            };
            let Ok(ifndef_name) = name_node.utf8_text(source) else {
                continue;
            };
            if ifndef_name.is_empty() {
                continue;
            }
            if Self::has_matching_define(&child, source, ifndef_name) {
                return true;
            }
        }
        false
    }

    fn has_matching_define(ifdef_node: &Node<'_>, source: &[u8], guard_name: &str) -> bool {
        for i in 0..ifdef_node.child_count() {
            let Some(child) = ifdef_node.child(i as u32) else {
                continue;
            };
            if child.kind() != "preproc_def" {
                continue;
            }
            if let Some(name_node) = child.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source) {
                    if name == guard_name {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn has_include_guard(tree: Option<&Tree>, source: &[u8]) -> bool {
        if let Some(tree) = tree {
            Self::has_include_guard_ts(tree, source)
        } else {
            false
        }
    }
}

impl Policy for IncludeGuardsPolicy {
    fn name(&self) -> &str {
        "include_guards"
    }

    fn apply(&self, context: &PolicyContext<'_>) -> PolicyResult {
        if !self.is_header(context.path_str()) {
            return PolicyResult::unchanged();
        }
        let semantic_query = context.semantic_query();
        if !semantic_query.is_available() {
            return PolicyResult::unchanged_with_warning(
                "include_guards: semantic context unavailable; skipping heuristic edits"
                    .to_string(),
            );
        }

        let shared = context.shared.unwrap();
        let eol = shared.line_ending();
        let mut lines = shared.lines_cow();
        let trailing_newline = shared.trailing_newline();
        let mut edits = Vec::new();
        let mut violations = Vec::new();
        let mut skipped_semantic_unsafe = 0usize;

        let tree = context.tree_sitter_tree;
        let source = context.text.as_bytes();
        let has_pragma = Self::has_pragma_once(tree, source);
        let has_guard = Self::has_include_guard(tree, source);

        if matches!(
            self.mode,
            IncludeGuardMode::PragmaOnce | IncludeGuardMode::Both
        ) && !has_pragma
        {
            let insert = Self::find_top_insert(&lines);
            if !semantic_query.is_safe_global(insert + 1, 1) {
                skipped_semantic_unsafe = skipped_semantic_unsafe.saturating_add(1);
            } else {
                lines.insert(insert, Cow::Owned("#pragma once".to_string()));
                edits.push(Edit {
                    policy: self.name().into(),
                    line: insert + 1,
                    before: String::new(),
                    after: "#pragma once".to_string(),
                });
                violations.push(Violation {
                    policy: self.name().into(),
                    message: "added #pragma once".to_string(),
                    line: insert + 1,
                    column: Some(1),
                });
            }
        }

        if matches!(
            self.mode,
            IncludeGuardMode::IncludeGuard | IncludeGuardMode::Both
        ) && !has_guard
        {
            let top_safe = semantic_query.is_safe_global(1, 1);
            let footer_line = lines.len().max(1);
            let footer_safe = semantic_query.is_safe_global(footer_line, 1);
            if !top_safe || !footer_safe {
                skipped_semantic_unsafe = skipped_semantic_unsafe.saturating_add(1);
            } else {
                let guard = Self::derive_macro(context.path_str());
                lines.insert(0, Cow::Owned(format!("#define {guard}")));
                lines.insert(0, Cow::Owned(format!("#ifndef {guard}")));
                if !lines.last().is_some_and(|line| line.trim().is_empty()) {
                    lines.push(Cow::Owned(String::new()));
                }
                lines.push(Cow::Owned(format!("#endif  // {guard}")));

                edits.push(Edit {
                    policy: self.name().into(),
                    line: 1,
                    before: String::new(),
                    after: format!("#ifndef {guard}"),
                });
                violations.push(Violation {
                    policy: self.name().into(),
                    message: "added include guard".to_string(),
                    line: 1,
                    column: Some(1),
                });
            }
        }

        if edits.is_empty() {
            let mut warnings = Vec::new();
            if skipped_semantic_unsafe > 0 {
                warnings.push(format!(
                    "include_guards: skipped {} semantic-unsafe candidate region(s)",
                    skipped_semantic_unsafe
                ));
            }
            return PolicyResult::unchanged_with_warnings(warnings);
        }

        let mut warnings = Vec::new();
        if skipped_semantic_unsafe > 0 {
            warnings.push(format!(
                "include_guards: skipped {} semantic-unsafe candidate region(s)",
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
    use std::collections::HashSet;
    use std::path::PathBuf;

    use super::*;
    use crate::model::policy_context::PolicyContext;
    use crate::parser::file_context::SemanticFileContext;
    use crate::policy::shared_data::PolicySharedData;

    #[test]
    fn adds_pragma_once() {
        let mut exts = HashSet::new();
        exts.insert(".hpp".to_string());
        let policy = IncludeGuardsPolicy::new(IncludeGuardMode::PragmaOnce, exts);
        let text = "class A {};\n".to_string();
        let path = PathBuf::from("a.hpp");
        let semantic = SemanticFileContext::default();
        let shared = PolicySharedData::new(text.as_str(), None);
        let ctx =
            PolicyContext::new(text.as_str(), &path).with_semantic(Some(&semantic)).with_shared(Some(&shared));
        let result = policy.apply(&ctx);
        assert!(result.text.contains("#pragma once"));
    }
}
