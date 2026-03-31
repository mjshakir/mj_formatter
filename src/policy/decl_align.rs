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

    fn find_assignment_eq_in_line(line: &str) -> Option<usize> {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with('#') {
            return None;
        }
        let bytes = line.as_bytes();
        let len = bytes.len();
        let mut paren_depth: i32 = 0;
        let mut angle_depth: i32 = 0;
        let mut brace_depth: i32 = 0;
        let mut in_string = false;
        let mut in_char = false;
        let mut i = 0;
        while i < len {
            let b = bytes[i];
            if in_string {
                if b == b'\\' {
                    i += 1;
                } else if b == b'"' {
                    in_string = false;
                }
                i += 1;
                continue;
            }
            if in_char {
                if b == b'\\' {
                    i += 1;
                } else if b == b'\'' {
                    in_char = false;
                }
                i += 1;
                continue;
            }
            match b {
                b'"' => in_string = true,
                b'\'' => in_char = true,
                b'(' => paren_depth += 1,
                b')' => paren_depth -= 1,
                b'{' => brace_depth += 1,
                b'}' => brace_depth -= 1,
                b'<' if paren_depth == 0 && brace_depth == 0 => {
                    if i + 1 < len && bytes[i + 1] == b'<' {
                        // << operator, skip both
                        i += 2;
                        continue;
                    }
                    angle_depth += 1;
                }
                b'>' if paren_depth == 0 && brace_depth == 0 && angle_depth > 0 => {
                    if i + 1 < len && bytes[i + 1] == b'>' {
                        // >> closing two levels
                        angle_depth = (angle_depth - 2).max(0);
                        i += 2;
                        continue;
                    }
                    angle_depth -= 1;
                }
                b'=' if paren_depth == 0 && angle_depth <= 0 && brace_depth == 0 => {
                    let next = if i + 1 < len { bytes[i + 1] } else { 0 };
                    if next == b'=' {
                        i += 2;
                        continue;
                    }
                    let prev = if i > 0 { bytes[i - 1] } else { 0 };
                    if matches!(prev, b'!' | b'<' | b'>' | b'+' | b'-' | b'*' | b'/' | b'%' | b'&' | b'|' | b'^') {
                        i += 1;
                        continue;
                    }
                    // Skip = delete / = default (handled by clause path)
                    let after = line[i + 1..].trim_start();
                    if after.starts_with("delete") || after.starts_with("default") {
                        i += 1;
                        continue;
                    }
                    return Some(i);
                }
                _ => {}
            }
            i += 1;
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

    fn scan_assignment_lines(lines: &[Cow<'_, str>]) -> Vec<ClauseHit> {
        let mut hits = Vec::new();
        for (row, line) in lines.iter().enumerate() {
            if let Some(eq_offset) = Self::find_assignment_eq_in_line(line) {
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

    fn align_group(
        group: &[usize],
        hits: &[ClauseHit],
        lines: &mut [Cow<'_, str>],
        edits: &mut Vec<Edit>,
        policy_name: &str,
    ) {
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
                    policy: policy_name.into(),
                    line: row + 1,
                    before: line_text,
                    after: new_line,
                });
            }
        }
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

        let mut all_edits = Vec::new();

        // Pass 1: = delete / = default alignment
        let clause_hits = Self::scan_lines(&lines);
        if !clause_hits.is_empty() {
            let groups = Self::group_consecutive(&clause_hits, &lines);
            for group in &groups {
                Self::align_group(group, &clause_hits, &mut lines, &mut all_edits, self.name());
            }
        }

        // Pass 2: general = initialization/assignment alignment
        let assign_hits = Self::scan_assignment_lines(&lines);
        if !assign_hits.is_empty() {
            let groups = Self::group_consecutive(&assign_hits, &lines);
            for group in &groups {
                Self::align_group(group, &assign_hits, &mut lines, &mut all_edits, self.name());
            }
        }

        if all_edits.is_empty() {
            return PolicyResult::unchanged();
        }

        let first_line = all_edits[0].line;
        let has_clause_edits = clause_hits.iter().any(|h| all_edits.iter().any(|e| e.line == h.line + 1));
        let has_assign_edits = assign_hits.iter().any(|h| all_edits.iter().any(|e| e.line == h.line + 1));
        let message = match (has_clause_edits, has_assign_edits) {
            (true, true) => "aligned consecutive declarations and initializations",
            (true, false) => "aligned consecutive = delete/default declarations",
            (false, true) => "aligned consecutive = initializations",
            _ => "aligned consecutive declarations",
        };

        PolicyResult {
            text: join_lines_cow(&lines, eol, trailing_newline),
            changed: true,
            violations: vec![Violation {
                policy: self.name().into(),
                message: message.to_string(),
                line: first_line,
                column: Some(1),
            }],
            edits: all_edits,
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

    fn run_policy(source: &str) -> PolicyResult {
        let policy = DeclarationAlignmentPolicy::new();
        let path = PathBuf::from("test.hpp");
        let tree = parse_cpp(source);
        let shared = PolicySharedData::new(source, None);
        let ctx = PolicyContext::new(source, &path)
            .with_tree(Some(&tree))
            .with_shared(Some(&shared));
        policy.apply(&ctx)
    }

    // ── Existing tests (= delete / = default) ────────────────────

    #[test]
    fn aligns_delete_default_group() {
        let source = concat!(
            "struct X {\n",
            "    X(const X&) = delete;\n",
            "    X& operator=(const X&) = delete;\n",
            "    X(X&&) noexcept = default;\n",
            "    X& operator=(X&&) = default;\n",
            "};\n",
        );
        let result = run_policy(source);
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
        let source = concat!(
            "struct X {\n",
            "    X(const X&)            = delete;\n",
            "    X& operator=(const X&) = delete;\n",
            "};\n",
        );
        let result = run_policy(source);
        assert!(!result.changed);
    }

    #[test]
    fn single_line_no_change() {
        let source = "struct X { X(const X&) = delete; };\n";
        let result = run_policy(source);
        assert!(!result.changed, "single delete should not trigger alignment");
    }

    #[test]
    fn comment_separator_breaks_group() {
        let source = concat!(
            "struct X {\n",
            "    X(const X&) = delete;\n",
            "    // other stuff\n",
            "    X(X&&) = default;\n",
            "};\n",
        );
        let result = run_policy(source);
        assert!(!result.changed, "non-dash comment should break group");
    }

    #[test]
    fn dash_separator_preserves_group() {
        let source = concat!(
            "struct X {\n",
            "    X(const X&) = delete;\n",
            "    //--------------------------\n",
            "    X& operator=(const X&) = delete;\n",
            "};\n",
        );
        let result = run_policy(source);
        assert!(result.changed, "dash separator should not break group");
        let result_lines: Vec<&str> = result.text.lines().collect();
        let eq1 = result_lines[1].find("= ").unwrap();
        let eq2 = result_lines[3].find("= ").unwrap();
        assert_eq!(eq1, eq2, "= signs should be aligned across dash separator");
    }

    #[test]
    fn skips_comment_lines() {
        let source = concat!(
            "// X(const X&) = delete;\n",
            "// X& operator=(const X&) = delete;\n",
        );
        let result = run_policy(source);
        assert!(!result.changed, "should skip commented-out lines");
    }

    // ── New tests (general = alignment) ──────────────────────────

    #[test]
    fn aligns_consecutive_assignments() {
        let source = concat!(
            "{\n",
            "    int short_name = 1;\n",
            "    int very_long_variable_name = 2;\n",
            "    int mid = 3;\n",
            "}\n",
        );
        let result = run_policy(source);
        assert!(result.changed, "expected alignment changes");
        let result_lines: Vec<&str> = result.text.lines().collect();
        let eq_positions: Vec<usize> = result_lines[1..4]
            .iter()
            .filter_map(|line| line.find(" = "))
            .collect();
        assert_eq!(eq_positions.len(), 3);
        assert!(
            eq_positions.windows(2).all(|w| w[0] == w[1]),
            "all = signs should be at the same column, got {:?}",
            eq_positions,
        );
    }

    #[test]
    fn skips_compound_operators() {
        let source = concat!(
            "{\n",
            "    x += 1;\n",
            "    y -= 2;\n",
            "    z = 3;\n",
            "}\n",
        );
        let result = run_policy(source);
        assert!(!result.changed, "compound operators should not be aligned");
    }

    #[test]
    fn skips_eq_in_parentheses() {
        let source = concat!(
            "if (x = foo()) {\n",
            "while (y = bar()) {\n",
        );
        let result = run_policy(source);
        assert!(!result.changed, "= inside parens should not be detected");
    }

    #[test]
    fn post_rename_alignment_fix() {
        let source = concat!(
            "{\n",
            "    const IndexType _c_capacity        = get_capacity();\n",
            "    const IndexType _c_mask_count      = get_mask_count();\n",
            "    const size_t _c_capacity_size      = static_cast<size_t>(_c_capacity);\n",
            "    const size_t _c_mask_count_size    = static_cast<size_t>(_c_mask_count);\n",
            "}\n",
        );
        let result = run_policy(source);
        assert!(result.changed, "post-rename misalignment should be fixed");
        let result_lines: Vec<&str> = result.text.lines().collect();
        let eq_positions: Vec<usize> = result_lines[1..5]
            .iter()
            .filter_map(|line| line.find(" = "))
            .collect();
        assert_eq!(eq_positions.len(), 4);
        assert!(
            eq_positions.windows(2).all(|w| w[0] == w[1]),
            "all = signs should be at the same column after fix, got {:?}",
            eq_positions,
        );
    }

    #[test]
    fn delete_and_assignment_groups_stay_separate() {
        let source = concat!(
            "struct X {\n",
            "    X(const X&)    = delete;\n",
            "    X(X&&)         = default;\n",
            "    int x = 1;\n",
            "    int longer_name = 2;\n",
            "};\n",
        );
        let result = run_policy(source);
        assert!(result.changed);
        let result_lines: Vec<&str> = result.text.lines().collect();
        // delete/default group (lines 1-2) should be aligned independently
        let del_eq1 = result_lines[1].find("= delete").unwrap();
        let del_eq2 = result_lines[2].find("= default").unwrap();
        assert_eq!(del_eq1, del_eq2, "delete/default = should be aligned");
        // assignment group (lines 3-4) should be aligned independently
        let asgn_eq1 = result_lines[3].find(" = ").unwrap();
        let asgn_eq2 = result_lines[4].find(" = ").unwrap();
        assert_eq!(asgn_eq1, asgn_eq2, "assignment = should be aligned");
        // The two groups should NOT be at the same column
        assert_ne!(del_eq1, asgn_eq1, "groups should be independent");
    }

    #[test]
    fn assignment_group_across_dash_separator() {
        let source = concat!(
            "{\n",
            "    int short = 1;\n",
            "    //--------------------------\n",
            "    int very_long_name = 2;\n",
            "}\n",
        );
        let result = run_policy(source);
        assert!(result.changed);
        let result_lines: Vec<&str> = result.text.lines().collect();
        let eq1 = result_lines[1].find(" = ").unwrap();
        let eq2 = result_lines[3].find(" = ").unwrap();
        assert_eq!(eq1, eq2, "= should be aligned across dash separator");
    }

    #[test]
    fn skips_eq_in_templates() {
        let source = concat!(
            "template<int N = 0>\n",
            "int short = 1;\n",
            "int very_long_name = 2;\n",
        );
        let result = run_policy(source);
        assert!(result.changed, "template line skipped, other two should align");
        let result_lines: Vec<&str> = result.text.lines().collect();
        let eq1 = result_lines[1].find(" = ").unwrap();
        let eq2 = result_lines[2].find(" = ").unwrap();
        assert_eq!(eq1, eq2, "non-template lines should be aligned");
    }

    #[test]
    fn no_change_assignments_already_aligned() {
        let source = concat!(
            "{\n",
            "    const int x    = 1;\n",
            "    const int long = 2;\n",
            "}\n",
        );
        let result = run_policy(source);
        assert!(!result.changed, "already aligned should produce no changes");
    }

    #[test]
    fn single_assignment_no_change() {
        let source = "int x = 42;\n";
        let result = run_policy(source);
        assert!(!result.changed, "single line should not trigger alignment");
    }
}
