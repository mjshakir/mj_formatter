use tree_sitter::Node;

use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::model::violation::Violation;
use crate::parser::node_kind;
use crate::parser::query_cache::TsQueryCache;
use crate::parser::ts_traversal;
use crate::policy::Policy;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnakeCaseApplyTarget {
    Variables,
    Functions,
    Both,
}

impl SnakeCaseApplyTarget {
    pub fn from_value(value: &str) -> Self {
        match value.trim().to_lowercase().as_str() {
            "variables" => Self::Variables,
            "functions" => Self::Functions,
            _ => Self::Both,
        }
    }

    fn include_functions(self) -> bool {
        matches!(self, Self::Functions | Self::Both)
    }

    fn include_variables(self) -> bool {
        matches!(self, Self::Variables | Self::Both)
    }
}

pub struct SnakeCasePolicy {
    apply_target: SnakeCaseApplyTarget,
    exclude_class_namespace: bool,
}

impl SnakeCasePolicy {
    pub fn new(
        apply_target: SnakeCaseApplyTarget,
        exclude_class_namespace: bool,
        prefer_clang: bool,
        use_tree_sitter: bool,
    ) -> Self {
        let _ = prefer_clang;
        let _ = use_tree_sitter;
        Self {
            apply_target,
            exclude_class_namespace,
        }
    }

    fn is_valid_identifier(name: &str) -> bool {
        let mut chars = name.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        (first.is_ascii_alphabetic() || first == '_')
            && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    }

    fn is_snake_case(name: &str) -> bool {
        if name.is_empty() {
            return true;
        }
        if name
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
            && name.contains('_')
        {
            return true;
        }
        let mut chars = name.chars();
        let Some(first) = chars.next() else {
            return true;
        };
        if !(first.is_ascii_lowercase() || first == '_') {
            return false;
        }
        chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
    }

    fn should_exclude_type_like(&self, name: &str) -> bool {
        self.exclude_class_namespace
            && name
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_uppercase())
    }

    const TARGET_QUERY: &str = r#"[
        (function_definition) @node
        (declaration) @node
        (field_declaration) @node
        (parameter_declaration) @node
    ]"#;

    fn collect_targets<'a>(
        root: Node<'a>,
        query_cache: Option<&TsQueryCache>,
    ) -> Vec<Node<'a>> {
        ts_traversal::query_or_traverse_collect(
            root,
            Self::TARGET_QUERY,
            query_cache,
            &[
                node_kind::FUNCTION_DEFINITION,
                node_kind::DECLARATION,
                node_kind::FIELD_DECLARATION,
                node_kind::PARAMETER_DECLARATION,
            ],
        )
    }

    fn declarator_identifier(decl_node: Node<'_>) -> Option<Node<'_>> {
        ts_traversal::declarator_identifier(decl_node)
    }
}

