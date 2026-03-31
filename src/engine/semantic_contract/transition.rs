use std::collections::BTreeSet;

use super::{SemanticContract, SemanticContractSnapshot, SemanticTransitionAssessment};

pub(super) fn evaluate(
    before: &SemanticContractSnapshot,
    after: &SemanticContractSnapshot,
    ref_drop_tol: usize,
    scope_drift_tol: usize,
    identity_shift_tol: usize,
    edited_lines: Option<&BTreeSet<usize>>,
    adaptive: &crate::engine::certainty_filter::CertaintyFilterState,
) -> SemanticTransitionAssessment {
    let mut assessment = SemanticTransitionAssessment::default();

    evaluate_identity(before, after, identity_shift_tol, edited_lines, &mut assessment, adaptive);
    evaluate_reference_integrity(
        before,
        after,
        ref_drop_tol,
        &mut assessment,
        adaptive,
    );
    evaluate_scope_integrity(
        before,
        after,
        scope_drift_tol,
        edited_lines,
        &mut assessment,
        adaptive,
    );
    evaluate_macro_region_safety(before, after, edited_lines, &mut assessment);

    assessment
}

fn evaluate_identity(
    before: &SemanticContractSnapshot,
    after: &SemanticContractSnapshot,
    line_shift_tolerance: usize,
    edited_lines: Option<&BTreeSet<usize>>,
    assessment: &mut SemanticTransitionAssessment,
    adaptive: &crate::engine::certainty_filter::CertaintyFilterState,
) {
    let mut missing_ids = Vec::<String>::new();
    let mut missing_lines = BTreeSet::<usize>::new();

    for stable_id in before
        .identity.decl_ids
        .difference(&after.identity.decl_ids)
    {
        let declaration_line = before
            .identity.id_decl_lines
            .get(stable_id)
            .copied()
            .unwrap_or(0);
        let declaration_kind = before.identity.kind_by_decl_id.get(stable_id).copied();
        let before_reference_count = before
            .identity.ref_id_counts
            .get(stable_id)
            .copied()
            .unwrap_or_else(|| {
                before
                    .identity.usr_ref_counts
                    .get(stable_id)
                    .copied()
                    .unwrap_or(0)
            });

        if SemanticContract::identity_migrated_locally(
            stable_id,
            declaration_line,
            declaration_kind,
            before_reference_count,
            after,
            line_shift_tolerance,
        ) {
            continue;
        }

        if declaration_line > 0 {
            missing_lines.insert(declaration_line);
        }
        missing_ids.push(stable_id.clone());
    }

    let identity_issue_delta = after
        .issues.identity_count
        .saturating_sub(before.issues.identity_count);
    if identity_issue_delta > 0 {
        missing_lines.extend(after.issues.identity_lines.iter().copied());
    }

    let total_stable_ids = before.identity.decl_ids.len().max(1);
    let identity_severity_raw = (missing_ids.len() + identity_issue_delta) as f64 / total_stable_ids as f64;
    assessment.identity_severity = identity_severity_raw.clamp(0.0, 1.0);

    if !missing_ids.is_empty() || identity_issue_delta > 0 {
        assessment.identity_integrity_regressed = true;
        let identity_penalty = adaptive.identity_penalty();
        assessment.failure_score_delta = assessment
            .failure_score_delta
            .saturating_add(identity_penalty)
            .saturating_add((missing_ids.len().min(8) as u32).saturating_mul(4));
        assessment
            .culprit_lines
            .extend(missing_lines.iter().copied());
        assessment.failure_messages.push(format!(
            "post-edit check failed: semantic symbol identity regressed (missing_stable_ids={}, added_identity_issues={}, severity={:.3}, lines={})",
            missing_ids.len(),
            identity_issue_delta,
            assessment.identity_severity,
            line_hint(missing_lines.iter().copied(), 8)
        ));
    } else if let Some(lines) = edited_lines {
        if !lines.is_empty()
            && before.summary.semantic_signature != after.summary.semantic_signature
        {
            assessment.warning_messages.push(format!(
                "post-edit check warning: semantic identity signature changed under edited lines {}",
                line_hint(lines.iter().copied(), 8)
            ));
        }
    }
}

