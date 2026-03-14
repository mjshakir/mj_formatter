use std::collections::HashSet;

use crate::model::context_query::SemanticContextQuery;
use crate::parser::clang_types::ClangSymbolKind;
use crate::parser::file_context::{
    SemanticDeclaration, SemanticReference, SemanticScope, SourceLocation,
};
use crate::parser::semantic_region::SemanticRegion;
use crate::graph::snapshot::ProjectGraphSnapshot;
use crate::graph::types::ProjectSignal;
use crate::graph::symbol_id::SymbolId;

const ALL_SYMBOL_KINDS: [ClangSymbolKind; 21] = [
    ClangSymbolKind::Function,
    ClangSymbolKind::FunctionTemplate,
    ClangSymbolKind::Method,
    ClangSymbolKind::Constructor,
    ClangSymbolKind::Destructor,
    ClangSymbolKind::Variable,
    ClangSymbolKind::Field,
    ClangSymbolKind::Parameter,
    ClangSymbolKind::Type,
    ClangSymbolKind::Namespace,
    ClangSymbolKind::Macro,
    ClangSymbolKind::Struct,
    ClangSymbolKind::Class,
    ClangSymbolKind::Union,
    ClangSymbolKind::Enum,
    ClangSymbolKind::Typedef,
    ClangSymbolKind::TypeAlias,
    ClangSymbolKind::ConversionFunction,
    ClangSymbolKind::UsingDecl,
    ClangSymbolKind::EnumConstant,
    ClangSymbolKind::FriendDecl,
];

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
        allowed_kinds: &[ClangSymbolKind],
    ) -> Option<&'a SemanticDeclaration> {
        self.semantic_query.symbol_at(line, column, allowed_kinds)
    }

    pub fn declaration_by_stable_id(&self, stable_id: &str) -> Option<&'a SemanticDeclaration> {
        self.semantic_query.declaration_by_stable_id(stable_id)
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

    pub fn project_signal_for_stable_id(&self, stable_id: &str) -> Option<ProjectSignal> {
        let snapshot = self.project_graph_snapshot?;
        let symbol_ids = Self::symbol_ids_for_stable_id(stable_id);
        Self::best_project_signal(snapshot, symbol_ids.as_slice())
    }

    pub fn project_signal_for_declaration(
        &self,
        declaration: &SemanticDeclaration,
    ) -> Option<ProjectSignal> {
        let snapshot = self.project_graph_snapshot?;
        let symbol_ids = Self::symbol_ids_for_declaration(declaration);
        Self::best_project_signal(snapshot, symbol_ids.as_slice())
    }

    pub fn project_signal_for_location(&self, location: SourceLocation) -> Option<ProjectSignal> {
        let declaration =
            self.semantic_query
                .symbol_at(location.line, location.column, &ALL_SYMBOL_KINDS)?;
        self.project_signal_for_declaration(declaration)
    }

    pub fn project_signal_for_line(&self, line: usize, column: usize) -> Option<ProjectSignal> {
        self.project_signal_for_location(SourceLocation::new(line, column))
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

    fn symbol_ids_for_stable_id(stable_id: &str) -> Vec<SymbolId> {
        let trimmed = stable_id.trim();
        if let Some(usr) = trimmed.strip_prefix("usr:") {
            return vec![SymbolId::new(format!(
                "usr|{}",
                Self::sanitize_component(usr)
            ))];
        }
        Vec::new()
    }

    fn symbol_ids_for_declaration(declaration: &SemanticDeclaration) -> Vec<SymbolId> {
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
                Self::kind_label(declaration.kind),
                Self::sanitize_component(scope_usr),
                Self::sanitize_component(declaration.name.as_str())
            )));
        }
        ids.push(SymbolId::new(format!(
            "bucket|{}|{}",
            Self::kind_label(declaration.kind),
            Self::sanitize_component(declaration.name.as_str())
        )));
        ids.extend(Self::symbol_ids_for_stable_id(
            declaration.stable_id.as_str(),
        ));
        let mut deduped = Vec::<SymbolId>::new();
        let mut seen = HashSet::<String>::new();
        for symbol_id in ids {
            let key = symbol_id.to_string();
            if seen.insert(key) {
                deduped.push(symbol_id);
            }
        }
        deduped
    }

    fn kind_label(kind: ClangSymbolKind) -> &'static str {
        match kind {
            ClangSymbolKind::Function | ClangSymbolKind::FunctionTemplate => "function",
            ClangSymbolKind::Method => "method",
            ClangSymbolKind::Constructor => "constructor",
            ClangSymbolKind::Destructor => "destructor",
            ClangSymbolKind::Variable => "variable",
            ClangSymbolKind::Field => "field",
            ClangSymbolKind::Parameter => "parameter",
            ClangSymbolKind::Type => "type",
            ClangSymbolKind::Namespace => "namespace",
            ClangSymbolKind::Macro => "macro",
            ClangSymbolKind::Struct => "struct",
            ClangSymbolKind::Class => "class",
            ClangSymbolKind::Union => "union",
            ClangSymbolKind::Enum => "enum",
            ClangSymbolKind::Typedef => "typedef",
            ClangSymbolKind::TypeAlias => "type_alias",
            ClangSymbolKind::ConversionFunction => "conversion_function",
            ClangSymbolKind::UsingDecl => "using_decl",
            ClangSymbolKind::EnumConstant => "enum_constant",
            ClangSymbolKind::FriendDecl => "friend_decl",
            ClangSymbolKind::Other => "other",
        }
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
    use crate::parser::clang_types::ClangSymbolKind;
    use crate::parser::file_context::{
        SemanticDeclaration, SemanticFileContext, SemanticIdProvenance,
    };
    use crate::graph::types::NodeMetrics;
    use crate::graph::snapshot::ProjectGraphSnapshot;
    use crate::graph::state::ProjectGraphState;
    use crate::graph::symbol_id::SymbolId;

    #[test]
    fn project_signal_resolves_usr_backed_declaration() {
        let semantic = SemanticFileContext {
            declarations: vec![SemanticDeclaration {
                stable_id: "usr:c:@S@Demo@FI@m_value".to_string(),
                provenance: SemanticIdProvenance::Usr,
                name: "m_value".to_string(),
                kind: ClangSymbolKind::Field,
                line: 10,
                column: 5,
                usr: Some("c:@S@Demo@FI@m_value".to_string()),
                scope_usr: Some("c:@S@Demo".to_string()),
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
            SemanticContextQuery::from_semantic_file_context(Some(&semantic)),
            Some(&snapshot),
        );
        let signal = query
            .project_signal_for_line(10, 5)
            .expect("project signal");
        assert_eq!(signal.reference_count, 42);
        assert_eq!(signal.file_count, 7);
    }
}
