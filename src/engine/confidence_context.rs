use std::collections::HashMap;

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
    pub text_scan_agreement: f64,
}

impl ConfidenceContext {
    pub fn from_parsers_and_semantic(
        tree: Option<&Tree>,
        clang: Option<&ClangParseResult>,
        semantic: Option<&SemanticFileContext>,
        text: &str,
        ts_tree: Option<&Tree>,
    ) -> Self {
        let tree_available = tree.is_some() || semantic.is_some();
        let tree_error_ratio = tree
            .map(|t| ts_traversal::tree_error_stats(t).error_ratio())
            .unwrap_or(0.0);

        let mut clang_success = clang.is_some_and(|parse| parse.success);
        let mut semantic_usr_ratio = 0.0f64;
        let mut text_scan_agreement = 1.0f64;

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

            if let Some(ts) = ts_tree {
                let excluded_ranges = crate::text_scan::collect_non_code_byte_ranges(ts);
                let mut ref_counts: HashMap<&str, usize> = HashMap::new();
                for reference in &semantic_context.references {
                    *ref_counts.entry(reference.stable_id.as_str()).or_insert(0) += 1;
                }
                let mut agreed = 0usize;
                let mut checked = 0usize;
                for decl in &semantic_context.declarations {
                    let semantic_count = ref_counts.get(decl.stable_id.as_str()).copied().unwrap_or(0);
                    if semantic_count == 0 || decl.name.len() < 2 {
                        continue;
                    }
                    checked += 1;
                    let text_count = crate::text_scan::count_identifier_occurrences_with_exclusions(
                        text, &decl.name, &excluded_ranges,
                    );
                    if text_count <= semantic_count + 1 {
                        agreed += 1;
                    }
                }
                if checked > 0 {
                    text_scan_agreement = (agreed as f64 / checked as f64).clamp(0.0, 1.0);
                }
            }
        }

        Self {
            tree_available,
            tree_error_ratio,
            clang_success,
            semantic_usr_ratio,
            text_scan_agreement,
        }
    }

}

#[cfg(test)]
mod tests {
    use crate::parser::clang_types::ClangSymbolKind;
    use crate::parser::file_context::SemanticDeclaration;
    use crate::parser::file_context::SemanticFileContext;
    use crate::parser::file_context::SemanticIdProvenance;
    use crate::parser::file_context::SemanticReference;
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

        let context = ConfidenceContext::from_parsers_and_semantic(Some(&tree), None, None, malformed, Some(&tree));
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

        let context = ConfidenceContext::from_parsers_and_semantic(None, None, Some(&semantic), "", None);
        assert!(context.clang_success);
        assert!((context.semantic_usr_ratio - 1.0).abs() < 0.0001);
    }

    #[test]
    fn text_scan_agreement_perfect_when_text_matches_semantic() {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("cpp language");
        let code = "int myVar = 0;\nint x = myVar + 1;\n";
        let tree = parser.parse(code, None).expect("parse tree");

        let semantic = SemanticFileContext {
            canonical_path: "test.cpp".to_string(),
            clang_success: true,
            tree_has_error: false,
            diagnostic_summary: crate::parser::clang_result::ClangDiagnosticSummary::default(),
            diagnostic_entries: Vec::new(),
            declarations: vec![SemanticDeclaration {
                stable_id: "usr:myVar".to_string(),
                provenance: SemanticIdProvenance::Usr,
                name: "myVar".to_string(),
                kind: ClangSymbolKind::Variable,
                line: 1,
                column: 5,
                usr: Some("usr:myVar".to_string()),
                scope_usr: None,
            }],
            references: vec![SemanticReference {
                stable_id: "usr:myVar".to_string(),
                provenance: SemanticIdProvenance::Usr,
                decl_path: "test.cpp".to_string(),
                decl_kind: ClangSymbolKind::Variable,
                offset: 19,
                line: 2,
                column: 9,
            }],
            scopes: Vec::new(),
            regions: Vec::new(),
        };

        let context = ConfidenceContext::from_parsers_and_semantic(
            Some(&tree), None, Some(&semantic), code, Some(&tree),
        );
        assert!(
            context.text_scan_agreement >= 0.99,
            "expected perfect agreement, got {}",
            context.text_scan_agreement
        );
    }

    #[test]
    fn text_scan_agreement_low_when_text_has_extra_occurrences() {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("cpp language");
        let code = "int val = 0;\nint a = val;\nint b = val;\nint c = val;\nint d = val;\n";
        let tree = parser.parse(code, None).expect("parse tree");

        let semantic = SemanticFileContext {
            canonical_path: "test.cpp".to_string(),
            clang_success: true,
            tree_has_error: false,
            diagnostic_summary: crate::parser::clang_result::ClangDiagnosticSummary::default(),
            diagnostic_entries: Vec::new(),
            declarations: vec![SemanticDeclaration {
                stable_id: "usr:val".to_string(),
                provenance: SemanticIdProvenance::Usr,
                name: "val".to_string(),
                kind: ClangSymbolKind::Variable,
                line: 1,
                column: 5,
                usr: Some("usr:val".to_string()),
                scope_usr: None,
            }],
            references: vec![SemanticReference {
                stable_id: "usr:val".to_string(),
                provenance: SemanticIdProvenance::Usr,
                decl_path: "test.cpp".to_string(),
                decl_kind: ClangSymbolKind::Variable,
                offset: 17,
                line: 2,
                column: 9,
            }],
            scopes: Vec::new(),
            regions: Vec::new(),
        };

        let context = ConfidenceContext::from_parsers_and_semantic(
            Some(&tree), None, Some(&semantic), code, Some(&tree),
        );
        assert!(
            context.text_scan_agreement < 1.0,
            "expected low agreement when text has 5 occurrences but semantic has 1 ref, got {}",
            context.text_scan_agreement
        );
    }
}
