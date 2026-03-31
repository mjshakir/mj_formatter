use tree_sitter::{Node, StreamingIterator};

use crate::model::edit::Edit;
use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::model::violation::Violation;
use crate::parser::file_context::SemanticFileContext;
use crate::parser::query_cache::TsQueryCache;
use crate::parser::ts_cpp_symbols;
use crate::parser::ts_traversal;
use crate::policy::Policy;

pub struct NumericLiteralSuffixPolicy;

impl NumericLiteralSuffixPolicy {
    pub fn new() -> Self {
        Self
    }

    fn suffix_from_type_kind(kind: i32) -> Option<(&'static str, bool)> {
        match kind {
            clang_sys::CXType_Float => Some(("f", true)),
            clang_sys::CXType_Double => Some(("", true)),
            clang_sys::CXType_LongDouble => Some(("L", true)),
            clang_sys::CXType_Float16 | clang_sys::CXType_Half => Some(("", true)),
            clang_sys::CXType_Float128 => Some(("Q", true)),
            clang_sys::CXType_SChar | clang_sys::CXType_Short | clang_sys::CXType_Int => Some(("", false)),
            clang_sys::CXType_Long => Some(("L", false)),
            clang_sys::CXType_LongLong => Some(("LL", false)),
            clang_sys::CXType_UChar | clang_sys::CXType_UShort | clang_sys::CXType_UInt => Some(("U", false)),
            clang_sys::CXType_ULong => Some(("UL", false)),
            clang_sys::CXType_ULongLong => Some(("ULL", false)),
            clang_sys::CXType_Int128 => Some(("LL", false)),
            clang_sys::CXType_UInt128 => Some(("ULL", false)),
            _ => None,
        }
    }

    fn suffix_from_ts_type_node(type_node: &Node<'_>, source: &[u8]) -> Option<(&'static str, bool)> {
        let kind_id = type_node.kind_id();
        let text = type_node.utf8_text(source).ok()?;

        if kind_id == ts_cpp_symbols::sym_primitive_type {
            match text {
                "float" => Some(("f", true)),
                "double" => Some(("", true)),
                "int" | "short" | "char" => Some(("", false)),
                _ => None,
            }
        } else if kind_id == ts_cpp_symbols::sym_sized_type_specifier {
            let mut unsigned = false;
            let mut long_count: usize = 0;
            let mut has_double = false;
            let mut has_short = false;
            for i in 0..type_node.child_count() {
                let Some(child) = type_node.child(i as u32) else {
                    continue;
                };
                let Ok(child_text) = child.utf8_text(source) else {
                    continue;
                };
                match child_text {
                    "unsigned" => unsigned = true,
                    "long" => long_count += 1,
                    "short" => has_short = true,
                    "double" => has_double = true,
                    _ => {}
                }
            }
            if has_double {
                return Some((if long_count >= 1 { "L" } else { "" }, true));
            }
            let suffix = match (unsigned, long_count) {
                (_, 0) if has_short => {
                    if unsigned { "U" } else { "" }
                }
                (false, 0) => "",
                (false, 1) => "L",
                (false, 2) => "LL",
                (true, 0) => "U",
                (true, 1) => "UL",
                (true, 2) => "ULL",
                _ => return None,
            };
            Some((suffix, false))
        } else {
            None
        }
    }

