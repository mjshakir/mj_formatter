use tree_sitter::{Node, StreamingIterator};

use crate::model::edit::Edit;
use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::model::violation::Violation;
use crate::parser::node_kind;
use crate::parser::query_cache::TsQueryCache;
use crate::policy::Policy;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NumericType {
    Float,
    Double,
    LongDouble,
    Int,
    UnsignedInt,
    Long,
    UnsignedLong,
    LongLong,
    UnsignedLongLong,
}

pub struct NumericLiteralSuffixPolicy;

impl NumericLiteralSuffixPolicy {
    pub fn new() -> Self {
        Self
    }

    fn classify_type(type_text: &str) -> Option<NumericType> {
        let normalized = type_text.split_whitespace().collect::<Vec<_>>().join(" ");
        match normalized.as_str() {
            // Primitive floating-point
            "float" => Some(NumericType::Float),
            "double" => Some(NumericType::Double),
            "long double" => Some(NumericType::LongDouble),
            // Primitive signed integer
            "int" | "signed" | "signed int" => Some(NumericType::Int),
            "long" | "long int" | "signed long" | "signed long int" => Some(NumericType::Long),
            "long long" | "long long int" | "signed long long" | "signed long long int" => {
                Some(NumericType::LongLong)
            }
            // Primitive unsigned integer
            "unsigned" | "unsigned int" => Some(NumericType::UnsignedInt),
            "unsigned long" | "unsigned long int" => Some(NumericType::UnsignedLong),
            "unsigned long long" | "unsigned long long int" => Some(NumericType::UnsignedLongLong),
            // Fixed-width signed integer typedefs (<cstdint> / <stdint.h>)
            "int8_t" | "int16_t" | "int32_t" | "int_fast8_t" | "int_fast16_t"
            | "int_fast32_t" | "int_least8_t" | "int_least16_t" | "int_least32_t" => {
                Some(NumericType::Int)
            }
            "int64_t" | "intmax_t" | "intptr_t" | "ptrdiff_t" | "ssize_t" | "int_fast64_t"
            | "int_least64_t" => Some(NumericType::LongLong),
            // Fixed-width unsigned integer typedefs (<cstdint> / <stdint.h>)
            "uint8_t" | "uint16_t" | "uint32_t" | "uint_fast8_t" | "uint_fast16_t"
            | "uint_fast32_t" | "uint_least8_t" | "uint_least16_t" | "uint_least32_t" => {
                Some(NumericType::UnsignedInt)
            }
            "uint64_t" | "uintmax_t" | "uintptr_t" | "size_t" | "uint_fast64_t"
            | "uint_least64_t" => Some(NumericType::UnsignedLongLong),
            _ => None,
        }
    }