fn evaluate_reference_integrity(
    before: &SemanticContractSnapshot,
    after: &SemanticContractSnapshot,
    ref_drop_tol: usize,
    assessment: &mut SemanticTransitionAssessment,
    adaptive: &crate::engine::certainty_filter::CertaintyFilterState,
) {
    let mut culprit_lines = BTreeSet::<usize>::new();
    let mut drop_events = 0usize;

    for (stable_id, before_count) in &before.identity.usr_ref_counts {
        let after_count = after
            .identity.usr_ref_counts
            .get(stable_id)
            .copied()
            .unwrap_or(0);
        if *before_count > after_count.saturating_add(ref_drop_tol) {
            drop_events = drop_events.saturating_add(1);
            if let Some(line) = before.identity.usr_decl_lines.get(stable_id) {
                culprit_lines.insert((*line).max(1));
            }
        }
    }

    let mut orphan_lines = BTreeSet::<usize>::new();
    let mut orphan_count = 0usize;
    for (stable_id, after_count) in &after.identity.ref_id_counts {
        if *after_count == 0 || after.identity.decl_ids.contains(stable_id) {
            continue;
        }
        let before_orphan_count = if before.identity.decl_ids.contains(stable_id) {
            0
        } else {
            before
                .identity.ref_id_counts
                .get(stable_id)
                .copied()
                .unwrap_or(0)
        };
        if *after_count > before_orphan_count {
            orphan_count =
                orphan_count.saturating_add(after_count.saturating_sub(before_orphan_count));
            if let Some(line) = after.identity.ref_first_line.get(stable_id) {
                orphan_lines.insert((*line).max(1));
            }
        }
    }

    let usage_mismatch_delta = after
        .issues.mismatch_count
        .saturating_sub(before.issues.mismatch_count);
    if usage_mismatch_delta > 0 {
        culprit_lines.extend(after.issues.mismatch_lines.iter().copied());
    }

    let total_references = before.identity.usr_ref_counts.len().max(1);
    let ref_severity_raw = (drop_events + usage_mismatch_delta + orphan_count) as f64 / total_references as f64;
    assessment.reference_severity = ref_severity_raw.clamp(0.0, 1.0);

    if drop_events > 0 {
        assessment.reference_integrity_regressed = true;
        let reference_penalty = adaptive.reference_penalty();
        assessment.failure_score_delta = assessment
            .failure_score_delta
            .saturating_add(reference_penalty)
            .saturating_add((drop_events.min(16) as u32).saturating_mul(3));
        assessment
            .culprit_lines
            .extend(culprit_lines.iter().copied());
        assessment.failure_messages.push(format!(
            "post-edit check failed: semantic declaration-reference connectivity regressed (drops={}, tolerance={}, lines={})",
            drop_events,
            ref_drop_tol,
            line_hint(culprit_lines.iter().copied(), 8)
        ));
    }

    if usage_mismatch_delta > 0 {
        assessment.reference_integrity_regressed = true;
        let usage_penalty = adaptive.usage_penalty();
        assessment.failure_score_delta = assessment
            .failure_score_delta
            .saturating_add(usage_penalty)
            .saturating_add((usage_mismatch_delta.min(16) as u32).saturating_mul(2));
        assessment
            .culprit_lines
            .extend(after.issues.mismatch_lines.iter().copied());
        assessment.failure_messages.push(format!(
            "post-edit check failed: semantic usage-role consistency regressed ({} -> {}, lines={})",
            before.issues.mismatch_count,
            after.issues.mismatch_count,
            line_hint(after.issues.mismatch_lines.iter().copied(), 8)
        ));
    }

    if orphan_count > 0 {
        assessment.reference_integrity_regressed = true;
        let orphan_penalty = adaptive.orphan_penalty();
        assessment.failure_score_delta = assessment
            .failure_score_delta
            .saturating_add(orphan_penalty)
            .saturating_add((orphan_count.min(16) as u32).saturating_mul(2));
        assessment
            .culprit_lines
            .extend(orphan_lines.iter().copied());
        assessment.failure_messages.push(format!(
            "post-edit check failed: orphan semantic references introduced (count={}, lines={})",
            orphan_count,
            line_hint(orphan_lines.iter().copied(), 8)
        ));
    }
}

fn evaluate_scope_integrity(
    before: &SemanticContractSnapshot,
    after: &SemanticContractSnapshot,
    scope_drift_tol: usize,
    edited_lines: Option<&BTreeSet<usize>>,
    assessment: &mut SemanticTransitionAssessment,
    adaptive: &crate::engine::certainty_filter::CertaintyFilterState,
) {
    let count_drift = scope_count_drift(before, after);
    let (local_range_drift, remote_range_drift, range_culprit_lines) =
        scope_range_drift(before, after, edited_lines, 1);

    let total_scopes = (before.scopes.counts.namespace + before.scopes.counts.type_scope + before.scopes.counts.function).max(1);
    let scope_severity_raw = (count_drift + remote_range_drift) as f64 / total_scopes as f64;
    assessment.scope_severity = scope_severity_raw.clamp(0.0, 1.0);

    let regressed = count_drift > scope_drift_tol
        || remote_range_drift > scope_drift_tol;
    if !regressed {
        return;
    }

    assessment.scope_integrity_regressed = true;
    let scope_penalty = adaptive.scope_penalty();
    assessment.failure_score_delta = assessment.failure_score_delta.saturating_add(scope_penalty);
    assessment
        .culprit_lines
        .extend(range_culprit_lines.iter().copied());

    assessment.failure_messages.push(format!(
        "post-edit check failed: semantic scope structural drift detected (count_drift={}, local_range_drift={}, remote_range_drift={}, tolerance={})",
        count_drift,
        local_range_drift,
        remote_range_drift,
        scope_drift_tol
    ));
}