    fn declarator_name<'a>(decl_node: &Node<'a>, source: &[u8]) -> Option<String> {
        for i in 0..decl_node.named_child_count() {
            let Some(child) = decl_node.named_child(i as u32) else { continue };
            if child.kind_id() == ts_cpp_symbols::sym_init_declarator {
                if let Some(declarator) = child.child_by_field_id(ts_cpp_symbols::field_declarator) {
                    if declarator.kind_id() == ts_cpp_symbols::sym_identifier {
                        return declarator.utf8_text(source).ok().map(|s| s.to_string());
                    }
                }
            }
            if child.kind_id() == ts_cpp_symbols::alias_sym_field_identifier {
                return child.utf8_text(source).ok().map(|s| s.to_string());
            }
        }
        None
    }

    fn resolve_suffix(
        decl_node: &Node<'_>,
        source: &[u8],
        semantic: Option<&SemanticFileContext>,
    ) -> Option<(&'static str, bool)> {
        let line = decl_node.start_position().row + 1;

        if let Some(ctx) = semantic {
            if let Some(name) = Self::declarator_name(decl_node, source) {
                if let Some(decl) = ctx.symbol_on_line(&name, line, &[])
                    .filter(|d| {
                        d.kind == clang_sys::CXCursor_VarDecl || d.kind == clang_sys::CXCursor_FieldDecl
                    })
                {
                    if let Some(result) = Self::suffix_from_type_kind(decl.canonical_type_kind) {
                        return Some(result);
                    }
                }
            }
        }

        if let Some(type_node) = Self::find_type_node(decl_node) {
            return Self::suffix_from_ts_type_node(&type_node, source);
        }

        None
    }

    fn is_plausible_number_literal(text: &str) -> bool {
        let bytes = text.as_bytes();
        if bytes.is_empty() {
            return false;
        }
        if !bytes[0].is_ascii_digit() {
            return false;
        }
        bytes.iter().all(|&b| matches!(b,
            b'0'..=b'9' | b'a'..=b'f' | b'A'..=b'F'
            | b'x' | b'X'
            | b'u' | b'U' | b'l' | b'L'
            | b'p' | b'P'
            | b'.' | b'\'' | b'+' | b'-'
        ))
    }

    fn is_float_literal(literal: &str) -> bool {
        if literal.len() >= 2 {
            let prefix = literal[..2].to_ascii_lowercase();
            if prefix == "0x" || prefix == "0b" {
                return false;
            }
        }
        literal.chars().any(|c| c == '.' || c == 'e' || c == 'E')
    }

    fn strip_suffix(literal: &str) -> &str {
        let bytes = literal.as_bytes();
        if bytes.is_empty() {
            return literal;
        }
        let is_hex_or_bin = literal.len() >= 2 && {
            let p = literal[..2].to_ascii_lowercase();
            p == "0x" || p == "0b"
        };
        if is_hex_or_bin {
            let n = bytes
                .iter()
                .rev()
                .take_while(|&&b| matches!(b, b'u' | b'U' | b'l' | b'L'))
                .count();
            &literal[..literal.len() - n]
        } else if Self::is_float_literal(literal) {
            let last = bytes[bytes.len() - 1];
            if matches!(last, b'f' | b'F' | b'l' | b'L') {
                &literal[..literal.len() - 1]
            } else {
                literal
            }
        } else {
            let n = bytes
                .iter()
                .rev()
                .take_while(|&&b| matches!(b, b'u' | b'U' | b'l' | b'L'))
                .count();
            &literal[..literal.len() - n]
        }
    }

    fn normalize_float_decimal(s: &str) -> String {
        let dot_pos = match s.find('.') {
            Some(p) => p,
            None => return s.to_string(),
        };
        let exp_pos = s[dot_pos..]
            .find(['e', 'E'])
            .map(|p| dot_pos + p)
            .unwrap_or(s.len());
        let decimal_digits = &s[dot_pos + 1..exp_pos];
        let trimmed = decimal_digits.trim_end_matches('0');
        let mut result = String::with_capacity(s.len());
        result.push_str(&s[..dot_pos + 1]);
        result.push_str(trimmed);
        result.push_str(&s[exp_pos..]);
        result
    }

    fn rewrite_literal(literal: &str, suffix: &str, is_float: bool) -> Option<String> {
        if Self::is_float_literal(literal) != is_float {
            return None;
        }
        let number_part = Self::strip_suffix(literal);
        let normalized = if is_float && !suffix.is_empty() {
            Self::normalize_float_decimal(number_part)
        } else {
            number_part.to_string()
        };
        let new_literal = format!("{normalized}{suffix}");
        (new_literal != literal).then_some(new_literal)
    }

    fn is_type_node_kind(id: u16) -> bool {
        ts_cpp_symbols::is_type_specifier(id)
    }

    fn find_type_node<'a>(node: &Node<'a>) -> Option<Node<'a>> {
        if let Some(type_node) = node.child_by_field_id(ts_cpp_symbols::field_type) {
            if Self::is_type_node_kind(type_node.kind_id()) {
                return Some(type_node);
            }
        }
        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i as u32) else {
                continue;
            };
            if Self::is_type_node_kind(child.kind_id()) {
                return Some(child);
            }
        }
        None
    }

    fn extract_type_text(node: &Node<'_>, source: &[u8]) -> Option<String> {
        let type_node = Self::find_type_node(node)?;
        let text = type_node.utf8_text(source).ok()?;
        Some(text.split_whitespace().collect::<Vec<_>>().join(" "))
    }

    fn collect_number_literals(node: Node<'_>) -> Vec<Node<'_>> {
        let language: tree_sitter::Language = tree_sitter_cpp::LANGUAGE.into();
        if let Ok(query) = tree_sitter::Query::new(&language, "(number_literal) @num") {
            let mut cursor = tree_sitter::QueryCursor::new();
            let empty: &[u8] = &[];
            let mut result = Vec::new();
            let mut matches = cursor.matches(&query, node, empty);
            while let Some(m) = {
                matches.advance();
                matches.get()
            } {
                for capture in m.captures {
                    result.push(capture.node);
                }
            }
            result
        } else {
            Vec::new()
        }
    }

    const DECL_QUERY: &str = r#"[
        (declaration) @decl
        (field_declaration) @decl
    ]"#;

    fn collect_decl_nodes<'a>(
        root: Node<'a>,
        query_cache: Option<&TsQueryCache>,
        source: &[u8],
        changed_ranges: Option<&[tree_sitter::Range]>,
    ) -> Vec<Node<'a>> {
        ts_traversal::query_or_traverse_in_ranges_collect(
            root,
            Self::DECL_QUERY,
            query_cache,
            &[ts_cpp_symbols::sym_declaration, ts_cpp_symbols::sym_field_declaration],
            source,
            changed_ranges,
        )
    }

    fn gather_initializer_literals<'a>(decl_node: Node<'a>) -> Vec<Node<'a>> {
        let mut literals = Vec::new();
        for i in 0..decl_node.named_child_count() {
            let Some(child) = decl_node.named_child(i as u32) else {
                continue;
            };
            if child.kind_id() == ts_cpp_symbols::sym_init_declarator {
                if let Some(value) = child.child_by_field_id(ts_cpp_symbols::field_value) {
                    literals.extend(Self::collect_number_literals(value));
                }
            }
        }
        if decl_node.kind_id() == ts_cpp_symbols::sym_field_declaration {
            if let Some(default) = decl_node.child_by_field_id(ts_cpp_symbols::field_default_value) {
                literals.extend(Self::collect_number_literals(default));
            }
        }
        literals
    }
}

