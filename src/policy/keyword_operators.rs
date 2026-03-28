use std::ops::Range;

use tree_sitter::{Node, StreamingIterator};

use crate::model::edit::Edit;
use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::model::context_query::SemanticContextQuery;
use crate::model::violation::Violation;
use crate::parser::file_context::SemanticScopeKind;
use crate::parser::query_cache::TsQueryCache;
use crate::policy::Policy;
use crate::parser::text_scan;
use crate::parser::ts_cpp_symbols;

struct Replacement {
    start: usize,
    end: usize,
    replacement: String,
    line: usize,
    before: &'static str,
}

pub struct LogicalKeywordOperatorsPolicy {
    replace_and: bool,
    replace_or: bool,
    skip_preprocessor: bool,
}

impl LogicalKeywordOperatorsPolicy {
    pub fn new(replace_and: bool, replace_or: bool, skip_preprocessor: bool) -> Self {
        Self {
            replace_and,
            replace_or,
            skip_preprocessor,
        }
    }

    const PROTECTED_QUERY_NO_PREPROC: &str = r#"[
        (comment) @p
        (string_literal) @p
        (raw_string_literal) @p
        (char_literal) @p
        (system_lib_string) @p
        (concatenated_string) @p
    ]"#;

    const PROTECTED_QUERY_WITH_PREPROC: &str = r#"[
        (comment) @p
        (string_literal) @p
        (raw_string_literal) @p
        (char_literal) @p
        (system_lib_string) @p
        (concatenated_string) @p
        (preproc_if) @p
        (preproc_ifdef) @p
        (preproc_elif) @p
        (preproc_else) @p
        (preproc_include) @p
        (preproc_def) @p
        (preproc_function_def) @p
    ]"#;

    fn collect_protected_ranges(
        root: Node<'_>,
        skip_preprocessor: bool,
        query_cache: Option<&TsQueryCache>,
        source: &[u8],
    ) -> Vec<Range<usize>> {
        let mut protected = Vec::<Range<usize>>::new();

        let pattern = if skip_preprocessor {
            Self::PROTECTED_QUERY_WITH_PREPROC
        } else {
            Self::PROTECTED_QUERY_NO_PREPROC
        };

        let cached = query_cache
            .and_then(|qc| qc.get_or_compile(pattern).ok());
        let language: tree_sitter::Language = tree_sitter_cpp::LANGUAGE.into();
        let direct = if cached.is_none() {
            tree_sitter::Query::new(&language, pattern).ok()
        } else {
            None
        };
        if let Some(query) = cached.as_deref().or(direct.as_ref()) {
            let mut cursor = tree_sitter::QueryCursor::new();
            let mut matches = cursor.matches(query, root, source);
            while let Some(m) = {
                matches.advance();
                matches.get()
            } {
                for capture in m.captures {
                    protected.push(capture.node.start_byte()..capture.node.end_byte());
                }
            }
        }

        if protected.is_empty() {
            return protected;
        }
        protected.sort_by_key(|range| range.start);
        let mut merged = Vec::<Range<usize>>::with_capacity(protected.len());
        let mut current = protected[0].clone();
        for range in protected.into_iter().skip(1) {
            if range.start <= current.end {
                current.end = current.end.max(range.end);
            } else {
                merged.push(current);
                current = range;
            }
        }
        merged.push(current);
        merged
    }



    fn line_for_offset(line_starts: &[usize], offset: usize) -> usize {
        match line_starts.binary_search(&offset) {
            Ok(idx) => idx + 1,
            Err(idx) => idx.max(1),
        }
    }

    fn needs_left_space(bytes: &[u8], start: usize) -> bool {
        if start == 0 {
            return false;
        }
        let prev = bytes[start - 1];
        !(prev.is_ascii_whitespace() || matches!(prev, b'(' | b'[' | b'{'))
    }

    fn needs_right_space(bytes: &[u8], end: usize) -> bool {
        if end >= bytes.len() {
            return false;
        }
        let next = bytes[end];
        !(next.is_ascii_whitespace()
            || matches!(next, b')' | b']' | b'}' | b';' | b',' | b':' | b'?'))
    }

    fn build_keyword_replacement(bytes: &[u8], start: usize, end: usize, keyword: &str) -> String {
        let mut result = String::with_capacity(keyword.len() + 2);
        if Self::needs_left_space(bytes, start) {
            result.push(' ');
        }
        result.push_str(keyword);
        if Self::needs_right_space(bytes, end) {
            result.push(' ');
        }
        result
    }

    fn contains_offset(ranges: &[Range<usize>], offset: usize) -> bool {
        let mut left = 0usize;
        let mut right = ranges.len();
        while left < right {
            let mid = left + (right - left) / 2;
            let range = &ranges[mid];
            if offset < range.start {
                right = mid;
            } else if offset >= range.end {
                left = mid + 1;
            } else {
                return true;
            }
        }
        false
    }

    const LOGICAL_OP_QUERY: &str = r#"(binary_expression ["&&" "||"] @op)"#;

    fn collect_replacements_from_tree(
        &self,
        text: &str,
        root: Node<'_>,
        protected: &[Range<usize>],
        semantic_query: &SemanticContextQuery<'_>,
        query_cache: Option<&TsQueryCache>,
    ) -> (Vec<Replacement>, usize) {
        let bytes = text.as_bytes();
        if bytes.len() < 2 {
            return (Vec::new(), 0);
        }
        let line_starts = text_scan::line_starts(text, true);
        let mut replacements = Vec::<Replacement>::new();
        let mut skipped_semantic_unsafe = 0usize;

        let process_node =
            |node: Node<'_>,
             replacements: &mut Vec<Replacement>,
             skipped: &mut usize| {
                let kid = node.kind_id();
                if (!self.replace_and || kid != ts_cpp_symbols::anon_sym_AMP_AMP)
                    && (!self.replace_or || kid != ts_cpp_symbols::anon_sym_PIPE_PIPE)
                {
                    return;
                }
                let start = node.start_byte();
                let end = node.end_byte();
                if start >= end || end > bytes.len() {
                    return;
                }
                if Self::contains_offset(protected, start) {
                    return;
                }
                let line = Self::line_for_offset(line_starts.as_slice(), start);
                let column = node.start_position().column + 1;
                if semantic_query.is_available()
                    && semantic_query
                        .scope_at(line, column)
                        .is_some_and(|scope| scope.kind == SemanticScopeKind::Preprocessor)
                {
                    *skipped = skipped.saturating_add(1);
                    return;
                }
                if semantic_query.is_available() && semantic_query.is_macro_region(line, column) {
                    *skipped = skipped.saturating_add(1);
                    return;
                }
                let replacement = if kid == ts_cpp_symbols::anon_sym_AMP_AMP {
                    Self::build_keyword_replacement(bytes, start, end, "and")
                } else {
                    Self::build_keyword_replacement(bytes, start, end, "or")
                };
                replacements.push(Replacement {
                    start,
                    end,
                    replacement,
                    line,
                    before: if kid == ts_cpp_symbols::anon_sym_AMP_AMP { "&&" } else { "||" },
                });
            };

        let cached = query_cache
            .and_then(|qc| qc.get_or_compile(Self::LOGICAL_OP_QUERY).ok());
        let language: tree_sitter::Language = tree_sitter_cpp::LANGUAGE.into();
        let direct = if cached.is_none() {
            tree_sitter::Query::new(&language, Self::LOGICAL_OP_QUERY).ok()
        } else {
            None
        };
        if let Some(query) = cached.as_deref().or(direct.as_ref()) {
            let mut cursor = tree_sitter::QueryCursor::new();
            let mut matches = cursor.matches(query, root, text.as_bytes());
            while let Some(m) = {
                matches.advance();
                matches.get()
            } {
                for capture in m.captures {
                    process_node(
                        capture.node,
                        &mut replacements,
                        &mut skipped_semantic_unsafe,
                    );
                }
            }
        }

        replacements.sort_by_key(|item| (item.start, item.end));
        (replacements, skipped_semantic_unsafe)
    }
}

