use std::collections::HashSet;

use crate::model::edit::Edit;
use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::model::violation::Violation;
use crate::policy::Policy;
use crate::policy::text_utils::{detect_line_ending, join_lines, split_lines};

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

    fn has_pragma_once(lines: &[String]) -> bool {
        lines
            .iter()
            .take(32)
            .any(|line| line.trim().eq_ignore_ascii_case("#pragma once"))
    }

    fn find_top_insert(lines: &[String]) -> usize {
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

    fn has_include_guard(lines: &[String]) -> bool {
        if lines.len() < 3 {
            return false;
        }
        let mut ifndef = None::<String>;
        let mut define = None::<String>;
        for line in lines.iter().take(40) {
            let trimmed = line.trim();
            if ifndef.is_none() && trimmed.starts_with("#ifndef ") {
                ifndef = Some(trimmed[8..].trim().to_string());
                continue;
            }
            if define.is_none() && trimmed.starts_with("#define ") {
                define = Some(trimmed[8..].trim().to_string());
            }
            if ifndef.is_some() && define.is_some() {
                break;
            }
        }

        match (ifndef, define) {
            (Some(a), Some(b)) if a == b && !a.is_empty() => lines
                .iter()
                .rev()
                .take(10)
                .any(|line| line.trim_start().starts_with("#endif")),
            _ => false,
        }
    }
}

impl Policy for IncludeGuardsPolicy {
    fn name(&self) -> &str {
        "include_guards"
    }

    fn apply(&self, context: &PolicyContext<'_>) -> PolicyResult {
        if !self.is_header(context.path_str()) {
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
                    "include_guards: semantic context unavailable; skipping heuristic edits"
                        .to_string(),
                ],
            };
        }

        let eol = detect_line_ending(context.text);
        let (mut lines, trailing_newline) = split_lines(context.text);
        let mut edits = Vec::new();
        let mut violations = Vec::new();
        let mut skipped_semantic_unsafe = 0usize;

        let has_pragma = Self::has_pragma_once(&lines);
        let has_guard = Self::has_include_guard(&lines);

        if matches!(
            self.mode,
            IncludeGuardMode::PragmaOnce | IncludeGuardMode::Both
        ) && !has_pragma
        {
            let insert = Self::find_top_insert(&lines);
            if !semantic_query.is_safe_global(insert + 1, 1) {
                skipped_semantic_unsafe = skipped_semantic_unsafe.saturating_add(1);
            } else {
                lines.insert(insert, "#pragma once".to_string());
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
                lines.insert(0, format!("#define {guard}"));
                lines.insert(0, format!("#ifndef {guard}"));
                if !lines.last().is_some_and(|line| line.trim().is_empty()) {
                    lines.push(String::new());
                }
                lines.push(format!("#endif  // {guard}"));

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
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits: Vec::new(),
                warnings,
            };
        }

        let mut warnings = Vec::new();
        if skipped_semantic_unsafe > 0 {
            warnings.push(format!(
                "include_guards: skipped {} semantic-unsafe candidate region(s)",
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
    use std::collections::HashSet;
    use std::path::PathBuf;

    use super::*;
    use crate::model::policy_context::PolicyContext;
    use crate::parser::file_context::SemanticFileContext;

    #[test]
    fn adds_pragma_once() {
        let mut exts = HashSet::new();
        exts.insert(".hpp".to_string());
        let policy = IncludeGuardsPolicy::new(IncludeGuardMode::PragmaOnce, exts);
        let text = "class A {};\n".to_string();
        let path = PathBuf::from("a.hpp");
        let semantic = SemanticFileContext::default();
        let ctx =
            PolicyContext::new(text.as_str(), &path).with_semantic(Some(&semantic));
        let result = policy.apply(&ctx);
        assert!(result.text.contains("#pragma once"));
    }
}