impl Policy for NumericLiteralSuffixPolicy {
    fn name(&self) -> &str {
        "numeric_literal_suffix"
    }
    fn apply(&self, context: &PolicyContext<'_>) -> PolicyResult {
        let Some(tree) = context.tree_sitter_tree else {
            return PolicyResult::unchanged_with_warning(
                "numeric_literal_suffix: tree-sitter context unavailable".to_string(),
            );
        };

        let text = context.text;
        let source = text.as_bytes();
        let semantic = context.semantic_file_context;
        let mut replacements: Vec<(usize, usize, usize, usize, String)> = Vec::new();
        let mut violations = Vec::new();
        let warnings = Vec::new();

        let root = tree.root_node();
        let decl_nodes = Self::collect_decl_nodes(root, context.query_cache, context.text.as_bytes(), context.changed_ranges);

        for node in &decl_nodes {
            if node.has_error() || node.is_error() {
                continue;
            }
            let Some((suffix, is_float)) = Self::resolve_suffix(node, source, semantic) else {
                continue;
            };
            let type_text = Self::extract_type_text(node, source).unwrap_or_default();
            for literal_node in Self::gather_initializer_literals(*node) {
                let start = literal_node.start_byte();
                let end = literal_node.end_byte();
                if start >= end || end > source.len() {
                    continue;
                }
                let Ok(literal_text) = std::str::from_utf8(&source[start..end]) else {
                    continue;
                };
                if !Self::is_plausible_number_literal(literal_text) {
                    continue;
                }
                if let Some(new_text) = Self::rewrite_literal(literal_text, suffix, is_float) {
                    let line = literal_node.start_position().row + 1;
                    let column = literal_node.start_position().column + 1;
                    violations.push(Violation {
                        policy: self.name().into(),
                        message: format!(
                            "literal '{literal_text}' should be '{new_text}' for type '{type_text}'"
                        ),
                        line,
                        column: Some(column),
                    });
                    replacements.push((start, end, line, column, new_text));
                }
            }
        }

        if replacements.is_empty() {
            return PolicyResult::unchanged();
        }

        replacements.sort_by_key(|(start, ..)| *start);
        replacements.dedup_by_key(|(start, ..)| *start);

        let mut output = text.to_string();
        let mut edits = Vec::new();
        for (start, end, line, _column, new_text) in replacements.iter().rev() {
            let old_text = text[*start..*end].to_string();
            output.replace_range(*start..*end, new_text);
            edits.push(Edit {
                policy: self.name().into(),
                line: *line,
                before: old_text,
                after: new_text.clone(),
            });
        }
        edits.reverse();

        PolicyResult {
            text: output,
            changed: true,
            violations,
            edits,
            warnings,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tree_sitter::Parser;

    use super::*;
    use crate::model::policy_context::PolicyContext;
    use crate::parser::clang_result::ClangParseResult;
    use crate::parser::file_context::SemanticDeclaration;

    fn parse_cpp(text: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("cpp language");
        parser.parse(text, None).expect("parse tree")
    }

    fn make_clang_var(
        name: &str,
        line: usize,
        column: usize,
        canonical_kind: i32,
    ) -> SemanticDeclaration {
        SemanticDeclaration {
            name: name.to_string(),
            kind: clang_sys::CXCursor_VarDecl,
            line,
            column,
            canonical_type_kind: canonical_kind,
            ..Default::default()
        }
    }

    fn make_parse_result(symbols: Vec<SemanticDeclaration>) -> ClangParseResult {
        ClangParseResult::new(
            true,
            Vec::new(),
            symbols,
            [0; 5],
            Vec::new(),
        )
    }

    fn semantic_from(
        text: &str,
        path: &std::path::Path,
        tree: &tree_sitter::Tree,
        clang: &ClangParseResult,
    ) -> crate::parser::file_context::SemanticFileContext {
        crate::parser::file_context::SemanticFileContext::from_parses(
            text,
            path,
            Some(tree),
            Some(clang),
        )
    }

    #[test]
    fn float_gets_f() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "float x = 1.0;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "float x = 1.f;\n");
        assert_eq!(result.edits.len(), 1);
    }

    #[test]
    fn float_strips_zeros() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "float x = 1.50;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "float x = 1.5f;\n");
    }