fn evaluate_macro_region_safety(
    before: &SemanticContractSnapshot,
    after: &SemanticContractSnapshot,
    edited_lines: Option<&BTreeSet<usize>>,
    assessment: &mut SemanticTransitionAssessment,
) {
    let Some(edited_lines) = edited_lines else {
        return;
    };
    if edited_lines.is_empty() {
        return;
    }

    let mut touched = BTreeSet::<usize>::new();
    for line in edited_lines {
        if SemanticContract::line_in_ranges(*line, before.scopes.preprocessor_ranges.as_slice())
            || SemanticContract::line_in_ranges(*line, after.scopes.preprocessor_ranges.as_slice())
        {
            touched.insert(*line);
        }
    }

    if touched.is_empty() {
        return;
    }

    assessment.warning_messages.push(format!(
        "post-edit check warning: macro-safety clause triggered for edited lines {}",
        line_hint(touched.iter().copied(), 8)
    ));
}

fn scope_count_drift(before: &SemanticContractSnapshot, after: &SemanticContractSnapshot) -> usize {
    before
        .scopes.counts
        .namespace
        .abs_diff(after.scopes.counts.namespace)
        + before
            .scopes.counts
            .type_scope
            .abs_diff(after.scopes.counts.type_scope)
        + before
            .scopes.counts
            .function
            .abs_diff(after.scopes.counts.function)
        + before
            .scopes.counts
            .preprocessor
            .abs_diff(after.scopes.counts.preprocessor)
}

fn scope_range_drift(
    before: &SemanticContractSnapshot,
    after: &SemanticContractSnapshot,
    edited_lines: Option<&BTreeSet<usize>>,
    radius: usize,
) -> (usize, usize, BTreeSet<usize>) {
    let empty = BTreeSet::<(usize, usize)>::new();
    let mut local = 0usize;
    let mut remote = 0usize;
    let mut culprit_lines = BTreeSet::<usize>::new();
    use crate::parser::ts_cpp_symbols;
    for kind in [
        ts_cpp_symbols::sym_namespace_definition,
        ts_cpp_symbols::sym_class_specifier,
        ts_cpp_symbols::sym_struct_specifier,
        ts_cpp_symbols::sym_union_specifier,
        ts_cpp_symbols::sym_enum_specifier,
        ts_cpp_symbols::sym_function_definition,
        ts_cpp_symbols::sym_function_declarator,
        ts_cpp_symbols::sym_lambda_expression,
        ts_cpp_symbols::sym_preproc_if,
        ts_cpp_symbols::sym_preproc_ifdef,
        ts_cpp_symbols::sym_preproc_elif,
        ts_cpp_symbols::sym_preproc_else,
        ts_cpp_symbols::sym_preproc_def,
        ts_cpp_symbols::sym_preproc_function_def,
    ] {
        let before_ranges = before.scopes.ranges_by_kind.get(&kind).unwrap_or(&empty);
        let after_ranges = after.scopes.ranges_by_kind.get(&kind).unwrap_or(&empty);

        for range in before_ranges.difference(after_ranges) {
            classify_range(*range, edited_lines, radius, &mut local, &mut remote);
            culprit_lines.insert(range.0.max(1));
        }
        for range in after_ranges.difference(before_ranges) {
            classify_range(*range, edited_lines, radius, &mut local, &mut remote);
            culprit_lines.insert(range.0.max(1));
        }
    }
    (local, remote, culprit_lines)
}

fn classify_range(
    range: (usize, usize),
    edited_lines: Option<&BTreeSet<usize>>,
    radius: usize,
    local: &mut usize,
    remote: &mut usize,
) {
    if edited_lines
        .is_some_and(|lines| SemanticContract::range_near_edited_lines(range, lines, radius))
    {
        *local = local.saturating_add(1);
    } else {
        *remote = remote.saturating_add(1);
    }
}

fn line_hint(lines: impl Iterator<Item = usize>, max: usize) -> String {
    let mut sample = lines.take(max).collect::<Vec<_>>();
    sample.sort_unstable();
    sample.dedup();
    if sample.is_empty() {
        return "none".to_string();
    }
    sample
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join(",")
}
