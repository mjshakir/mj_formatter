use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::engine::accuracy_gate::AccuracyGateDecision;
use crate::engine::catalog::PolicyCertainty;
use crate::model::edit::Edit;
use crate::model::exec_trace::PolicyExecutionTrace;
use crate::model::rename_plan::SemanticRenamePlan;
use crate::model::violation::Violation;

#[derive(Clone, Debug, Default)]
pub struct FileResult {
    pub path: PathBuf,
    pub changed: bool,
    pub pending_text: Option<String>,
    pub semantic_rename_plans: Vec<SemanticRenamePlan>,
    pub convergence_pairs: BTreeMap<(String, String), usize>,
    pub violations: Vec<Violation>,
    pub edits: Vec<Edit>,
    pub policy_traces: Vec<PolicyExecutionTrace>,
    pub accuracy_gate: Option<AccuracyGateDecision>,
    pub error: Option<String>,
    pub backup_path: Option<PathBuf>,
    pub warnings: Vec<String>,
    pub elapsed_engine_ms: f64,
    pub elapsed_total_ms: f64,
    pub policy_certainty: Option<PolicyCertainty>,
}
