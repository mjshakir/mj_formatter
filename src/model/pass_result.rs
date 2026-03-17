use std::collections::BTreeMap;

use crate::engine::accuracy_gate::AccuracyGateDecision;
use crate::engine::catalog::PolicyCertainty;
use crate::model::exec_trace::PolicyExecutionTrace;
use crate::model::policy_result::PolicyResult;

#[derive(Clone, Debug, Default)]
pub struct FormatPassMetrics {
    pub retry_attempts: u32,
    pub post_edit_validations: u32,
    pub salvage_candidate_checks: u32,
}

impl FormatPassMetrics {
    pub fn retry_effort_units(&self) -> u64 {
        (self.retry_attempts as u64)
            .saturating_mul(8)
            .saturating_add((self.post_edit_validations as u64).saturating_mul(4))
            .saturating_add(self.salvage_candidate_checks as u64)
    }
}

#[derive(Clone, Debug, Default)]
pub struct FormatPassResult {
    pub policy_result: PolicyResult,
    pub convergence_pairs: BTreeMap<(String, String), usize>,
    pub policy_traces: Vec<PolicyExecutionTrace>,
    pub accuracy_gate: Option<AccuracyGateDecision>,
    pub metrics: FormatPassMetrics,
    pub policy_certainty: Option<PolicyCertainty>,
    pub rollback_count: usize,
}
