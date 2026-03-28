use rustc_hash::FxHashSet;

use crate::model::context_query::SemanticContextQuery;
use crate::parser::clang_types;
use crate::parser::file_context::{
    SemanticDeclaration, SemanticReference, SemanticScope, SourceLocation,
};
use crate::parser::semantic_region::SemanticRegion;
use crate::graph::snapshot::ProjectGraphSnapshot;
use crate::graph::types::ProjectSignal;
use crate::graph::symbol_id::SymbolId;

pub trait SignalKey {
    fn symbol_ids(&self, query: &SemanticContextQuery<'_>) -> Vec<SymbolId>;
}

impl SignalKey for &str {
    fn symbol_ids(&self, _query: &SemanticContextQuery<'_>) -> Vec<SymbolId> {
        ProjectContextQuery::symbol_ids_for_stable_id(self)
    }
}

impl SignalKey for &SemanticDeclaration {
    fn symbol_ids(&self, _query: &SemanticContextQuery<'_>) -> Vec<SymbolId> {
        ProjectContextQuery::symbol_ids_for_declaration(self)
    }
}

impl SignalKey for SourceLocation {
    fn symbol_ids(&self, query: &SemanticContextQuery<'_>) -> Vec<SymbolId> {
        match query.symbol_at(self.line, self.column, &[]) {
            Some(decl) => ProjectContextQuery::symbol_ids_for_declaration(decl),
            None => Vec::new(),
        }
    }
}

impl SignalKey for (usize, usize) {
    fn symbol_ids(&self, query: &SemanticContextQuery<'_>) -> Vec<SymbolId> {
        SourceLocation::new(self.0, self.1).symbol_ids(query)
    }
}

#[derive(Clone, Debug)]
pub struct ProjectContextQuery<'a> {
    semantic_query: SemanticContextQuery<'a>,
    project_graph_snapshot: Option<&'a ProjectGraphSnapshot>,
}

impl<'a> ProjectContextQuery<'a> {
    pub fn new(
        semantic_query: SemanticContextQuery<'a>,
        project_graph_snapshot: Option<&'a ProjectGraphSnapshot>,
    ) -> Self {
        Self {
            semantic_query,
            project_graph_snapshot,
        }
    }

    pub fn is_available(&self) -> bool {
        self.semantic_query.is_available()
    }

