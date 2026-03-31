use std::path::Path;

use tree_sitter::Tree;

use crate::model::project_query::ProjectContextQuery;
use crate::model::context_query::SemanticContextQuery;
use crate::parser::clang_result::DiagnosticCounts;
use crate::parser::file_context::SemanticFileContext;
use crate::parser::query_cache::TsQueryCache;
use crate::graph::snapshot::ProjectGraphSnapshot;
use crate::policy::shared_data::PolicySharedData;

#[derive(Clone, Copy, Debug)]
pub struct PolicyContext<'a> {
    pub text: &'a str,
    pub path: &'a Path,
    pub tree_sitter_tree: Option<&'a Tree>,
    pub semantic_file_context: Option<&'a SemanticFileContext>,
    pub project_graph_snapshot: Option<&'a ProjectGraphSnapshot>,
    pub query_cache: Option<&'a TsQueryCache>,
    pub forced_batch_size: Option<usize>,
    pub shared: Option<&'a PolicySharedData<'a>>,
    pub changed_ranges: Option<&'a [tree_sitter::Range]>,
}

impl<'a> PolicyContext<'a> {
    pub fn new(text: &'a str, path: &'a Path) -> Self {
        Self {
            text,
            path,
            tree_sitter_tree: None,
            semantic_file_context: None,
            project_graph_snapshot: None,
            query_cache: None,
            forced_batch_size: None,
            shared: None,
            changed_ranges: None,
        }
    }

    pub fn with_query_cache(mut self, query_cache: Option<&'a TsQueryCache>) -> Self {
        self.query_cache = query_cache;
        self
    }

    pub fn with_tree(mut self, tree_sitter_tree: Option<&'a Tree>) -> Self {
        self.tree_sitter_tree = tree_sitter_tree;
        self
    }

    pub fn with_semantic(
        mut self,
        semantic_file_context: Option<&'a SemanticFileContext>,
    ) -> Self {
        self.semantic_file_context = semantic_file_context;
        self
    }

    pub fn with_graph(
        mut self,
        project_graph_snapshot: Option<&'a ProjectGraphSnapshot>,
    ) -> Self {
        self.project_graph_snapshot = project_graph_snapshot;
        self
    }

    pub fn with_shared(mut self, shared: Option<&'a PolicySharedData<'a>>) -> Self {
        self.shared = shared;
        self
    }

    pub fn with_changed_ranges(mut self, ranges: Option<&'a [tree_sitter::Range]>) -> Self {
        self.changed_ranges = ranges;
        self
    }

    pub fn path_str(&self) -> &str {
        self.path.to_str().unwrap_or_default()
    }

    pub fn semantic_query(&self) -> SemanticContextQuery<'_> {
        SemanticContextQuery::from_semantic(self.semantic_file_context)
    }

    pub fn project_query(&self) -> ProjectContextQuery<'_> {
        ProjectContextQuery::new(self.semantic_query(), self.project_graph_snapshot)
    }

    pub fn clang_diagnostic_counts(&self) -> Option<DiagnosticCounts> {
        self.semantic_file_context
            .map(|ctx| ctx.diagnostic_counts)
    }

    pub fn fatal_diag_count(&self) -> usize {
        self.clang_diagnostic_counts()
            .map(|c| c[clang_sys::CXDiagnostic_Fatal as usize])
            .unwrap_or(0)
    }

    pub fn has_fatal_diags(&self) -> bool {
        self.fatal_diag_count() > 0
    }
}
