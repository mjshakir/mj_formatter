use rustc_hash::FxHashMap;

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
    pub rename_coverage_signal: Option<f64>,
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
        let mut text_scan_agreement = 0.5f64;

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
                let excluded_ranges = crate::parser::text_scan::non_code_ranges(ts);
                let mut ref_counts: FxHashMap<&str, usize> = FxHashMap::default();
                for reference in &semantic_context.references {
                    *ref_counts.entry(reference.stable_id.as_str()).or_insert(0) += 1;
                }
                let mut total_agreement = 0.0f64;
                let mut checked = 0usize;
                for decl in &semantic_context.declarations {
                    let semantic_count = ref_counts.get(decl.stable_id.as_str()).copied().unwrap_or(0);
                    if semantic_count == 0 || decl.name.len() < 2 {
                        continue;
                    }
                    checked += 1;
                    let text_count = crate::parser::text_scan::count_id_excluded(
                        text, &decl.name, &excluded_ranges,
                    );
                    let agreement = if text_count <= semantic_count + 1 {
                        1.0
                    } else {
                        ((semantic_count + 1) as f64 / text_count as f64).clamp(0.1, 1.0)
                    };
                    total_agreement += agreement;
                }
                if checked > 0 {
                    text_scan_agreement = (total_agreement / checked as f64).clamp(0.0, 1.0);
                }
            }
        }

        Self {
            tree_available,
            tree_error_ratio,
            clang_success,
            semantic_usr_ratio,
            text_scan_agreement,
            rename_coverage_signal: None,
        }
    }

}

#[cfg(test)]
mod tests {
    use crate::parser::file_context::SemanticDeclaration;
    use crate::parser::file_context::SemanticFileContext;
    use crate::parser::file_context::SemanticIdProvenance;
    use crate::parser::file_context::SemanticReference;
    use tree_sitter::Parser;

    use super::ConfidenceContext;

    #[test]
    fn computes_error_ratio() {
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
    fn context_enriches_clang() {
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
                kind: clang_sys::CXCursor_FunctionDecl,
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
    fn agreement_perfect_match() {
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
                kind: clang_sys::CXCursor_VarDecl,
                line: 1,
                column: 5,
                usr: Some("usr:myVar".to_string()),
                scope_usr: None,
            }],
            references: vec![SemanticReference {
                stable_id: "usr:myVar".to_string(),
                provenance: SemanticIdProvenance::Usr,
                decl_path: "test.cpp".to_string(),
                decl_kind: clang_sys::CXCursor_VarDecl,
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
    fn agreement_low_extra() {
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
                kind: clang_sys::CXCursor_VarDecl,
                line: 1,
                column: 5,
                usr: Some("usr:val".to_string()),
                scope_usr: None,
            }],
            references: vec![SemanticReference {
                stable_id: "usr:val".to_string(),
                provenance: SemanticIdProvenance::Usr,
                decl_path: "test.cpp".to_string(),
                decl_kind: clang_sys::CXCursor_VarDecl,
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
        // With continuous agreement: semantic=1, text=5 → (1+1)/5 = 0.4
        assert!(
            context.text_scan_agreement > 0.3 && context.text_scan_agreement < 0.5,
            "expected continuous partial credit ~0.4, got {}",
            context.text_scan_agreement
        );
    }

    #[test]
    fn agreement_partial_credit() {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("cpp language");
        // semantic=2 refs, text will find 4 occurrences → (2+1)/4 = 0.75
        let code = "int foo = 0;\nint a = foo;\nint b = foo;\nint c = foo;\n";
        let tree = parser.parse(code, None).expect("parse tree");

        let semantic = SemanticFileContext {
            canonical_path: "test.cpp".to_string(),
            clang_success: true,
            tree_has_error: false,
            diagnostic_summary: crate::parser::clang_result::ClangDiagnosticSummary::default(),
            diagnostic_entries: Vec::new(),
            declarations: vec![SemanticDeclaration {
                stable_id: "usr:foo".to_string(),
                provenance: SemanticIdProvenance::Usr,
                name: "foo".to_string(),
                kind: clang_sys::CXCursor_VarDecl,
                line: 1,
                column: 5,
                usr: Some("usr:foo".to_string()),
                scope_usr: None,
            }],
            references: vec![
                SemanticReference {
                    stable_id: "usr:foo".to_string(),
                    provenance: SemanticIdProvenance::Usr,
                    decl_path: "test.cpp".to_string(),
                    decl_kind: clang_sys::CXCursor_VarDecl,
                    offset: 17,
                    line: 2,
                    column: 9,
                },
                SemanticReference {
                    stable_id: "usr:foo".to_string(),
                    provenance: SemanticIdProvenance::Usr,
                    decl_path: "test.cpp".to_string(),
                    decl_kind: clang_sys::CXCursor_VarDecl,
                    offset: 29,
                    line: 3,
                    column: 9,
                },
            ],
            scopes: Vec::new(),
            regions: Vec::new(),
        };

        let context = ConfidenceContext::from_parsers_and_semantic(
            Some(&tree), None, Some(&semantic), code, Some(&tree),
        );
        // semantic=2, text=4 → (2+1)/4 = 0.75
        assert!(
            context.text_scan_agreement > 0.6 && context.text_scan_agreement < 0.85,
            "expected partial credit ~0.75, got {}",
            context.text_scan_agreement
        );
    }

    #[test]
    fn agreement_neutral_norefs() {
        let semantic = SemanticFileContext {
            canonical_path: "test.cpp".to_string(),
            clang_success: true,
            tree_has_error: false,
            diagnostic_summary: crate::parser::clang_result::ClangDiagnosticSummary::default(),
            diagnostic_entries: Vec::new(),
            declarations: vec![SemanticDeclaration {
                stable_id: "usr:demo".to_string(),
                provenance: SemanticIdProvenance::Usr,
                name: "DemoFn".to_string(),
                kind: clang_sys::CXCursor_FunctionDecl,
                line: 1,
                column: 1,
                usr: Some("usr:demo".to_string()),
                scope_usr: None,
            }],
            references: Vec::new(), // No references → checked will be 0
            scopes: Vec::new(),
            regions: Vec::new(),
        };

        let context = ConfidenceContext::from_parsers_and_semantic(
            None, None, Some(&semantic), "void DemoFn() {}", None,
        );
        assert!(
            (context.text_scan_agreement - 0.5).abs() < 0.01,
            "expected neutral 0.5 when no refs checked, got {}",
            context.text_scan_agreement
        );
    }
}
