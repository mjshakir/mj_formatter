use super::{SemanticContextAssessment, SemanticContractSnapshot};

pub(super) fn evaluate(snapshot: &SemanticContractSnapshot) -> SemanticContextAssessment {
    let mut assessment = SemanticContextAssessment::default();

    if snapshot.issues.identity_count > 0 {
        assessment.hard_failures.push(format!(
            "semantic symbol identity invariants failed: issues={} lines={}",
            snapshot.issues.identity_count,
            line_hint(snapshot.issues.identity_lines.iter().copied(), 8)
        ));
        assessment
            .culprit_lines
            .extend(snapshot.issues.identity_lines.iter().copied());
    }

    if snapshot.issues.mismatch_count > 0 {
        assessment.hard_failures.push(format!(
            "semantic usage-role consistency invariants failed: mismatches={} lines={}",
            snapshot.issues.mismatch_count,
            line_hint(snapshot.issues.mismatch_lines.iter().copied(), 8)
        ));
        assessment
            .culprit_lines
            .extend(snapshot.issues.mismatch_lines.iter().copied());
    }

    if snapshot.summary.declaration_count == 0 && snapshot.summary.reference_count > 0 {
        assessment.warnings.push(format!(
            "semantic context warning: references={} but declarations=0",
            snapshot.summary.reference_count
        ));
    }

    assessment.ready = assessment.hard_failures.is_empty();
    assessment
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
