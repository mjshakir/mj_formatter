use std::collections::{BTreeMap, BTreeSet};

use crate::parser::clang_types::ClangSymbolKind;
use crate::parser::file_context::{
    SemanticFileContext, SemanticIdProvenance, SemanticScopeKind,
};

use super::{SemanticContractSnapshot, SemanticScopeCounts};

pub(super) fn build(context: &SemanticFileContext) -> SemanticContractSnapshot {
    let mut usr_decl_reference_counts = BTreeMap::<String, usize>::new();
    let mut usr_decl_lines = BTreeMap::<String, usize>::new();
    let mut declaration_stable_ids_by_line = BTreeMap::<usize, BTreeSet<String>>::new();
    let mut declaration_kind_by_stable_id = BTreeMap::new();
    let mut declaration_stable_ids = BTreeSet::<String>::new();
    let mut stable_id_decl_lines = BTreeMap::<String, usize>::new();
    let mut reference_stable_id_counts = BTreeMap::<String, usize>::new();
    let mut reference_stable_id_first_line = BTreeMap::<String, usize>::new();
    let mut scope_counts = SemanticScopeCounts::default();
    let mut scope_ranges_by_kind = BTreeMap::<String, BTreeSet<(usize, usize)>>::new();
    let mut symbol_identity_issue_count = 0usize;
    let mut symbol_identity_issue_lines = BTreeSet::<usize>::new();
    let mut usage_role_mismatch_count = 0usize;
    let mut usage_role_mismatch_lines = BTreeSet::<usize>::new();
    let mut preprocessor_ranges = Vec::<(usize, usize)>::new();

    for declaration in &context.declarations {
        if declaration.kind == ClangSymbolKind::FunctionTemplate {
            continue;
        }
        declaration_stable_ids.insert(declaration.stable_id.clone());
        declaration_stable_ids_by_line
            .entry(declaration.line.max(1))
            .or_default()
            .insert(declaration.stable_id.clone());
        declaration_kind_by_stable_id.insert(declaration.stable_id.clone(), declaration.kind);
        stable_id_decl_lines.insert(declaration.stable_id.clone(), declaration.line.max(1));

        let stable_id_looks_usr = declaration.stable_id.starts_with("usr:");
        let is_usr_backed = declaration.provenance == SemanticIdProvenance::Usr
            || stable_id_looks_usr
            || declaration
                .usr
                .as_deref()
                .map(str::trim)
                .is_some_and(|value| !value.is_empty());
        if is_usr_backed {
            usr_decl_reference_counts
                .entry(declaration.stable_id.clone())
                .or_insert(0);
            usr_decl_lines.insert(declaration.stable_id.clone(), declaration.line.max(1));
        }

        if (declaration.provenance == SemanticIdProvenance::Usr && !stable_id_looks_usr)
            || (declaration.provenance != SemanticIdProvenance::Usr && stable_id_looks_usr)
        {
            symbol_identity_issue_count = symbol_identity_issue_count.saturating_add(1);
            symbol_identity_issue_lines.insert(declaration.line.max(1));
        }
    }

    for reference in &context.references {
        reference_stable_id_counts
            .entry(reference.stable_id.clone())
            .and_modify(|count| *count = count.saturating_add(1))
            .or_insert(1);
        reference_stable_id_first_line
            .entry(reference.stable_id.clone())
            .or_insert(reference.line.max(1));

        if let Some(count) = usr_decl_reference_counts.get_mut(reference.stable_id.as_str()) {
            *count = count.saturating_add(1);
        }

        if let Some(decl_kind) = declaration_kind_by_stable_id.get(reference.stable_id.as_str()) {
            if *decl_kind != reference.decl_kind {
                usage_role_mismatch_count = usage_role_mismatch_count.saturating_add(1);
                usage_role_mismatch_lines.insert(reference.line.max(1));
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
                scope_counts.namespace = scope_counts.namespace.saturating_add(1);
                scope_ranges_by_kind
                    .entry("namespace".to_string())
                    .or_default()
                    .insert(range);
            }
            SemanticScopeKind::Type => {
                scope_counts.type_scope = scope_counts.type_scope.saturating_add(1);
                scope_ranges_by_kind
                    .entry("type".to_string())
                    .or_default()
                    .insert(range);
            }
            SemanticScopeKind::Function => {
                scope_counts.function = scope_counts.function.saturating_add(1);
                scope_ranges_by_kind
                    .entry("function".to_string())
                    .or_default()
                    .insert(range);
            }
            SemanticScopeKind::Preprocessor => {
                scope_counts.preprocessor = scope_counts.preprocessor.saturating_add(1);
                scope_ranges_by_kind
                    .entry("preprocessor".to_string())
                    .or_default()
                    .insert(range);
                preprocessor_ranges.push(range);
            }
            SemanticScopeKind::Template => {
                scope_ranges_by_kind
                    .entry("template".to_string())
                    .or_default()
                    .insert(range);
            }
            SemanticScopeKind::Attribute => {
                scope_ranges_by_kind
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
        usr_decl_reference_counts,
        usr_decl_lines,
        declaration_stable_ids_by_line,
        declaration_kind_by_stable_id,
        declaration_stable_ids,
        stable_id_decl_lines,
        reference_stable_id_counts,
        reference_stable_id_first_line,
        scope_counts,
        scope_ranges_by_kind,
        symbol_identity_issue_count,
        symbol_identity_issue_lines,
        usage_role_mismatch_count,
        usage_role_mismatch_lines,
        preprocessor_ranges,
    }
}