    fn target_suffix(t: NumericType) -> &'static str {
        match t {
            NumericType::Float => "f",
            NumericType::Double => "",
            NumericType::LongDouble => "L",
            NumericType::Int => "",
            NumericType::UnsignedInt => "U",
            NumericType::Long => "L",
            NumericType::UnsignedLong => "UL",
            NumericType::LongLong => "LL",
            NumericType::UnsignedLongLong => "ULL",
        }
    }

    fn is_float_type(t: NumericType) -> bool {
        matches!(
            t,
            NumericType::Float | NumericType::Double | NumericType::LongDouble
        )
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
            // Only u/U/l/L are valid suffixes; f/F are hex digits
            let n = bytes
                .iter()
                .rev()
                .take_while(|&&b| matches!(b, b'u' | b'U' | b'l' | b'L'))
                .count();
            &literal[..literal.len() - n]
        } else if Self::is_float_literal(literal) {
            // Float suffix is a single trailing f/F/l/L
            let last = bytes[bytes.len() - 1];
            if matches!(last, b'f' | b'F' | b'l' | b'L') {
                &literal[..literal.len() - 1]
            } else {
                literal
            }
        } else {
            // Integer: strip trailing combination of u/U/l/L
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

    fn rewrite_literal(literal: &str, target: NumericType) -> Option<String> {
        let literal_is_float = Self::is_float_literal(literal);
        let target_is_float = Self::is_float_type(target);
        if literal_is_float != target_is_float {
            return None;
        }
        let number_part = Self::strip_suffix(literal);
        let suffix = Self::target_suffix(target);
        // Only normalize (strip trailing zeros) when adding a non-empty suffix to a float
        let normalized_number = if target_is_float && !suffix.is_empty() {
            Self::normalize_float_decimal(number_part)
        } else {
            number_part.to_string()
        };
        let new_literal = format!("{normalized_number}{suffix}");
        if new_literal == literal {
            None
        } else {
            Some(new_literal)
        }
    }

    fn extract_type_text(node: &Node<'_>, source: &[u8]) -> Option<String> {
        const TYPE_NODE_KINDS: &[&str] =
            &[node_kind::PRIMITIVE_TYPE, node_kind::SIZED_TYPE_SPECIFIER, node_kind::TYPE_IDENTIFIER];
        // Try named field first
        if let Some(type_node) = node.child_by_field_name("type") {
            if TYPE_NODE_KINDS.contains(&type_node.kind()) {
                if let Ok(text) = type_node.utf8_text(source) {
                    return Some(text.split_whitespace().collect::<Vec<_>>().join(" "));
                }
            }
        }
        // Fallback: scan direct children for a type node
        for i in 0..node.child_count() {
            let Some(child) = node.child(i as u32) else {
                continue;
            };
            if TYPE_NODE_KINDS.contains(&child.kind()) {
                if let Ok(text) = child.utf8_text(source) {
                    return Some(text.split_whitespace().collect::<Vec<_>>().join(" "));
                }
            }
        }
        None
    }

    fn collect_number_literals(node: Node<'_>) -> Vec<Node<'_>> {
        let mut result = Vec::new();
        let mut stack = vec![node];
        while let Some(current) = stack.pop() {
            if current.kind() == node_kind::NUMBER_LITERAL {
                result.push(current);
                continue;
            }
            for i in (0..current.child_count()).rev() {
                if let Some(child) = current.child(i as u32) {
                    stack.push(child);
                }
            }
        }
        result
    }

    const DECL_QUERY: &str = r#"[
        (declaration) @decl
        (field_declaration) @decl
    ]"#;

    fn collect_decl_nodes<'a>(
        root: Node<'a>,
        query_cache: Option<&TsQueryCache>,
    ) -> Vec<Node<'a>> {
        let mut nodes = Vec::new();

        if let Some(query) = query_cache
            .and_then(|qc| qc.get_or_compile(Self::DECL_QUERY).ok())
        {
            let mut cursor = tree_sitter::QueryCursor::new();
            let mut matches = cursor.matches(&query, root, "".as_bytes());
            while let Some(m) = {
                matches.advance();
                matches.get()
            } {
                for capture in m.captures {
                    nodes.push(capture.node);
                }
            }
        } else {
            let mut stack = vec![root];
            while let Some(node) = stack.pop() {
                if matches!(
                    node.kind(),
                    node_kind::DECLARATION | node_kind::FIELD_DECLARATION
                ) {
                    nodes.push(node);
                }
                for i in (0..node.child_count()).rev() {
                    if let Some(child) = node.child(i as u32) {
                        stack.push(child);
                    }
                }
            }
        }
        nodes
    }

    fn gather_initializer_literals<'a>(decl_node: Node<'a>) -> Vec<Node<'a>> {
        let mut literals = Vec::new();
        for i in 0..decl_node.child_count() {
            let Some(child) = decl_node.child(i as u32) else {
                continue;
            };
            if child.kind() == node_kind::INIT_DECLARATOR {
                if let Some(value) = child.child_by_field_name("value") {
                    literals.extend(Self::collect_number_literals(value));
                }
            }
        }
        // field_declaration may carry a direct default_value field
        if decl_node.kind() == node_kind::FIELD_DECLARATION {
            if let Some(default) = decl_node.child_by_field_name("default_value") {
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
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits: Vec::new(),
                warnings: vec![
                    "numeric_literal_suffix: tree-sitter context unavailable".to_string(),
                ],
            };
        };

        let text = context.text;
        let source = text.as_bytes();
        // (start_byte, end_byte, line, column, new_text)
        let mut replacements: Vec<(usize, usize, usize, usize, String)> = Vec::new();
        let mut violations = Vec::new();
        let warnings = Vec::new();

        let root = tree.root_node();
        let decl_nodes = Self::collect_decl_nodes(root, context.query_cache);

        for node in &decl_nodes {
            if let Some(type_text) = Self::extract_type_text(node, source) {
                if let Some(numeric_type) = Self::classify_type(&type_text) {
                    for literal_node in Self::gather_initializer_literals(*node) {
                        let start = literal_node.start_byte();
                        let end = literal_node.end_byte();
                        if start >= end || end > source.len() {
                            continue;
                        }
                        let Ok(literal_text) = std::str::from_utf8(&source[start..end]) else {
                            continue;
                        };
                        if let Some(new_text) =
                            Self::rewrite_literal(literal_text, numeric_type)
                        {
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
            }
        }

        if replacements.is_empty() {
            return PolicyResult {
                text: text.to_string(),
                violations,
                edits: Vec::new(),
                warnings,
            };
        }

        // Sort by start byte and remove any duplicates (same start offset)
        replacements.sort_by_key(|(start, ..)| *start);
        replacements.dedup_by_key(|(start, ..)| *start);

        // Apply in reverse byte order so earlier offsets stay valid
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

    fn parse_cpp(text: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("cpp language");
        parser.parse(text, None).expect("parse tree")
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
        assert_eq!(result.text, text);
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
        assert_eq!(result.text, text);
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
        assert_eq!(result.text, text);
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
        assert_eq!(result.text, text);
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
    fn uint8_gets_u() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "uint8_t x = 1;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "uint8_t x = 1U;\n");
        assert_eq!(result.edits.len(), 1);
    }

    #[test]
    fn uint64_gets_ull() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "uint64_t x = 1;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "uint64_t x = 1ULL;\n");
    }

    #[test]
    fn int32_strips_suffix() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "int32_t x = 1u;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "int32_t x = 1;\n");
    }

    #[test]
    fn int64_gets_ll() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "int64_t x = 1;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "int64_t x = 1LL;\n");
    }

    #[test]
    fn sizet_gets_ull() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "size_t x = 10;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "size_t x = 10ULL;\n");
    }

    #[test]
    fn constexpr_sizet_ull() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "static constexpr size_t x = 2;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "static constexpr size_t x = 2ULL;\n");
    }

    #[test]
    fn constexpr_class_ull() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "class Foo {\n    static constexpr size_t C_MAX = 2;\n};\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.hpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert_eq!(
            result.text,
            "class Foo {\n    static constexpr size_t C_MAX = 2ULL;\n};\n"
        );
    }

    #[test]
    fn sizet_ull_unchanged() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "static constexpr size_t x = 2ULL;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, text);
        assert!(result.edits.is_empty());
    }

    #[test]
    fn sizet_ul_corrected() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "static constexpr size_t x = 6UL;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, "static constexpr size_t x = 6ULL;\n");
    }

    #[test]
    fn sizet_template_untouched() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "template<typename T, size_t N = 0UL>\nclass Foo {};\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.hpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        // Template parameters are not declarations, should be untouched
        assert_eq!(result.text, text);
    }

    #[test]
    fn unknown_typedef_skipped() {
        let policy = NumericLiteralSuffixPolicy::new();
        let text = "MyCustomType x = 1;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&ctx);
        assert_eq!(result.text, text);
        assert!(result.edits.is_empty());
    }
}
