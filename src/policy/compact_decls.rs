use crate::model::edit::Edit;
use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::model::violation::Violation;
use crate::policy::Policy;
use crate::policy::text_utils::{detect_line_ending, join_lines, split_lines};

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

    fn is_identifier(value: &str) -> bool {
        let mut chars = value.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        if !(first.is_ascii_alphabetic() || first == '_') {
            return false;
        }
        chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    }

    fn parse_candidate(&self, line: &str) -> Option<CandidateDecl> {
        if line.contains("//") || line.contains("/*") {
            return None;
        }

        let trimmed = line.trim();
        if !trimmed.ends_with(';') {
            return None;
        }
        let core = trimmed.trim_end_matches(';').trim();
        if core.is_empty() {
            return None;
        }

        let banned_tokens = [",", "=", "(", ")", "{", "}", "[", "]", ":", "->", "*", "&"];
        if banned_tokens.iter().any(|token| core.contains(token)) {
            return None;
        }

        let parts: Vec<&str> = core.split_whitespace().collect();
        if parts.len() < 2 {
            return None;
        }
        let name = parts.last().copied().unwrap_or_default();
        if !Self::is_identifier(name) {
            return None;
        }

        let name_index = core.rfind(name)?;
        if name_index == 0 {
            return None;
        }
        let type_prefix = core.get(..name_index)?.trim_end().to_string();
        if type_prefix.is_empty() {
            return None;
        }

        let indent_len = line
            .len()
            .saturating_sub(line.trim_start_matches([' ', '\t']).len());
        let indent = line.get(..indent_len).unwrap_or("").to_string();

        Some(CandidateDecl {
            indent,
            type_prefix,
            name: name.to_string(),
        })
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
        if before.len() == after.len() {
            return edits;
        }

        let tail = before.len().max(after.len());
        for idx in shared..tail {
            edits.push(Edit {
                policy: self.name().into(),
                line: idx + 1,
                before: before.get(idx).cloned().unwrap_or_default(),
                after: after.get(idx).cloned().unwrap_or_default(),
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
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits: Vec::new(),
                warnings: vec![
                    "compact_declarations: semantic context unavailable; skipping heuristic edits"
                        .to_string(),
                ],
            };
        }
        let eol = detect_line_ending(context.text);
        let (before_lines, trailing_newline) = split_lines(context.text);
        if before_lines.len() < self.min_group_size {
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits: Vec::new(),
                warnings: Vec::new(),
            };
        }

        let mut after_lines = Vec::with_capacity(before_lines.len());
        let mut violations = Vec::new();
        let mut warnings = Vec::new();
        let mut skipped_semantic_unsafe = 0usize;
        let mut idx = 0usize;

        while idx < before_lines.len() {
            let Some(first) = self.parse_candidate(&before_lines[idx]) else {
                after_lines.push(before_lines[idx].clone());
                idx += 1;
                continue;
            };

            let mut group = vec![first];
            let mut group_end = idx + 1;
            while group_end < before_lines.len() {
                let Some(next) = self.parse_candidate(&before_lines[group_end]) else {
                    break;
                };
                if next.indent != group[0].indent || next.type_prefix != group[0].type_prefix {
                    break;
                }
                group.push(next);
                group_end += 1;
            }

            if group.len() >= self.min_group_size {
                let group_safe = !semantic_query.is_available()
                    || (idx..group_end)
                        .all(|line_idx| !semantic_query.is_macro_region(line_idx + 1, 1));
                if !group_safe {
                    skipped_semantic_unsafe = skipped_semantic_unsafe.saturating_add(group.len());
                    after_lines.push(before_lines[idx].clone());
                    idx += 1;
                    continue;
                }
                let names: Vec<String> = group.iter().map(|item| item.name.clone()).collect();
                after_lines.push(format!(
                    "{}{} {};",
                    group[0].indent,
                    group[0].type_prefix,
                    names.join(", ")
                ));
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
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits: Vec::new(),
                warnings: Vec::new(),
            };
        }

        let edits = self.line_edits(before_lines.as_slice(), after_lines.as_slice());
        if skipped_semantic_unsafe > 0 {
            warnings.push(format!(
                "compact_declarations: skipped {} semantic-unsafe declaration line(s)",
                skipped_semantic_unsafe
            ));
        }
        PolicyResult {
            text: join_lines(&after_lines, eol, trailing_newline),
            violations,
            edits,
            warnings,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::model::policy_context::PolicyContext;
    use crate::parser::file_context::SemanticFileContext;

    #[test]
    fn compacts_adjacent_declarations() {
        let policy = CompactDeclarationsPolicy::new(3);
        let text = "int a;\nint b;\nint c;\n".to_string();
        let path = PathBuf::from("sample.cpp");
        let semantic = SemanticFileContext::default();
        let ctx =
            PolicyContext::new(text.as_str(), &path).with_semantic(Some(&semantic));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "int a, b, c;\n");
        assert_eq!(result.violations.len(), 1);
    }
}