    #[test]
    fn float_already_correct() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "float x = 1.f;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert!(!result.changed);
        assert!(result.edits.is_empty());
    }

    #[test]
    fn double_strips_suffix() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "double x = 1.0f;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "double x = 1.0;\n");
    }

    #[test]
    fn double_already_correct() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "double x = 1.0;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert!(!result.changed);
        assert!(result.edits.is_empty());
    }

    #[test]
    fn long_double_suffix() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "long double x = 1.0;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "long double x = 1.L;\n");
    }

    #[test]
    fn unsigned_gets_u() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "unsigned int x = 42;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "unsigned int x = 42U;\n");
    }

    #[test]
    fn int_strips_unsigned() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "int x = 1u;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "int x = 1;\n");
    }

    #[test]
    fn ull_gets_suffix() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "unsigned long long x = 1;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "unsigned long long x = 1ULL;\n");
    }

    #[test]
    fn hex_gets_suffix() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "unsigned int x = 0xFF;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "unsigned int x = 0xFFU;\n");
    }

    #[test]
    fn integer_float_skipped() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "float x = 0;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert!(!result.changed);
        assert!(result.edits.is_empty());
    }

    #[test]
    fn multiple_declarators_fixed() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "float x = 1.0, y = 2.0;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "float x = 1.f, y = 2.f;\n");
        assert_eq!(result.edits.len(), 2);
    }

    #[test]
    fn expression_literals_fixed() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "float x = 1.0 + 2.0;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "float x = 1.f + 2.f;\n");
    }

    #[test]
    fn auto_type_skipped() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "auto x = 1.0;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert!(!result.changed);
        assert!(result.edits.is_empty());
    }

    #[test]
    fn scientific_gets_suffix() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "float x = 1.0e10;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "float x = 1.e10f;\n");
    }

    #[test]
    fn uint8_gets_u_via_clang() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "uint8_t x = 1;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let clang = make_parse_result(vec![make_clang_var("x", 1, 9, clang_sys::CXType_UChar)]);
        let semantic = semantic_from(text, &path, &tree, &clang);
        let ctx = PolicyContext::new(text, &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "uint8_t x = 1U;\n");
        assert_eq!(result.edits.len(), 1);
    }

    #[test]
    fn uint64_gets_ull_via_clang() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "uint64_t x = 1;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let clang = make_parse_result(vec![make_clang_var("x", 1, 10, clang_sys::CXType_ULongLong)]);
        let semantic = semantic_from(text, &path, &tree, &clang);
        let ctx = PolicyContext::new(text, &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "uint64_t x = 1ULL;\n");
    }

    #[test]
    fn int32_strips_suffix_via_clang() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "int32_t x = 1u;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let clang = make_parse_result(vec![make_clang_var("x", 1, 9, clang_sys::CXType_Int)]);
        let semantic = semantic_from(text, &path, &tree, &clang);
        let ctx = PolicyContext::new(text, &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "int32_t x = 1;\n");
    }

    #[test]
    fn int64_gets_ll_via_clang() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "int64_t x = 1;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let clang = make_parse_result(vec![make_clang_var("x", 1, 9, clang_sys::CXType_LongLong)]);
        let semantic = semantic_from(text, &path, &tree, &clang);
        let ctx = PolicyContext::new(text, &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "int64_t x = 1LL;\n");
    }

    #[test]
    fn sizet_gets_platform_suffix_via_clang() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "size_t x = 10;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        // LP64: size_t is unsigned long
        let clang = make_parse_result(vec![make_clang_var("x", 1, 8, clang_sys::CXType_ULong)]);
        let semantic = semantic_from(text, &path, &tree, &clang);
        let ctx = PolicyContext::new(text, &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "size_t x = 10UL;\n");
    }

    #[test]
    fn sizet_lp64_ul_unchanged() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "static constexpr size_t x = 2UL;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let _clang = make_parse_result(vec![make_clang_var("x", 1, 25, clang_sys::CXType_ULong)]);
        let ctx = PolicyContext::new(text, &path)
            .with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert!(!result.changed);
    }

    #[test]
    fn constexpr_class_field_via_clang() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "class Foo {\n    static constexpr size_t C_MAX = 2;\n};\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.hpp");
        let clang = make_parse_result(vec![make_clang_var("C_MAX", 2, 29, clang_sys::CXType_ULong)]);
        let semantic = semantic_from(text, &path, &tree, &clang);
        let ctx = PolicyContext::new(text, &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic));
        let result = policy.apply(&ctx);
        assert_eq!(
            result.text,
            "class Foo {\n    static constexpr size_t C_MAX = 2UL;\n};\n"
        );
    }

    #[test]
    fn typedef_skipped_without_clang() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "size_t x = 10;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert!(!result.changed);
    }

    #[test]
    fn sizet_template_untouched() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "template<typename T, size_t N = 0UL>\nclass Foo {};\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.hpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert!(!result.changed);
    }

    #[test]
    fn unknown_typedef_skipped() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "MyCustomType x = 1;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert!(!result.changed);
        assert!(result.edits.is_empty());
    }

    #[test]
    fn error_node_skipped() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "int x = ;;\nfloat y = 1.0;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        // The first declaration has an error; only the valid float decl should be touched
        assert_eq!(result.text, "int x = ;;\nfloat y = 1.f;\n");
    }

    #[test]
    fn identifier_not_rewritten() {
        let policy = NumericLiteralSuffixPolicy::new();
        // Ensure identifiers like C_WORD_BITS are never corrupted
        let text = "static constexpr size_t C_WORD_BITS = static_cast<size_t>(64);\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.hpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        // size_t is a typedef — without clang, policy should skip it entirely
        assert!(!result.changed);
    }

    #[test]
    fn clang_name_mismatch_falls_back() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "float x = 1.0;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        // Provide a clang symbol with a different name — should fall back to tree-sitter
        let _clang = make_parse_result(vec![make_clang_var("y", 1, 7, clang_sys::CXType_ULong)]);
        let ctx = PolicyContext::new(text, &path)
            .with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        // Tree-sitter sees "float" type → suffix "f", not ULong's "UL"
        assert_eq!(result.text, "float x = 1.f;\n");
    }
}