    pub fn symbol_at(
        &self,
        line: usize,
        column: usize,
        allowed_kinds: &[i32],
    ) -> Option<&'a SemanticDeclaration> {
        self.semantic_query.symbol_at(line, column, allowed_kinds)
    }

    pub fn decl_by_id(&self, stable_id: &str) -> Option<&'a SemanticDeclaration> {
        self.semantic_query.decl_by_id(stable_id)
    }

    pub fn references_of(&self, stable_id: &str) -> Vec<&'a SemanticReference> {
        self.semantic_query.references_of(stable_id)
    }

    pub fn scope_at(&self, line: usize, column: usize) -> Option<&'a SemanticScope> {
        self.semantic_query.scope_at(line, column)
    }

    pub fn region_at(&self, line: usize, column: usize) -> Option<&'a SemanticRegion> {
        self.semantic_query.region_at(line, column)
    }

    pub fn regions_for_line(&self, line: usize) -> Vec<&'a SemanticRegion> {
        self.semantic_query.regions_for_line(line)
    }

    pub fn is_macro_region(&self, line: usize, column: usize) -> bool {
        self.semantic_query.is_macro_region(line, column)
    }

    pub fn is_safe_edit(&self, line: usize, column: usize) -> bool {
        self.semantic_query.is_safe_edit(line, column)
    }

    pub fn context_cluster_key(&self, lines: &std::collections::BTreeSet<usize>) -> u64 {
        self.semantic_query.context_cluster_key(lines)
    }

    pub fn signal(&self, key: impl SignalKey) -> Option<ProjectSignal> {
        let snapshot = self.project_graph_snapshot?;
        let ids = key.symbol_ids(&self.semantic_query);
        Self::best_project_signal(snapshot, &ids)
    }

    fn best_project_signal(
        snapshot: &ProjectGraphSnapshot,
        symbol_ids: &[SymbolId],
    ) -> Option<ProjectSignal> {
        symbol_ids
            .iter()
            .filter_map(|symbol_id| snapshot.project_signal(symbol_id))
            .max_by(|left, right| {
                left.reference_count
                    .cmp(&right.reference_count)
                    .then(left.file_count.cmp(&right.file_count))
                    .then_with(|| left.consensus_score.total_cmp(&right.consensus_score))
                    .then((!left.from_tombstone).cmp(&!right.from_tombstone))
            })
    }

    pub(crate) fn symbol_ids_for_stable_id(stable_id: &str) -> Vec<SymbolId> {
        let trimmed = stable_id.trim();
        if let Some(usr) = trimmed.strip_prefix("usr:") {
            return vec![SymbolId::new(format!(
                "usr|{}",
                Self::sanitize_component(usr)
            ))];
        }
        Vec::new()
    }

    pub(crate) fn symbol_ids_for_declaration(declaration: &SemanticDeclaration) -> Vec<SymbolId> {
        let mut ids = Vec::<SymbolId>::new();
        if let Some(usr) = declaration
            .usr
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            ids.push(SymbolId::new(format!(
                "usr|{}",
                Self::sanitize_component(usr)
            )));
        }
        if let Some(scope_usr) = declaration
            .scope_usr
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            ids.push(SymbolId::new(format!(
                "scoped|{}|{}|{}",
                clang_types::cursor_kind_spelling(declaration.kind),
                Self::sanitize_component(scope_usr),
                Self::sanitize_component(declaration.name.as_str())
            )));
        }
        ids.push(SymbolId::new(format!(
            "bucket|{}|{}",
            clang_types::cursor_kind_spelling(declaration.kind),
            Self::sanitize_component(declaration.name.as_str())
        )));
        ids.extend(Self::symbol_ids_for_stable_id(
            declaration.stable_id.as_str(),
        ));
        let mut deduped = Vec::<SymbolId>::new();
        let mut seen: FxHashSet<String> = FxHashSet::default();
        for symbol_id in ids {
            let key = symbol_id.to_string();
            if seen.insert(key) {
                deduped.push(symbol_id);
            }
        }
        deduped
    }

    fn sanitize_component(raw: &str) -> String {
        raw.replace('|', "%7C")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::model::project_query::ProjectContextQuery;
    use crate::model::context_query::SemanticContextQuery;
    use crate::parser::file_context::{
        SemanticDeclaration, SemanticFileContext, SemanticIdProvenance,
    };
    use crate::graph::types::NodeMetrics;
    use crate::graph::snapshot::ProjectGraphSnapshot;
    use crate::graph::state::ProjectGraphState;
    use crate::graph::symbol_id::SymbolId;

    #[test]
    fn signal_resolves_usr() {
        let semantic = SemanticFileContext {
            declarations: vec![SemanticDeclaration {
                stable_id: "usr:c:@S@Demo@FI@m_value".to_string(),
                provenance: SemanticIdProvenance::Usr,
                name: "m_value".to_string(),
                kind: clang_sys::CXCursor_FieldDecl,
                line: 10,
                column: 5,
                usr: Some("c:@S@Demo@FI@m_value".to_string()),
                scope_usr: Some("c:@S@Demo".to_string()),
                canonical_type_kind: clang_sys::CXType_Unexposed,
                ..Default::default()
            }],
            ..SemanticFileContext::default()
        };
        let mut state = ProjectGraphState::new();
        state.set_metrics(
            SymbolId::new("usr|c:@S@Demo@FI@m_value"),
            NodeMetrics {
                reference_count: 42,
                file_count: 7,
                consensus_score: 0.91,
                last_updated_unix_ms: 1,
            },
        );
        let snapshot = ProjectGraphSnapshot::with_tombstone_decay(Arc::new(state), 0);
        let query = ProjectContextQuery::new(
            SemanticContextQuery::from_semantic(Some(&semantic)),
            Some(&snapshot),
        );
        let signal = query
            .signal((10, 5))
            .expect("project signal");
        assert_eq!(signal.reference_count, 42);
        assert_eq!(signal.file_count, 7);
    }
}
