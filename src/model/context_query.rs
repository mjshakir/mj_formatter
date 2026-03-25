use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};

use crate::parser::clang_result::ClangDiagnosticSeverity;
use crate::parser::file_context::{
    SemanticDeclaration, SemanticFileContext, SemanticReference, SemanticScope, SemanticScopeKind,
    SourceLocation,
};
use crate::parser::semantic_region::{SemanticRegion, SemanticRegionKind};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticLineRole {
    Unknown,
    Declaration,
    Reference,
    Mixed,
    Structural,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticLineScope {
    Unknown,
    Global,
    Namespace,
    Type,
    Function,
    Preprocessor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticLineProfile {
    pub line: usize,
    pub role: SemanticLineRole,
    pub scope: SemanticLineScope,
    pub declaration_count: usize,
    pub reference_count: usize,
    pub in_macro_region: bool,
    pub has_diagnostic_error: bool,
    pub safe_edit: bool,
}

#[derive(Clone, Debug)]
pub struct SemanticContextQuery<'a> {
    semantic: Option<&'a SemanticFileContext>,
    preprocessor_ranges: Vec<(usize, usize)>,
    diagnostic_error_lines: BTreeSet<usize>,
    declaration_counts_by_line: BTreeMap<usize, usize>,
    reference_counts_by_line: BTreeMap<usize, usize>,
}

impl<'a> SemanticContextQuery<'a> {
    pub fn from_semantic(semantic: Option<&'a SemanticFileContext>) -> Self {
        let mut preprocessor_ranges = Vec::<(usize, usize)>::new();
        let mut diagnostic_error_lines = BTreeSet::<usize>::new();
        let mut declaration_counts_by_line = BTreeMap::<usize, usize>::new();
        let mut reference_counts_by_line = BTreeMap::<usize, usize>::new();

        if let Some(context) = semantic {
            for scope in &context.scopes {
                if scope.kind == SemanticScopeKind::Preprocessor {
                    preprocessor_ranges.push((scope.start_line, scope.end_line));
                }
            }
            preprocessor_ranges.sort_unstable_by(|left, right| left.0.cmp(&right.0));
            for entry in &context.diagnostic_entries {
                if entry.severity == ClangDiagnosticSeverity::Error
                    || entry.severity == ClangDiagnosticSeverity::Fatal
                {
                    diagnostic_error_lines.insert(entry.line);
                }
            }
            for declaration in &context.declarations {
                if declaration.line == 0 {
                    continue;
                }
                *declaration_counts_by_line
                    .entry(declaration.line)
                    .or_insert(0) += 1;
            }
            for reference in &context.references {
                if reference.line == 0 {
                    continue;
                }
                *reference_counts_by_line.entry(reference.line).or_insert(0) += 1;
            }
        }

        Self {
            semantic,
            preprocessor_ranges,
            diagnostic_error_lines,
            declaration_counts_by_line,
            reference_counts_by_line,
        }
    }

    pub fn is_available(&self) -> bool {
        self.semantic.is_some()
    }

    pub fn symbol_at(
        &self,
        line: usize,
        column: usize,
        allowed_kinds: &[i32],
    ) -> Option<&'a SemanticDeclaration> {
        self.symbol_at_location(SourceLocation::new(line, column), allowed_kinds)
    }

    pub fn symbol_at_location(
        &self,
        location: SourceLocation,
        allowed_kinds: &[i32],
    ) -> Option<&'a SemanticDeclaration> {
        let semantic = self.semantic?;
        let allow_kind =
            |kind: i32| allowed_kinds.is_empty() || allowed_kinds.contains(&kind);
        if let Some(declaration) = semantic.declaration_at_location(location, allowed_kinds) {
            return Some(declaration);
        }

        let reference = semantic.references.iter().find(|reference| {
            reference.line == location.line && reference.column == location.column
        })?;
        semantic
            .declarations
            .iter()
            .find(|declaration| declaration.stable_id == reference.stable_id)
            .filter(|declaration| allow_kind(declaration.kind))
    }

    pub fn decl_by_id(&self, stable_id: &str) -> Option<&'a SemanticDeclaration> {
        self.semantic?
            .declarations
            .iter()
            .find(|declaration| declaration.stable_id == stable_id)
    }

    pub fn references_of(&self, stable_id: &str) -> Vec<&'a SemanticReference> {
        self.semantic
            .map(|semantic| semantic.refs_by_id(stable_id))
            .unwrap_or_default()
    }

    pub fn scope_at(&self, line: usize, _column: usize) -> Option<&'a SemanticScope> {
        self.scope_at_location(SourceLocation::new(line, 1))
    }

    pub fn scope_at_location(&self, location: SourceLocation) -> Option<&'a SemanticScope> {
        let semantic = self.semantic?;
        semantic.scope_at_location(location)
    }

    pub fn region_at(&self, line: usize, column: usize) -> Option<&'a SemanticRegion> {
        self.region_at_location(SourceLocation::new(line, column))
    }

    pub fn region_at_location(&self, location: SourceLocation) -> Option<&'a SemanticRegion> {
        let semantic = self.semantic?;
        semantic.region_at_location(location)
    }

    pub fn regions_for_line(&self, line: usize) -> Vec<&'a SemanticRegion> {
        let Some(semantic) = self.semantic else {
            return Vec::new();
        };
        let line = line.max(1);
        let mut regions = semantic
            .regions
            .iter()
            .filter(|region| region.contains_line(line))
            .collect::<Vec<_>>();
        regions.sort_by(|left, right| {
            left.width_lines()
                .cmp(&right.width_lines())
                .then(left.start_offset.cmp(&right.start_offset))
                .then(left.id.cmp(&right.id))
        });
        regions
    }

    pub fn is_macro_region(&self, line: usize, _column: usize) -> bool {
        self.is_macro_region_at(SourceLocation::new(line, 1))
    }

    pub fn is_macro_region_at(&self, location: SourceLocation) -> bool {
        self.preprocessor_ranges
            .binary_search_by(|&(start, end)| {
                if location.line < start {
                    std::cmp::Ordering::Greater
                } else if location.line > end {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .is_ok()
    }

    pub fn has_diag_error(&self, line: usize) -> bool {
        self.diagnostic_error_lines.contains(&line)
    }

    pub fn is_safe_edit(&self, line: usize, column: usize) -> bool {
        self.is_safe_edit_at(SourceLocation::new(line, column))
    }

    pub fn is_safe_edit_at(&self, location: SourceLocation) -> bool {
        if !self.is_available() {
            return false;
        }
        if self.is_macro_region(location.line, location.column) {
            return false;
        }
        if self.has_diag_error(location.line) {
            // Keep semantic edits strict on diagnostic lines, but allow
            // structural/comment-only edits where no declaration/reference is
            // attached to the line.
            let declaration_count = self
                .declaration_counts_by_line
                .get(&location.line)
                .copied()
                .unwrap_or(0);
            let reference_count = self
                .reference_counts_by_line
                .get(&location.line)
                .copied()
                .unwrap_or(0);
            if declaration_count > 0 || reference_count > 0 {
                return false;
            }
        }
        if self.has_diag_error(location.line)
            && self
                .scope_at(location.line, location.column)
                .is_some_and(|scope| scope.kind == SemanticScopeKind::Preprocessor)
        {
            return false;
        }
        true
    }

    pub fn is_safe_global(&self, line: usize, column: usize) -> bool {
        if !self.is_available() {
            return false;
        }
        if self.has_diag_error(line) {
            return false;
        }
        if self.is_macro_region(line, column) {
            return true;
        }
        self.scope_at(line, column).is_none()
    }

    pub fn line_profile(&self, line: usize) -> SemanticLineProfile {
        let line = line.max(1);
        let declaration_count = self
            .declaration_counts_by_line
            .get(&line)
            .copied()
            .unwrap_or(0);
        let reference_count = self
            .reference_counts_by_line
            .get(&line)
            .copied()
            .unwrap_or(0);
        let in_macro_region = self.is_macro_region(line, 1);
        let has_diagnostic_error = self.has_diag_error(line);
        let scope = self.line_scope(line);
        let role = Self::line_role_for_counts(declaration_count, reference_count, scope);
        let safe_edit = self.is_safe_edit(line, 1);
        SemanticLineProfile {
            line,
            role,
            scope,
            declaration_count,
            reference_count,
            in_macro_region,
            has_diagnostic_error,
            safe_edit,
        }
    }

    pub fn context_cluster_key(&self, lines: &BTreeSet<usize>) -> u64 {
        if lines.is_empty() {
            return 0;
        }
        let mut macro_lines = 0usize;
        let mut diagnostic_lines = 0usize;
        let mut declaration_lines = 0usize;
        let mut reference_lines = 0usize;
        let mut function_lines = 0usize;
        let mut type_lines = 0usize;
        let mut namespace_lines = 0usize;
        let mut preprocessor_lines = 0usize;
        let mut diagnostic_region_lines = 0usize;
        let mut declaration_region_lines = 0usize;
        let mut reference_region_lines = 0usize;

        for line in lines {
            let profile = self.line_profile(*line);
            if profile.in_macro_region {
                macro_lines = macro_lines.saturating_add(1);
            }
            if profile.has_diagnostic_error {
                diagnostic_lines = diagnostic_lines.saturating_add(1);
            }
            if profile.declaration_count > 0 {
                declaration_lines = declaration_lines.saturating_add(1);
            }
            if profile.reference_count > 0 {
                reference_lines = reference_lines.saturating_add(1);
            }
            match profile.scope {
                SemanticLineScope::Function => {
                    function_lines = function_lines.saturating_add(1);
                }
                SemanticLineScope::Type => {
                    type_lines = type_lines.saturating_add(1);
                }
                SemanticLineScope::Namespace => {
                    namespace_lines = namespace_lines.saturating_add(1);
                }
                SemanticLineScope::Preprocessor => {
                    preprocessor_lines = preprocessor_lines.saturating_add(1);
                }
                _ => {}
            }
            if let Some(region) = self.region_at(*line, 1) {
                match region.kind {
                    SemanticRegionKind::Diagnostic => {
                        diagnostic_region_lines = diagnostic_region_lines.saturating_add(1)
                    }
                    SemanticRegionKind::Declaration => {
                        declaration_region_lines = declaration_region_lines.saturating_add(1)
                    }
                    SemanticRegionKind::Reference => {
                        reference_region_lines = reference_region_lines.saturating_add(1)
                    }
                    _ => {}
                }
            }
        }

        let total = lines.len().max(1);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        total.hash(&mut hasher);
        Self::ratio_bucket(macro_lines, total, 8).hash(&mut hasher);
        Self::ratio_bucket(diagnostic_lines, total, 8).hash(&mut hasher);
        Self::ratio_bucket(declaration_lines, total, 8).hash(&mut hasher);
        Self::ratio_bucket(reference_lines, total, 8).hash(&mut hasher);
        Self::ratio_bucket(function_lines, total, 8).hash(&mut hasher);
        Self::ratio_bucket(type_lines, total, 8).hash(&mut hasher);
        Self::ratio_bucket(namespace_lines, total, 8).hash(&mut hasher);
        Self::ratio_bucket(preprocessor_lines, total, 8).hash(&mut hasher);
        Self::ratio_bucket(diagnostic_region_lines, total, 8).hash(&mut hasher);
        Self::ratio_bucket(declaration_region_lines, total, 8).hash(&mut hasher);
        Self::ratio_bucket(reference_region_lines, total, 8).hash(&mut hasher);
        lines.first().copied().unwrap_or_default().hash(&mut hasher);
        lines.last().copied().unwrap_or_default().hash(&mut hasher);
        hasher.finish()
    }

    fn line_role_for_counts(
        declaration_count: usize,
        reference_count: usize,
        scope: SemanticLineScope,
    ) -> SemanticLineRole {
        if declaration_count > 0 && reference_count > 0 {
            return SemanticLineRole::Mixed;
        }
        if declaration_count > 0 {
            return SemanticLineRole::Declaration;
        }
        if reference_count > 0 {
            return SemanticLineRole::Reference;
        }
        if scope != SemanticLineScope::Unknown {
            return SemanticLineRole::Structural;
        }
        SemanticLineRole::Unknown
    }

    fn line_scope(&self, line: usize) -> SemanticLineScope {
        if !self.is_available() {
            return SemanticLineScope::Unknown;
        }
        match self.scope_at(line, 1).map(|scope| scope.kind) {
            Some(SemanticScopeKind::Preprocessor) => SemanticLineScope::Preprocessor,
            Some(SemanticScopeKind::Function) => SemanticLineScope::Function,
            Some(SemanticScopeKind::Type) => SemanticLineScope::Type,
            Some(SemanticScopeKind::Namespace) => SemanticLineScope::Namespace,
            Some(SemanticScopeKind::Template)
            | Some(SemanticScopeKind::Attribute) => SemanticLineScope::Global,
            None => SemanticLineScope::Global,
        }
    }

    fn ratio_bucket(value: usize, total: usize, buckets: usize) -> usize {
        if total == 0 || buckets == 0 {
            return 0;
        }
        let scaled = value.saturating_mul(buckets);
        let bucket = scaled / total;
        bucket.min(buckets)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::model::context_query::SemanticContextQuery;
    use crate::model::context_query::SemanticLineRole;
    use crate::model::context_query::SemanticLineScope;
    use crate::parser::clang_result::{
        ClangDiagnosticEntry, ClangDiagnosticSeverity, ClangDiagnosticSummary,
    };
    use crate::parser::file_context::{
        SemanticDeclaration, SemanticFileContext, SemanticIdProvenance, SemanticReference,
        SemanticScope, SemanticScopeKind,
    };
    use crate::parser::semantic_region::{SemanticRegion, SemanticRegionKind};
    use crate::parser::node_kind;

    #[test]
    fn queries_resolve_stable() {
        let semantic = SemanticFileContext {
            declarations: vec![SemanticDeclaration {
                stable_id: "usr:c:@F@foo#".to_string(),
                provenance: SemanticIdProvenance::Usr,
                name: "foo".to_string(),
                kind: clang_sys::CXCursor_FunctionDecl,
                line: 2,
                column: 5,
                usr: Some("c:@F@foo#".to_string()),
                scope_usr: None,
            }],
            references: vec![
                SemanticReference {
                    stable_id: "usr:c:@F@foo#".to_string(),
                    provenance: SemanticIdProvenance::Usr,
                    decl_path: "a.cpp".to_string(),
                    decl_kind: clang_sys::CXCursor_FunctionDecl,
                    offset: 10,
                    line: 2,
                    column: 5,
                },
                SemanticReference {
                    stable_id: "usr:c:@F@foo#".to_string(),
                    provenance: SemanticIdProvenance::Usr,
                    decl_path: "a.cpp".to_string(),
                    decl_kind: clang_sys::CXCursor_FunctionDecl,
                    offset: 40,
                    line: 4,
                    column: 12,
                },
            ],
            scopes: vec![SemanticScope {
                kind: SemanticScopeKind::Function,
                node_kind: node_kind::FUNCTION_DEFINITION,
                start_offset: 0,
                end_offset: 60,
                start_line: 1,
                end_line: 6,
            }],
            ..SemanticFileContext::default()
        };
        let query = SemanticContextQuery::from_semantic(Some(&semantic));
        let declaration = query
            .symbol_at(2, 5, &[clang_sys::CXCursor_FunctionDecl])
            .expect("declaration at location");
        assert_eq!(declaration.name, "foo");
        assert!(query.decl_by_id("usr:c:@F@foo#").is_some());
        assert!(query.scope_at(2, 1).is_some());
        assert_eq!(query.references_of(declaration.stable_id.as_str()).len(), 2);
    }

    #[test]
    fn blocks_preprocessor_diag() {
        let semantic = SemanticFileContext {
            diagnostic_summary: ClangDiagnosticSummary {
                error: 2,
                ..ClangDiagnosticSummary::default()
            },
            diagnostic_entries: vec![
                ClangDiagnosticEntry {
                    line: 8,
                    column: 2,
                    severity: ClangDiagnosticSeverity::Error,
                },
                ClangDiagnosticEntry {
                    line: 9,
                    column: 2,
                    severity: ClangDiagnosticSeverity::Error,
                },
            ],
            declarations: vec![SemanticDeclaration {
                stable_id: "usr:c:@F@broken#".to_string(),
                provenance: SemanticIdProvenance::Usr,
                name: "broken".to_string(),
                kind: clang_sys::CXCursor_FunctionDecl,
                line: 8,
                column: 1,
                usr: Some("c:@F@broken#".to_string()),
                scope_usr: None,
            }],
            scopes: vec![SemanticScope {
                kind: SemanticScopeKind::Preprocessor,
                node_kind: "preproc_if",
                start_offset: 0,
                end_offset: 20,
                start_line: 1,
                end_line: 4,
            }],
            ..SemanticFileContext::default()
        };
        let query = SemanticContextQuery::from_semantic(Some(&semantic));
        assert!(query.is_macro_region(2, 1));
        assert!(!query.is_safe_edit(2, 1));
        assert!(!query.is_safe_edit(8, 1));
        assert!(query.is_safe_edit(9, 1));
        assert!(query.is_safe_edit(20, 1));
        assert!(query.is_safe_global(2, 1));
        assert!(!query.is_safe_global(8, 1));
        assert!(query.is_safe_global(20, 1));
    }

    #[test]
    fn profile_reports_role() {
        let semantic = SemanticFileContext {
            diagnostic_summary: ClangDiagnosticSummary {
                error: 1,
                ..ClangDiagnosticSummary::default()
            },
            diagnostic_entries: vec![ClangDiagnosticEntry {
                line: 9,
                column: 2,
                severity: ClangDiagnosticSeverity::Error,
            }],
            declarations: vec![SemanticDeclaration {
                stable_id: "usr:c:@F@foo#".to_string(),
                provenance: SemanticIdProvenance::Usr,
                name: "foo".to_string(),
                kind: clang_sys::CXCursor_FunctionDecl,
                line: 3,
                column: 1,
                usr: Some("c:@F@foo#".to_string()),
                scope_usr: None,
            }],
            references: vec![SemanticReference {
                stable_id: "usr:c:@F@foo#".to_string(),
                provenance: SemanticIdProvenance::Usr,
                decl_path: "a.cpp".to_string(),
                decl_kind: clang_sys::CXCursor_FunctionDecl,
                offset: 30,
                line: 5,
                column: 3,
            }],
            scopes: vec![
                SemanticScope {
                    kind: SemanticScopeKind::Preprocessor,
                    node_kind: "preproc_if",
                    start_offset: 0,
                    end_offset: 8,
                    start_line: 1,
                    end_line: 1,
                },
                SemanticScope {
                    kind: SemanticScopeKind::Function,
                    node_kind: node_kind::FUNCTION_DEFINITION,
                    start_offset: 9,
                    end_offset: 80,
                    start_line: 2,
                    end_line: 8,
                },
            ],
            ..SemanticFileContext::default()
        };
        let query = SemanticContextQuery::from_semantic(Some(&semantic));

        let macro_profile = query.line_profile(1);
        assert_eq!(macro_profile.scope, SemanticLineScope::Preprocessor);
        assert_eq!(macro_profile.role, SemanticLineRole::Structural);
        assert!(!macro_profile.safe_edit);

        let declaration_profile = query.line_profile(3);
        assert_eq!(declaration_profile.scope, SemanticLineScope::Function);
        assert_eq!(declaration_profile.role, SemanticLineRole::Declaration);
        assert!(declaration_profile.safe_edit);

        let reference_profile = query.line_profile(5);
        assert_eq!(reference_profile.role, SemanticLineRole::Reference);
        assert!(reference_profile.safe_edit);

        let diagnostic_profile = query.line_profile(9);
        assert_eq!(diagnostic_profile.scope, SemanticLineScope::Global);
        assert!(diagnostic_profile.has_diagnostic_error);
        assert!(diagnostic_profile.safe_edit);
    }

    #[test]
    fn cluster_key_stable() {
        let semantic = SemanticFileContext {
            declarations: vec![SemanticDeclaration {
                stable_id: "usr:c:@F@foo#".to_string(),
                provenance: SemanticIdProvenance::Usr,
                name: "foo".to_string(),
                kind: clang_sys::CXCursor_FunctionDecl,
                line: 3,
                column: 1,
                usr: Some("c:@F@foo#".to_string()),
                scope_usr: None,
            }],
            references: vec![SemanticReference {
                stable_id: "usr:c:@F@foo#".to_string(),
                provenance: SemanticIdProvenance::Usr,
                decl_path: "a.cpp".to_string(),
                decl_kind: clang_sys::CXCursor_FunctionDecl,
                offset: 30,
                line: 5,
                column: 3,
            }],
            scopes: vec![SemanticScope {
                kind: SemanticScopeKind::Function,
                node_kind: node_kind::FUNCTION_DEFINITION,
                start_offset: 9,
                end_offset: 80,
                start_line: 2,
                end_line: 8,
            }],
            ..SemanticFileContext::default()
        };
        let query = SemanticContextQuery::from_semantic(Some(&semantic));
        let first = BTreeSet::from([3usize, 5usize]);
        let second = BTreeSet::from([3usize, 5usize]);
        let third = BTreeSet::from([1usize, 3usize, 5usize]);
        assert_eq!(
            query.context_cluster_key(&first),
            query.context_cluster_key(&second)
        );
        assert_ne!(
            query.context_cluster_key(&first),
            query.context_cluster_key(&third)
        );
    }

    #[test]
    fn region_smallest_covering() {
        let semantic = SemanticFileContext {
            regions: vec![
                SemanticRegion::new(
                    "file.cpp",
                    SemanticRegionKind::File,
                    1,
                    20,
                    0,
                    200,
                    None,
                    false,
                ),
                SemanticRegion::new(
                    "file.cpp",
                    SemanticRegionKind::Function,
                    5,
                    12,
                    40,
                    150,
                    None,
                    false,
                ),
                SemanticRegion::new(
                    "file.cpp",
                    SemanticRegionKind::Declaration,
                    7,
                    7,
                    70,
                    90,
                    Some("usr:c:@F@foo#".to_string()),
                    false,
                ),
            ],
            ..SemanticFileContext::default()
        };
        let query = SemanticContextQuery::from_semantic(Some(&semantic));
        let region = query.region_at(7, 1).expect("region");
        assert_eq!(region.kind, SemanticRegionKind::Declaration);
        assert!(!query.regions_for_line(7).is_empty());
    }
}
