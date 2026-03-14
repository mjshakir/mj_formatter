use crate::parser::clang_result::ClangParseResult;
use crate::parser::file_context::SemanticFileContext;
use crate::parser::ts_traversal;
use tree_sitter::Tree;

#[derive(Clone, Debug, Default)]
pub struct ConfidenceContext {
    pub tree_available: bool,
    pub tree_error_ratio: f64,
    pub clang_success: bool,
    pub semantic_usr_ratio: f64,
}

impl ConfidenceContext {
    pub fn from_parsers_and_semantic(
        tree: Option<&Tree>,
        clang: Option<&ClangParseResult>,
        semantic: Option<&SemanticFileContext>,
    ) -> Self {
        let tree_available = tree.is_some() || semantic.is_some();
        let tree_error_ratio = tree
            .map(|t| ts_traversal::tree_error_stats(t).error_ratio())
            .unwrap_or(0.0);

        let mut clang_success = clang.is_some_and(|parse| parse.success);
        let mut semantic_usr_ratio = 0.0f64;

        if let Some(semantic_context) = semantic {
            clang_success = clang_success || semantic_context.clang_success;
            if !semantic_context.declarations.is_empty() {
                let usr_backed = semantic_context
                    .declarations
                    .iter()
                    .filter(|declaration| {
                        declaration.usr.is_some()
                            || declaration.provenance
                                == crate::parser::file_context::SemanticIdProvenance::Usr
                    })
                    .count();
                semantic_usr_ratio = (usr_backed as f64
                    / semantic_context.declarations.len() as f64)
                    .clamp(0.0, 1.0);
            }
        }

        Self {
            tree_available,
            tree_error_ratio,
            clang_success,
            semantic_usr_ratio,
        }
    }

}

#[cfg(test)]
mod tests {
    use crate::parser::clang_types::ClangSymbolKind;
    use crate::parser::file_context::SemanticDeclaration;
    use crate::parser::file_context::SemanticFileContext;
    use crate::parser::file_context::SemanticIdProvenance;
    use tree_sitter::Parser;

    use super::ConfidenceContext;

    #[test]
    fn computes_tree_error_ratio_from_parse_tree() {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("cpp language");
        let malformed = "int main( { return 0; }\n";
        let tree = parser.parse(malformed, None).expect("parse tree");

        let context = ConfidenceContext::from_parsers_and_semantic(Some(&tree), None, None);
        assert!(context.tree_available);
        assert!(context.tree_error_ratio > 0.0);
    }

    #[test]
    fn semantic_context_enriches_clang_success_and_usr_ratio() {
        let semantic = SemanticFileContext {
            canonical_path: "src/demo.cpp".to_string(),
            clang_success: true,
            tree_has_error: false,
            diagnostic_summary: crate::parser::clang_result::ClangDiagnosticSummary::default(),
            diagnostic_entries: Vec::new(),
            declarations: vec![SemanticDeclaration {
                stable_id: "usr:demo".to_string(),
                provenance: SemanticIdProvenance::Usr,
                name: "DemoFn".to_string(),
                kind: ClangSymbolKind::Function,
                line: 6,
                column: 1,
                usr: Some("usr:demo".to_string()),
                scope_usr: None,
            }],
            references: Vec::new(),
            scopes: Vec::new(),
            regions: Vec::new(),
        };

        let context = ConfidenceContext::from_parsers_and_semantic(None, None, Some(&semantic));
        assert!(context.clang_success);
        assert!((context.semantic_usr_ratio - 1.0).abs() < 0.0001);
    }
}
