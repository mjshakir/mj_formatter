use std::borrow::Cow;

use crate::model::edit::Edit;
use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::model::violation::Violation;
use crate::policy::Policy;
use crate::policy::text_utils::join_lines_cow;

pub struct DeclarationAlignmentPolicy;

struct ClauseHit {
    line: usize,
    eq_byte_offset: usize,
}

impl DeclarationAlignmentPolicy {
    pub fn new() -> Self {
        Self
    }

    fn find_clause_eq_in_line(line: &str) -> Option<usize> {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with('#') {
            return None;
        }
        for pat in &["= delete", "= default"] {
            if let Some(pos) = line.rfind(pat) {
                if line[..pos].contains('(') {
                    return Some(pos);
                }
            }
        }
        None
    }

    fn scan_lines(lines: &[Cow<'_, str>]) -> Vec<ClauseHit> {
        let mut hits = Vec::new();
        for (row, line) in lines.iter().enumerate() {
            if let Some(eq_offset) = Self::find_clause_eq_in_line(line) {
                hits.push(ClauseHit {
                    line: row,
                    eq_byte_offset: eq_offset,
                });
            }
        }
        hits
    }

    fn group_consecutive(hits: &[ClauseHit], lines: &[Cow<'_, str>]) -> Vec<Vec<usize>> {
        if hits.is_empty() {
            return Vec::new();
        }
        let mut groups: Vec<Vec<usize>> = Vec::new();
        let mut current_group: Vec<usize> = vec![0];

        for i in 1..hits.len() {
            let prev_line = hits[i - 1].line;
            let curr_line = hits[i].line;
            let gap_is_clean = (prev_line + 1..curr_line).all(|row| {
                if row >= lines.len() {
                    return false;
                }
                let trimmed = lines[row].trim();
                trimmed.is_empty()
                    || trimmed.starts_with("//-")
                    || trimmed == "//"
            });
            if gap_is_clean && curr_line.saturating_sub(prev_line) <= 2 {
                current_group.push(i);
            } else {
                groups.push(std::mem::take(&mut current_group));
                current_group.push(i);
            }
        }
        if !current_group.is_empty() {
            groups.push(current_group);
        }
        groups.retain(|g| g.len() >= 2);
        groups
    }
}

impl Policy for DeclarationAlignmentPolicy {
    fn name(&self) -> &str {
        "declaration_alignment"
    }

    fn apply(&self, context: &PolicyContext<'_>) -> PolicyResult {
        let shared = context.shared.unwrap();
        let eol = shared.line_ending();
        let mut lines = shared.lines_cow();
        let trailing_newline = shared.trailing_newline();

        let hits = Self::scan_lines(&lines);
        if hits.is_empty() {
            return PolicyResult::unchanged();
        }

        let groups = Self::group_consecutive(&hits, &lines);
        if groups.is_empty() {
            return PolicyResult::unchanged();
        }

        let mut edits = Vec::new();

        for group in &groups {
            let mut max_content_len: usize = 0;
            for &hit_idx in group {
                let row = hits[hit_idx].line;
                if row >= lines.len() {
                    continue;
                }
                let line_text = &lines[row];
                let eq_offset = hits[hit_idx].eq_byte_offset;
                let prefix = &line_text[..eq_offset];
                let content_len = prefix.trim_end().len();
                if content_len > max_content_len {
                    max_content_len = content_len;
                }
            }

            let target_col = max_content_len + 1;

            for &hit_idx in group {
                let row = hits[hit_idx].line;
                if row >= lines.len() {
                    continue;
                }
                let line_text = lines[row].to_string();
                let eq_offset = hits[hit_idx].eq_byte_offset;

                let prefix_raw = &line_text[..eq_offset];
                let suffix = &line_text[eq_offset..];

                let content = prefix_raw.trim_end();
                let padding = target_col.saturating_sub(content.len());
                let new_line = format!("{}{}{}", content, " ".repeat(padding), suffix);

                if new_line != line_text {
                    lines[row] = Cow::Owned(new_line.clone());
                    edits.push(Edit {
                        policy: self.name().into(),
                        line: row + 1,
                        before: line_text,
                        after: new_line,
                    });
                }
            }
        }

        if edits.is_empty() {
            return PolicyResult::unchanged();
        }

        PolicyResult {
            text: join_lines_cow(&lines, eol, trailing_newline),
            changed: true,
            violations: vec![Violation {
                policy: self.name().into(),
                message: "aligned consecutive = delete/default declarations".to_string(),
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
    use std::path::PathBuf;
    use tree_sitter::Parser;

    use super::*;
    use crate::model::policy_context::PolicyContext;
    use crate::policy::shared_data::PolicySharedData;

    fn parse_cpp(text: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("cpp language");
        parser.parse(text, None).expect("parse tree")
    }

    #[test]
    fn aligns_delete_default_group() {
        let policy = DeclarationAlignmentPolicy::new();
        let path = PathBuf::from("test.hpp");
        let source = concat!(
            "struct X {\n",
            "    X(const X&) = delete;\n",
            "    X& operator=(const X&) = delete;\n",
            "    X(X&&) noexcept = default;\n",
            "    X& operator=(X&&) = default;\n",
            "};\n",
        );
        let tree = parse_cpp(source);
        let shared = PolicySharedData::new(source, None);
        let ctx = PolicyContext::new(source, &path)
            .with_tree(Some(&tree))
            .with_shared(Some(&shared));
        let result = policy.apply(&ctx);
        assert!(result.changed, "expected alignment changes");
        let result_lines: Vec<&str> = result.text.lines().collect();
        let eq_positions: Vec<usize> = result_lines[1..5]
            .iter()
            .filter_map(|line| line.find("= "))
            .collect();
        assert_eq!(eq_positions.len(), 4, "should find 4 aligned lines");
        assert!(
            eq_positions.windows(2).all(|w| w[0] == w[1]),
            "all = signs should be at the same column, got {:?}",
            eq_positions,
        );
    }

    #[test]
    fn no_change_when_already_aligned() {
        let policy = DeclarationAlignmentPolicy::new();
        let path = PathBuf::from("test.hpp");
        let source = concat!(
            "struct X {\n",
            "    X(const X&)            = delete;\n",
            "    X& operator=(const X&) = delete;\n",
            "};\n",
        );
        let tree = parse_cpp(source);
        let shared = PolicySharedData::new(source, None);
        let ctx = PolicyContext::new(source, &path)
            .with_tree(Some(&tree))
            .with_shared(Some(&shared));
        let result = policy.apply(&ctx);
        assert!(!result.changed);
    }

    #[test]
    fn single_line_no_change() {
        let policy = DeclarationAlignmentPolicy::new();
        let path = PathBuf::from("test.hpp");
        let source = "struct X { X(const X&) = delete; };\n";
        let tree = parse_cpp(source);
        let shared = PolicySharedData::new(source, None);
        let ctx = PolicyContext::new(source, &path)
            .with_tree(Some(&tree))
            .with_shared(Some(&shared));
        let result = policy.apply(&ctx);
        assert!(!result.changed, "single delete should not trigger alignment");
    }

    #[test]
    fn comment_separator_breaks_group() {
        let policy = DeclarationAlignmentPolicy::new();
        let path = PathBuf::from("test.hpp");
        let source = concat!(
            "struct X {\n",
            "    X(const X&) = delete;\n",
            "    // other stuff\n",
            "    X(X&&) = default;\n",
            "};\n",
        );
        let tree = parse_cpp(source);
        let shared = PolicySharedData::new(source, None);
        let ctx = PolicyContext::new(source, &path)
            .with_tree(Some(&tree))
            .with_shared(Some(&shared));
        let result = policy.apply(&ctx);
        assert!(!result.changed, "non-dash comment should break group");
    }

    #[test]
    fn dash_separator_preserves_group() {
        let policy = DeclarationAlignmentPolicy::new();
        let path = PathBuf::from("test.hpp");
        let source = concat!(
            "struct X {\n",
            "    X(const X&) = delete;\n",
            "    //--------------------------\n",
            "    X& operator=(const X&) = delete;\n",
            "};\n",
        );
        let tree = parse_cpp(source);
        let shared = PolicySharedData::new(source, None);
        let ctx = PolicyContext::new(source, &path)
            .with_tree(Some(&tree))
            .with_shared(Some(&shared));
        let result = policy.apply(&ctx);
        assert!(result.changed, "dash separator should not break group");
        let result_lines: Vec<&str> = result.text.lines().collect();
        let eq1 = result_lines[1].find("= ").unwrap();
        let eq2 = result_lines[3].find("= ").unwrap();
        assert_eq!(eq1, eq2, "= signs should be aligned across dash separator");
    }

    #[test]
    fn skips_comment_lines() {
        let policy = DeclarationAlignmentPolicy::new();
        let path = PathBuf::from("test.hpp");
        let source = concat!(
            "// X(const X&) = delete;\n",
            "// X& operator=(const X&) = delete;\n",
        );
        let tree = parse_cpp(source);
        let shared = PolicySharedData::new(source, None);
        let ctx = PolicyContext::new(source, &path)
            .with_tree(Some(&tree))
            .with_shared(Some(&shared));
        let result = policy.apply(&ctx);
        assert!(!result.changed, "should skip commented-out lines");
    }
}
