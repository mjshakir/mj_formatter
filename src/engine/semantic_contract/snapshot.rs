use std::collections::BTreeSet;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::parser::file_context::{
    SemanticFileContext, SemanticIdProvenance,
    is_function_scope, is_namespace_scope, is_preprocessor_scope, is_type_scope,
};

use super::{
    SemanticContractSnapshot, SemanticIssues, SemanticScopeCounts, ScopeStructure, SymbolIdentity,
};

pub(super) fn build(context: &SemanticFileContext) -> SemanticContractSnapshot {
    let mut usr_ref_counts = FxHashMap::<String, usize>::default();
    let mut usr_decl_lines = FxHashMap::<String, usize>::default();
    let mut decl_ids_by_line = FxHashMap::<usize, BTreeSet<String>>::default();
    let mut kind_by_decl_id = FxHashMap::default();
    let mut decl_ids = FxHashSet::<String>::default();
    let mut id_decl_lines = FxHashMap::<String, usize>::default();
    let mut ref_id_counts = FxHashMap::<String, usize>::default();
    let mut ref_first_line = FxHashMap::<String, usize>::default();
    let mut counts = SemanticScopeCounts::default();
    let mut ranges_by_kind = FxHashMap::<u16, BTreeSet<(usize, usize)>>::default();
    let mut identity_count = 0usize;
    let mut identity_lines = FxHashSet::<usize>::default();
    let mut mismatch_count = 0usize;
    let mut mismatch_lines = FxHashSet::<usize>::default();
    let mut preprocessor_ranges = Vec::<(usize, usize)>::new();

    for declaration in &context.declarations {
        if declaration.kind == clang_sys::CXCursor_FunctionTemplate {
            continue;
        }
        let id = declaration.stable_id.clone();
        let line = declaration.line.max(1);
        decl_ids.insert(id.clone());
        decl_ids_by_line
            .entry(line)
            .or_default()
            .insert(id.clone());
        kind_by_decl_id.insert(id.clone(), declaration.kind);
        id_decl_lines.insert(id.clone(), line);

        let stable_id_looks_usr = id.starts_with("usr:");
        let is_usr_backed = declaration.provenance == SemanticIdProvenance::Usr
            || stable_id_looks_usr
            || declaration
                .usr
                .as_deref()
                .map(str::trim)
                .is_some_and(|value| !value.is_empty());
        if is_usr_backed {
            usr_ref_counts
                .entry(id.clone())
                .or_insert(0);
            usr_decl_lines.insert(id, line);
        }

        if (declaration.provenance == SemanticIdProvenance::Usr && !stable_id_looks_usr)
            || (declaration.provenance != SemanticIdProvenance::Usr && stable_id_looks_usr)
        {
            identity_count = identity_count.saturating_add(1);
            identity_lines.insert(line);
        }
    }

    for reference in &context.references {
        let ref_id = reference.stable_id.clone();
        ref_id_counts
            .entry(ref_id.clone())
            .and_modify(|count| *count = count.saturating_add(1))
            .or_insert(1);
        ref_first_line
            .entry(ref_id)
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
        if is_namespace_scope(scope.node_kind_id) {
            counts.namespace = counts.namespace.saturating_add(1);
        } else if is_type_scope(scope.node_kind_id) {
            counts.type_scope = counts.type_scope.saturating_add(1);
        } else if is_function_scope(scope.node_kind_id) {
            counts.function = counts.function.saturating_add(1);
        } else if is_preprocessor_scope(scope.node_kind_id) {
            counts.preprocessor = counts.preprocessor.saturating_add(1);
            preprocessor_ranges.push(range);
        }
        ranges_by_kind
            .entry(scope.node_kind_id)
            .or_default()
            .insert(range);
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