impl Policy for SnakeCasePolicy {
    fn name(&self) -> &str {
        "snake_case"
    }
    fn apply(&self, context: &PolicyContext<'_>) -> PolicyResult {
        let Some(tree) = context.tree_sitter_tree else {
            return PolicyResult::unchanged_with_warning("snake_case: tree-sitter context unavailable".to_string());
        };
        let semantic_query = context.semantic_query();
        if !semantic_query.is_available() {
            return PolicyResult::unchanged_with_warning("snake_case: semantic context unavailable".to_string());
        }

        let mut violations = Vec::new();
        let root = tree.root_node();

        let targets = Self::collect_targets(root, context.query_cache);

        for node in &targets {
            let node = *node;
            let kind = node.kind();

            if self.apply_target.include_functions() && kind == node_kind::FUNCTION_DEFINITION {
                if let Some(declarator) = ts_traversal::first_descendant(
                    node,
                    &[node_kind::FUNCTION_DECLARATOR],
                    &[node_kind::COMPOUND_STATEMENT],
                ) {
                    if let Some(name_node) = ts_traversal::rightmost_descendant(
                        declarator,
                        &[
                            node_kind::IDENTIFIER,
                            node_kind::FIELD_IDENTIFIER,
                            node_kind::TYPE_IDENTIFIER,
                            node_kind::DESTRUCTOR_NAME,
                        ],
                        &[node_kind::PARAMETER_LIST, node_kind::TEMPLATE_PARAMETER_LIST],
                    ) {
                        let name = name_node.utf8_text(context.text.as_bytes()).unwrap_or("");
                        let line = name_node.start_position().row + 1;
                        let column = name_node.start_position().column + 1;
                        let allowed = [
                            clang_sys::CXCursor_FunctionDecl,
                            clang_sys::CXCursor_CXXMethod,
                            clang_sys::CXCursor_Constructor,
                            clang_sys::CXCursor_Destructor,
                        ];
                        let Some(symbol) = semantic_query.symbol_at(line, column, &allowed) else {
                            continue;
                        };
                        if symbol.kind == clang_sys::CXCursor_Constructor
                            || symbol.kind == clang_sys::CXCursor_Destructor
                        {
                            continue;
                        }
                        let short_name = symbol.name.split("::").last().unwrap_or(name);
                        if !short_name.starts_with("operator")
                            && !short_name.starts_with('~')
                            && !short_name.contains('<')
                            && !self.should_exclude_type_like(short_name)
                            && !Self::is_snake_case(short_name)
                        {
                            violations.push(Violation {
                                policy: self.name().into(),
                                message: format!("Function '{short_name}' is not snake_case"),
                                line,
                                column: Some(name_node.start_position().column + 1),
                            });
                        }
                    }
                }
            }

            if self.apply_target.include_variables()
                && matches!(
                    kind,
                    node_kind::DECLARATION
                        | node_kind::FIELD_DECLARATION
                        | node_kind::PARAMETER_DECLARATION
                )
            {
                if let Some(name_node) = Self::declarator_identifier(node) {
                    let name = name_node.utf8_text(context.text.as_bytes()).unwrap_or("");
                    if name.is_empty() || !Self::is_valid_identifier(name) {
                        continue;
                    }
                    let line = name_node.start_position().row + 1;
                    let column = name_node.start_position().column + 1;
                    let allowed = [
                        clang_sys::CXCursor_VarDecl,
                        clang_sys::CXCursor_FieldDecl,
                        clang_sys::CXCursor_ParmDecl,
                    ];
                    if semantic_query.symbol_at(line, column, &allowed).is_none() {
                        continue;
                    }
                    if !self.should_exclude_type_like(name)
                        && !Self::is_snake_case(name)
                    {
                        violations.push(Violation {
                            policy: self.name().into(),
                            message: format!("Variable '{name}' is not snake_case"),
                            line,
                            column: Some(name_node.start_position().column + 1),
                        });
                    }
                }
            }
        }

        let mut warnings = Vec::new();
        if let Some(clang_parse) = context.clang_parse_result {
            if !clang_parse.success {
                warnings.push(
                    "snake_case: clang syntax diagnostics detected; semantic confidence reduced"
                        .to_string(),
                );
                for message in clang_parse.diagnostics.iter().take(3) {
                    warnings.push(format!("clang: {message}"));
                }
            }
        }

        PolicyResult {
            changed: false,
            violations,
            warnings,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use tree_sitter::Parser;

    use super::*;
    use crate::model::policy_context::PolicyContext;
    use crate::parser::clang_result::{ClangDiagnosticSummary, ClangParseResult};
    use crate::parser::clang_symbol::ClangSymbol;
    use crate::parser::manager::ParserManager;
    use crate::parser::file_context::SemanticFileContext;

    fn parse_cpp(text: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("set cpp language");
        parser.parse(text, None).expect("parse tree")
    }

    fn semantic_for(
        text: &str,
        path: &Path,
        tree: &tree_sitter::Tree,
    ) -> (
        std::sync::Arc<crate::parser::clang_result::ClangParseResult>,
        SemanticFileContext,
    ) {
        let parser_manager = ParserManager::with_clang("clang".to_string(), Vec::new());
        let clang = parser_manager
            .parse_clang(text, path)
            .expect("clang parse for test context");
        let semantic = SemanticFileContext::from_parses(text, path, Some(tree), Some(&clang));
        (clang, semantic)
    }

    #[test]
    fn reports_non_snake() {
        let policy = SnakeCasePolicy::new(SnakeCaseApplyTarget::Both, false, true, true);
        let text = "int CamelVar = 0;\nint BadName() { return CamelVar; }\n".to_string();
        let tree = parse_cpp(text.as_str());
        let path = PathBuf::from("sample.cpp");
        let (clang, semantic) = semantic_for(text.as_str(), &path, &tree);
        let ctx = PolicyContext::new(text.as_str(), &path)
            .with_tree(Some(&tree))
            .with_clang(Some(&*clang))
            .with_semantic(Some(&semantic));
        let result = policy.apply(&ctx);
        assert!(result.violations.len() >= 2);
    }

    #[test]
    fn ignores_uppercase_types() {
        let policy = SnakeCasePolicy::new(SnakeCaseApplyTarget::Variables, true, true, true);
        let text = "const MyType Value = {};\n".to_string();
        let tree = parse_cpp(text.as_str());
        let path = PathBuf::from("sample.cpp");
        let (clang, semantic) = semantic_for(text.as_str(), &path, &tree);
        let ctx = PolicyContext::new(text.as_str(), &path)
            .with_tree(Some(&tree))
            .with_clang(Some(&*clang))
            .with_semantic(Some(&semantic));
        let result = policy.apply(&ctx);
        assert!(result.violations.is_empty());
    }

    #[test]
    fn clang_filters_tree() {
        let policy = SnakeCasePolicy::new(SnakeCaseApplyTarget::Both, false, true, true);
        let text = "int CamelVar = 0;\nint BadName() { return CamelVar; }\n".to_string();
        let tree = parse_cpp(text.as_str());
        let clang_parse_result = ClangParseResult::new(
            true,
            Vec::new(),
            vec![ClangSymbol {
                name: "DifferentName".to_string(),
                kind: clang_sys::CXCursor_VarDecl,
                line: 1,
                column: 5,
                usr: None,
                scope_usr: None,
                storage_class: None,
                is_const: false,
                is_volatile: false,
                type_kind: clang_sys::CXType_Unexposed,
                type_display: String::new(),
            canonical_type_kind: clang_sys::CXType_Unexposed,
            template_name: None,
            }],
            ClangDiagnosticSummary::default(),
            Vec::new(),
        );
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text.as_str(), &path)
            .with_tree(Some(&tree))
            .with_clang(Some(&clang_parse_result));
        let result = policy.apply(&ctx);
        assert!(result.violations.is_empty());
    }
}
