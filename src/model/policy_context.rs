use std::path::Path;

use tree_sitter::Tree;

use crate::engine::catalog::PolicyCertainty;
use crate::model::project_query::ProjectContextQuery;
use crate::model::context_query::SemanticContextQuery;
use crate::parser::clang_result::ClangDiagnosticSummary;
use crate::parser::clang_result::ClangParseResult;
use crate::parser::file_context::SemanticFileContext;
use crate::parser::query_cache::TsQueryCache;
use crate::graph::snapshot::ProjectGraphSnapshot;

#[derive(Clone, Copy, Debug)]
pub struct ParserTrust {
    pub semantic_rewrite: f64,
}

impl Default for ParserTrust {
    fn default() -> Self {
        Self {
            semantic_rewrite: 1.0,
        }
    }
}

impl ParserTrust {
    #[inline]
    pub fn scaled_edit_willingness(&self) -> f64 {
        1.0 / (1.0 + (-5.0_f64 * (self.semantic_rewrite - 0.5)).exp())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PolicyContext<'a> {
    pub text: &'a str,
    pub path: &'a Path,
    pub tree_sitter_tree: Option<&'a Tree>,
    pub clang_parse_result: Option<&'a ClangParseResult>,
    pub semantic_file_context: Option<&'a SemanticFileContext>,
    pub project_graph_snapshot: Option<&'a ProjectGraphSnapshot>,
    pub parser_trust: ParserTrust,
    pub policy_certainty: Option<PolicyCertainty>,
    pub query_cache: Option<&'a TsQueryCache>,
}

impl<'a> PolicyContext<'a> {
    pub fn new(text: &'a str, path: &'a Path) -> Self {
        Self {
            text,
            path,
            tree_sitter_tree: None,
            clang_parse_result: None,
            semantic_file_context: None,
            project_graph_snapshot: None,
            parser_trust: ParserTrust::default(),
            policy_certainty: None,
            query_cache: None,
        }
    }

    pub fn with_parser_trust(mut self, parser_trust: ParserTrust) -> Self {
        self.parser_trust = parser_trust;
        self
    }

    pub fn with_policy_certainty(mut self, policy_certainty: Option<PolicyCertainty>) -> Self {
        self.policy_certainty = policy_certainty;
        self
    }

    pub fn with_query_cache(mut self, query_cache: Option<&'a TsQueryCache>) -> Self {
        self.query_cache = query_cache;
        self
    }

    pub fn with_tree_sitter_tree(mut self, tree_sitter_tree: Option<&'a Tree>) -> Self {
        self.tree_sitter_tree = tree_sitter_tree;
        self
    }

    pub fn with_clang_parse_result(
        mut self,
        clang_parse_result: Option<&'a ClangParseResult>,
    ) -> Self {
        self.clang_parse_result = clang_parse_result;
        self
    }

    pub fn with_semantic_file_context(
        mut self,
        semantic_file_context: Option<&'a SemanticFileContext>,
    ) -> Self {
        self.semantic_file_context = semantic_file_context;
        self
    }

    pub fn with_project_graph_snapshot(
        mut self,
        project_graph_snapshot: Option<&'a ProjectGraphSnapshot>,
    ) -> Self {
        self.project_graph_snapshot = project_graph_snapshot;
        self
    }

    pub fn path_str(&self) -> &str {
        self.path.to_str().unwrap_or_default()
    }

    pub fn semantic_query(&self) -> SemanticContextQuery<'_> {
        SemanticContextQuery::from_semantic_file_context(self.semantic_file_context)
    }

    pub fn project_query(&self) -> ProjectContextQuery<'_> {
        ProjectContextQuery::new(self.semantic_query(), self.project_graph_snapshot)
    }

    pub fn clang_diagnostic_summary(&self) -> Option<ClangDiagnosticSummary> {
        self.clang_parse_result
            .map(ClangParseResult::diagnostic_summary)
    }

    pub fn clang_fatal_diagnostic_count(&self) -> usize {
        self.clang_diagnostic_summary()
            .map(|summary| summary.fatal)
            .unwrap_or(0)
    }

    pub fn has_fatal_clang_diagnostics(&self) -> bool {
        self.clang_fatal_diagnostic_count() > 0
    }
}
