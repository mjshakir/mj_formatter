use super::{SemanticReadinessAssessment, SemanticReadinessInput};

pub(super) fn evaluate(
    input: SemanticReadinessInput,
) -> SemanticReadinessAssessment {
    let mut reasons = Vec::<String>::new();

    if input.tree_unavailable {
        reasons.push("tree-sitter parser unavailable".to_string());
    }
    if input.clang_unavailable {
        reasons.push("clang parser unavailable".to_string());
    }

    let ready = reasons.is_empty();

    if !ready {
        reasons.push("semantic readiness: parsers unavailable".to_string());
    }

    SemanticReadinessAssessment { ready, reasons }
}
