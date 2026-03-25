use std::collections::{BTreeMap, BTreeSet};

use crate::parser::file_context::{
    SemanticFileContext, SemanticIdProvenance, SemanticScopeKind,
};

use super::{
    SemanticContractSnapshot, SemanticIssues, SemanticScopeCounts, ScopeStructure, SymbolIdentity,
};

pub(super) fn build(context: &SemanticFileContext) -> SemanticContractSnapshot {
    let mut usr_ref_counts = BTreeMap::<String, usize>::new();
    let mut usr_decl_lines = BTreeMap::<String, usize>::new();
    let mut decl_ids_by_line = BTreeMap::<usize, BTreeSet<String>>::new();
    let mut kind_by_decl_id = BTreeMap::new();
    let mut decl_ids = BTreeSet::<String>::new();
    let mut id_decl_lines = BTreeMap::<String, usize>::new();
    let mut ref_id_counts = BTreeMap::<String, usize>::new();
    let mut ref_first_line = BTreeMap::<String, usize>::new();
    let mut counts = SemanticScopeCounts::default();
    let mut ranges_by_kind = BTreeMap::<String, BTreeSet<(usize, usize)>>::new();
    let mut identity_count = 0usize;
    let mut identity_lines = BTreeSet::<usize>::new();
    let mut mismatch_count = 0usize;
    let mut mismatch_lines = BTreeSet::<usize>::new();
    let mut preprocessor_ranges = Vec::<(usize, usize)>::new();

    for declaration in &context.declarations {
        if declaration.kind == clang_sys::CXCursor_FunctionTemplate {
            continue;
        }
        decl_ids.insert(declaration.stable_id.clone());
        decl_ids_by_line
            .entry(declaration.line.max(1))
            .or_default()
            .insert(declaration.stable_id.clone());
        kind_by_decl_id.insert(declaration.stable_id.clone(), declaration.kind);
        id_decl_lines.insert(declaration.stable_id.clone(), declaration.line.max(1));

        let stable_id_looks_usr = declaration.stable_id.starts_with("usr:");
        let is_usr_backed = declaration.provenance == SemanticIdProvenance::Usr
            || stable_id_looks_usr
            || declaration
                .usr
                .as_deref()
                .map(str::trim)
                .is_some_and(|value| !value.is_empty());
        if is_usr_backed {
            usr_ref_counts
                .entry(declaration.stable_id.clone())
                .or_insert(0);
            usr_decl_lines.insert(declaration.stable_id.clone(), declaration.line.max(1));
        }

        if (declaration.provenance == SemanticIdProvenance::Usr && !stable_id_looks_usr)
            || (declaration.provenance != SemanticIdProvenance::Usr && stable_id_looks_usr)
        {
            identity_count = identity_count.saturating_add(1);
            identity_lines.insert(declaration.line.max(1));
        }
    }

    for reference in &context.references {
        ref_id_counts
            .entry(reference.stable_id.clone())
            .and_modify(|count| *count = count.saturating_add(1))
            .or_insert(1);
        ref_first_line
            .entry(reference.stable_id.clone())
            .or_insert(reference.line.max(1));

        if let Some(count) = usr_ref_counts.get_mut(reference.stable_id.as_str()) {
            *count = count.saturating_add(1);
        }

        if let Some(decl_kind) = kind_by_decl_id.get(reference.stable_id.as_str()) {
            if *decl_kind != reference.decl_kind {
                mismatch_count = mismatch_count.saturating_add(1);
                mismatch_lines.insert(reference.line.max(1));
            }
        }
    }

    for scope in &context.scopes {
        let range = (
            scope.start_line.max(1),
            scope.end_line.max(scope.start_line).max(1),
        );
        match scope.kind {
            SemanticScopeKind::Namespace => {
                counts.namespace = counts.namespace.saturating_add(1);
                ranges_by_kind
                    .entry("namespace".to_string())
                    .or_default()
                    .insert(range);
            }
            SemanticScopeKind::Type => {
                counts.type_scope = counts.type_scope.saturating_add(1);
                ranges_by_kind
                    .entry("type".to_string())
                    .or_default()
                    .insert(range);
            }
            SemanticScopeKind::Function => {
                counts.function = counts.function.saturating_add(1);
                ranges_by_kind
                    .entry("function".to_string())
                    .or_default()
                    .insert(range);
            }
            SemanticScopeKind::Preprocessor => {
                counts.preprocessor = counts.preprocessor.saturating_add(1);
                ranges_by_kind
                    .entry("preprocessor".to_string())
                    .or_default()
                    .insert(range);
                preprocessor_ranges.push(range);
            }
            SemanticScopeKind::Template => {
                ranges_by_kind
                    .entry("template".to_string())
                    .or_default()
                    .insert(range);
            }
            SemanticScopeKind::Attribute => {
                ranges_by_kind
                    .entry("attribute".to_string())
                    .or_default()
                    .insert(range);
            }
        }
    }

    preprocessor_ranges.sort_unstable();
    preprocessor_ranges.dedup();

    SemanticContractSnapshot {
        summary: context.summary(),
        identity: SymbolIdentity {
            usr_ref_counts,
            usr_decl_lines,
            decl_ids_by_line,
            kind_by_decl_id,
            decl_ids,
            id_decl_lines,
            ref_id_counts,
            ref_first_line,
        },
        scopes: ScopeStructure {
            counts,
            ranges_by_kind,
            preprocessor_ranges,
        },
        issues: SemanticIssues {
            identity_count,
            identity_lines,
            mismatch_count,
            mismatch_lines,
        },
    }
}