impl Policy for LogicalKeywordOperatorsPolicy {
    fn name(&self) -> &str {
        "logical_keyword_operators"
    }
    fn apply(&self, context: &PolicyContext<'_>) -> PolicyResult {
        if !self.replace_and && !self.replace_or {
            return PolicyResult::unchanged();
        }

        let Some(tree) = context.tree_sitter_tree else {
            return PolicyResult::unchanged_with_warning(
                "logical_keyword_operators: tree-sitter context unavailable".to_string(),
            );
        };

        if context.has_fatal_diags() {
            return PolicyResult::unchanged_with_warning(format!(
                "logical_keyword_operators: skipped due fatal clang diagnostics (fatal={})",
                context.fatal_diag_count()
            ));
        }

        let semantic_query = context.semantic_query();
        if !semantic_query.is_available() {
            return PolicyResult::unchanged_with_warning(
                "logical_keyword_operators: semantic context unavailable; skipping accuracy-unsafe replacement"
                    .to_string(),
            );
        }

        let root = tree.root_node();
        let protected =
            Self::collect_protected_ranges(root, self.skip_preprocessor, context.query_cache, context.text.as_bytes());
        let (replacements, skipped_semantic_unsafe) = self.collect_replacements_from_tree(
            context.text,
            root,
            protected.as_slice(),
            &semantic_query,
            context.query_cache,
        );
        if replacements.is_empty() {
            return if skipped_semantic_unsafe > 0 {
                PolicyResult::unchanged_with_warning(format!(
                    "logical_keyword_operators: skipped {} semantic-unsafe replacement(s)",
                    skipped_semantic_unsafe
                ))
            } else {
                PolicyResult::unchanged()
            };
        }

        let mut rewritten = String::with_capacity(context.text.len() + replacements.len() * 2);
        let mut cursor = 0usize;
        let mut edits = Vec::<Edit>::with_capacity(replacements.len());
        for replacement in replacements {
            rewritten.push_str(&context.text[cursor..replacement.start]);
            rewritten.push_str(&replacement.replacement);
            cursor = replacement.end;
            edits.push(Edit {
                policy: self.name().into(),
                line: replacement.line,
                before: replacement.before.to_string(),
                after: replacement.replacement,
            });
        }
        rewritten.push_str(&context.text[cursor..]);

        let mut warnings = Vec::new();
        if skipped_semantic_unsafe > 0 {
            warnings.push(format!(
                "logical_keyword_operators: skipped {} semantic-unsafe replacement(s)",
                skipped_semantic_unsafe
            ));
        }

        PolicyResult {
            text: rewritten,
            changed: true,
            violations: vec![Violation {
                policy: self.name().into(),
                message: "replaced logical operators with keyword forms where safe".to_string(),
                line: edits[0].line,
                column: Some(1),
            }],
            edits,
            warnings,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tree_sitter::Parser;

    use crate::model::policy_context::PolicyContext;
    use crate::parser::file_context::SemanticFileContext;
    use crate::policy::keyword_operators::LogicalKeywordOperatorsPolicy;
    use crate::policy::Policy;

    fn parse_cpp(text: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("cpp language");
        parser.parse(text, None).expect("parse tree")
    }

    #[test]
    fn replaces_logical_ops() {
        let policy = LogicalKeywordOperatorsPolicy::new(true, true, true);
        let path = PathBuf::from("sample.cpp");
        let source = "bool ok = a&&b || c;\n";
        let tree = parse_cpp(source);
        let semantic = SemanticFileContext::default();
        let ctx = PolicyContext::new(source, &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "bool ok = a and b or c;\n");
    }

    #[test]
    fn skips_preprocessor_regions() {
        let policy = LogicalKeywordOperatorsPolicy::new(true, true, true);
        let path = PathBuf::from("sample.cpp");
        let source = "#if defined(A) && defined(B)\nint x = a&&b;\n#endif\n";
        let tree = parse_cpp(source);
        let semantic = SemanticFileContext::default();
        let ctx = PolicyContext::new(source, &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic));
        let result = policy.apply(&ctx);
        assert!(!result.changed);
    }

    #[test]
    fn preserves_comments_strings() {
        let policy = LogicalKeywordOperatorsPolicy::new(true, true, true);
        let path = PathBuf::from("sample.cpp");
        let source = "const char* s = \"a&&b || c\"; // keep && ||\nbool x = a||b;\n";
        let tree = parse_cpp(source);
        let semantic = SemanticFileContext::default();
        let ctx = PolicyContext::new(source, &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic));
        let result = policy.apply(&ctx);
        assert_eq!(
            result.text,
            "const char* s = \"a&&b || c\"; // keep && ||\nbool x = a or b;\n"
        );
    }

