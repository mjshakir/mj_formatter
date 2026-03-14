use crate::engine::catalog::PolicyCertainty;

use super::{SemanticReadinessAssessment, SemanticReadinessInput};

pub(super) fn evaluate(
    input: SemanticReadinessInput,
    certainty: Option<&PolicyCertainty>,
) -> SemanticReadinessAssessment {
    let mut reasons = Vec::<String>::new();

    if input.tree_unavailable {
        reasons.push("tree-sitter parser unavailable".to_string());
    }
    if input.clang_unavailable {
        reasons.push("clang parser unavailable".to_string());
    }

    // Binary: ready if parsers are available (no hard failures)
    let ready = reasons.is_empty();

    if !ready {
        let score = crate::engine::fuzzy_inference::fuzzy_semantic_readiness(
            input.tree_error_ratio,
            input.clang_error_count,
            input.clang_fatal_count,
            certainty,
        );
        reasons.push(format!("semantic readiness score {score:.3} (parsers unavailable)"));
    }

    SemanticReadinessAssessment { ready, reasons }
}
