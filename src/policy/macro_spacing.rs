use crate::model::edit::Edit;
use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::model::violation::Violation;
use crate::policy::Policy;
use std::borrow::Cow;

use crate::policy::text_utils::join_lines_cow;

pub struct LuaMacroSpacingPolicy;

impl LuaMacroSpacingPolicy {
    pub fn new() -> Self {
        Self
    }

    fn has_macro_header(lines: &[Cow<'_, str>], index: usize) -> bool {
        if index < 1 || index + 1 >= lines.len() {
            return false;
        }

        let title = lines[index].to_lowercase();
        if !title.contains("user defined macros") {
            return false;
        }

        let above = lines[index - 1].trim();
        let below = lines[index + 1].trim();
        above.starts_with("//")
            && above[2..].chars().all(|ch| ch == '-')
            && below.starts_with("//")
            && below[2..].chars().all(|ch| ch == '-')
    }

    fn is_cpp_source(path: &str) -> bool {
        let lower = path.to_lowercase();
        [".c", ".cc", ".cpp", ".cxx"]
            .iter()
            .any(|ext| lower.ends_with(ext))
    }
}

impl Policy for LuaMacroSpacingPolicy {
    fn name(&self) -> &str {
        "lua_macro_spacing"
    }

    fn apply(&self, context: &PolicyContext<'_>) -> PolicyResult {
        if !Self::is_cpp_source(context.path_str()) {
            return PolicyResult::unchanged();
        }
        let semantic_query = context.semantic_query();
        if !semantic_query.is_available() {
            return PolicyResult::unchanged_with_warning(
                "lua_macro_spacing: semantic context unavailable; skipping heuristic edits"
                    .to_string(),
            );
        }

        let shared = context.shared.unwrap();
        let eol = shared.line_ending();
        let mut lines = shared.lines_cow();
        let trailing_newline = shared.trailing_newline();
        let mut edits = Vec::new();
        let mut skipped_semantic_unsafe = 0usize;

        let mut index = 0usize;
        while index < lines.len() {
            if Self::has_macro_header(&lines, index) {
                let define_index = index + 2;
                if define_index < lines.len() {
                    let candidate = lines[define_index].trim_start();
                    if candidate.starts_with("#define") {
                        if !semantic_query.is_safe_global(define_index + 1, 1)
                        {
                            skipped_semantic_unsafe = skipped_semantic_unsafe.saturating_add(1);
                            index += 1;
                            continue;
                        }
                        lines.insert(define_index, Cow::Owned(String::new()));
                        edits.push(Edit {
                            policy: self.name().into(),
                            line: define_index + 1,
                            before: String::new(),
                            after: String::new(),
                        });
                        index += 1;
                    }
                }
            }
            index += 1;
        }

        if edits.is_empty() {
            let mut warnings = Vec::new();
            if skipped_semantic_unsafe > 0 {
                warnings.push(format!(
                    "lua_macro_spacing: skipped {} semantic-unsafe candidate line(s)",
                    skipped_semantic_unsafe
                ));
            }
            return PolicyResult::unchanged_with_warnings(warnings);
        }

        let mut warnings = Vec::new();
        if skipped_semantic_unsafe > 0 {
            warnings.push(format!(
                "lua_macro_spacing: skipped {} semantic-unsafe candidate line(s)",
                skipped_semantic_unsafe
            ));
        }
        PolicyResult {
            text: join_lines_cow(&lines, eol, trailing_newline),
            changed: true,
            violations: vec![Violation {
                policy: self.name().into(),
                message: "inserted blank line after macro section headers".to_string(),
                line: edits[0].line,
                column: Some(1),
            }],
            edits,
            warnings,
        }
    }
}