    #[test]
    fn keeps_rvalue_tokens() {
        let policy = LogicalKeywordOperatorsPolicy::new(true, true, true);
        let path = PathBuf::from("sample.hpp");
        let source =
            "struct A {\n  A(A&& other) noexcept;\n  A& operator=(A&& other) noexcept;\n};\n";
        let tree = parse_cpp(source);
        let semantic = SemanticFileContext::default();
        let ctx = PolicyContext::new(source, &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic));
        let result = policy.apply(&ctx);
        assert!(!result.changed);
    }

    #[test]
    fn keeps_operator_overload() {
        let policy = LogicalKeywordOperatorsPolicy::new(true, true, true);
        let path = PathBuf::from("sample.hpp");
        let source = "struct A {\n  bool operator&&(const A& rhs) const;\n};\n";
        let tree = parse_cpp(source);
        let semantic = SemanticFileContext::default();
        let ctx = PolicyContext::new(source, &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic));
        let result = policy.apply(&ctx);
        assert!(!result.changed);
    }

    #[test]
    fn applies_header_files() {
        let policy = LogicalKeywordOperatorsPolicy::new(true, true, true);
        let path = PathBuf::from("sample.hpp");
        let source = "bool ok = a && b || c;\n";
        let tree = parse_cpp(source);
        let semantic = SemanticFileContext::default();
        let ctx = PolicyContext::new(source, &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "bool ok = a and b or c;\n");
        assert_eq!(result.edits.len(), 2);
    }
}
