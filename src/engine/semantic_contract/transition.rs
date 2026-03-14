use std::collections::BTreeSet;

use super::{SemanticContract, SemanticContractSnapshot, SemanticTransitionAssessment};

pub(super) fn evaluate(
    before: &SemanticContractSnapshot,
    after: &SemanticContractSnapshot,
    semantic_reference_drop_tolerance: usize,
    semantic_scope_drift_tolerance: usize,
    identity_line_shift_tolerance: usize,
    edited_lines: Option<&BTreeSet<usize>>,
) -> SemanticTransitionAssessment {
    let mut assessment = SemanticTransitionAssessment::default();

    evaluate_identity(before, after, identity_line_shift_tolerance, edited_lines, &mut assessment);
    evaluate_reference_integrity(
        before,
        after,
        semantic_reference_drop_tolerance,
        &mut assessment,
    );
    evaluate_scope_integrity(
        before,
        after,
        semantic_scope_drift_tolerance,
        edited_lines,
        &mut assessment,
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
) {
    let mut missing_ids = Vec::<String>::new();
    let mut missing_lines = BTreeSet::<usize>::new();

    for stable_id in before
        .declaration_stable_ids
        .difference(&after.declaration_stable_ids)
    {
        let declaration_line = before
            .stable_id_decl_lines
            .get(stable_id)
            .copied()
            .unwrap_or(0);
        let declaration_kind = before.declaration_kind_by_stable_id.get(stable_id).copied();
        let before_reference_count = before
            .reference_stable_id_counts
            .get(stable_id)
            .copied()
            .unwrap_or_else(|| {
                before
                    .usr_decl_reference_counts
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
        .symbol_identity_issue_count
        .saturating_sub(before.symbol_identity_issue_count);
    if identity_issue_delta > 0 {
        missing_lines.extend(after.symbol_identity_issue_lines.iter().copied());
    }

    let total_stable_ids = before.declaration_stable_ids.len().max(1);
    let identity_severity_raw = (missing_ids.len() + identity_issue_delta) as f64 / total_stable_ids as f64;
    assessment.identity_severity = identity_severity_raw.clamp(0.0, 1.0);

    if !missing_ids.is_empty() || identity_issue_delta > 0 {
        assessment.identity_integrity_regressed = true;
        let identity_penalty = crate::engine::fuzzy_inference::fuzzy_transition_penalty(0, 0.5);
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
    semantic_reference_drop_tolerance: usize,
    assessment: &mut SemanticTransitionAssessment,
) {
    let mut culprit_lines = BTreeSet::<usize>::new();
    let mut drop_events = 0usize;

    for (stable_id, before_count) in &before.usr_decl_reference_counts {
        let after_count = after
            .usr_decl_reference_counts
            .get(stable_id)
            .copied()
            .unwrap_or(0);
        if *before_count > after_count.saturating_add(semantic_reference_drop_tolerance) {
            drop_events = drop_events.saturating_add(1);
            if let Some(line) = before.usr_decl_lines.get(stable_id) {
                culprit_lines.insert((*line).max(1));
            }
        }
    }

    let mut orphan_lines = BTreeSet::<usize>::new();
    let mut orphan_count = 0usize;
    for (stable_id, after_count) in &after.reference_stable_id_counts {
        if *after_count == 0 || after.declaration_stable_ids.contains(stable_id) {
            continue;
        }
        let before_orphan_count = if before.declaration_stable_ids.contains(stable_id) {
            0
        } else {
            before
                .reference_stable_id_counts
                .get(stable_id)
                .copied()
                .unwrap_or(0)
        };
        if *after_count > before_orphan_count {
            orphan_count =
                orphan_count.saturating_add(after_count.saturating_sub(before_orphan_count));
            if let Some(line) = after.reference_stable_id_first_line.get(stable_id) {
                orphan_lines.insert((*line).max(1));
            }
        }
    }

    let usage_mismatch_delta = after
        .usage_role_mismatch_count
        .saturating_sub(before.usage_role_mismatch_count);
    if usage_mismatch_delta > 0 {
        culprit_lines.extend(after.usage_role_mismatch_lines.iter().copied());
    }

    let total_references = before.usr_decl_reference_counts.len().max(1);
    let ref_severity_raw = (drop_events + usage_mismatch_delta + orphan_count) as f64 / total_references as f64;
    assessment.reference_severity = ref_severity_raw.clamp(0.0, 1.0);

    if drop_events > 0 {
        assessment.reference_integrity_regressed = true;
        let reference_penalty = crate::engine::fuzzy_inference::fuzzy_transition_penalty(1, 0.5);
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
            semantic_reference_drop_tolerance,
            line_hint(culprit_lines.iter().copied(), 8)
        ));
    }

    if usage_mismatch_delta > 0 {
        assessment.reference_integrity_regressed = true;
        let usage_penalty = crate::engine::fuzzy_inference::fuzzy_transition_penalty(2, 0.5);
        assessment.failure_score_delta = assessment
            .failure_score_delta
            .saturating_add(usage_penalty)
            .saturating_add((usage_mismatch_delta.min(16) as u32).saturating_mul(2));
        assessment
            .culprit_lines
            .extend(after.usage_role_mismatch_lines.iter().copied());
        assessment.failure_messages.push(format!(
            "post-edit check failed: semantic usage-role consistency regressed ({} -> {}, lines={})",
            before.usage_role_mismatch_count,
            after.usage_role_mismatch_count,
            line_hint(after.usage_role_mismatch_lines.iter().copied(), 8)
        ));
    }

    if orphan_count > 0 {
        assessment.reference_integrity_regressed = true;
        let orphan_penalty = crate::engine::fuzzy_inference::fuzzy_transition_penalty(3, 0.5);
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
    semantic_scope_drift_tolerance: usize,
    edited_lines: Option<&BTreeSet<usize>>,
    assessment: &mut SemanticTransitionAssessment,
) {
    let count_drift = scope_count_drift(before, after);
    let (local_range_drift, remote_range_drift, range_culprit_lines) =
        scope_range_drift(before, after, edited_lines, 1);

    let total_scopes = (before.scope_counts.namespace + before.scope_counts.type_scope + before.scope_counts.function).max(1);
    let scope_severity_raw = (count_drift + remote_range_drift) as f64 / total_scopes as f64;
    assessment.scope_severity = scope_severity_raw.clamp(0.0, 1.0);

    let regressed = count_drift > semantic_scope_drift_tolerance
        || remote_range_drift > semantic_scope_drift_tolerance;
    if !regressed {
        return;
    }

    assessment.scope_integrity_regressed = true;
    let scope_penalty = crate::engine::fuzzy_inference::fuzzy_transition_penalty(4, 0.5);
    assessment.failure_score_delta = assessment.failure_score_delta.saturating_add(scope_penalty);
    assessment
        .culprit_lines
        .extend(range_culprit_lines.iter().copied());

    assessment.failure_messages.push(format!(
        "post-edit check failed: semantic scope structural drift detected (count_drift={}, local_range_drift={}, remote_range_drift={}, tolerance={})",
        count_drift,
        local_range_drift,
        remote_range_drift,
        semantic_scope_drift_tolerance
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
        if SemanticContract::line_in_ranges(*line, before.preprocessor_ranges.as_slice())
            || SemanticContract::line_in_ranges(*line, after.preprocessor_ranges.as_slice())
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
        .scope_counts
        .namespace
        .abs_diff(after.scope_counts.namespace)
        + before
            .scope_counts
            .type_scope
            .abs_diff(after.scope_counts.type_scope)
        + before
            .scope_counts
            .function
            .abs_diff(after.scope_counts.function)
        + before
            .scope_counts
            .preprocessor
            .abs_diff(after.scope_counts.preprocessor)
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
    for kind in ["namespace", "type", "function", "preprocessor"] {
        let before_ranges = before.scope_ranges_by_kind.get(kind).unwrap_or(&empty);
        let after_ranges = after.scope_ranges_by_kind.get(kind).unwrap_or(&empty);

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
