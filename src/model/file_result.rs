use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::engine::accuracy_gate::AccuracyGateDecision;
use crate::engine::catalog::PolicyCertainty;
use crate::model::edit::Edit;
use crate::model::exec_trace::PolicyExecutionTrace;
use crate::model::rename_plan::SemanticRenamePlan;
use crate::model::violation::Violation;

#[derive(Clone, Debug, Default)]
pub struct FileMeta {
    pub path: PathBuf,
    pub backup_path: Option<PathBuf>,
    pub engine_ms: f64,
    pub total_ms: f64,
    pub boot_parse_ms: f64,
}

#[derive(Clone, Debug, Default)]
pub struct FormatOutcome {
    pub changed: bool,
    pub pending_text: Option<String>,
    pub rename_plans: Vec<SemanticRenamePlan>,
    pub convergence_pairs: BTreeMap<(String, String), usize>,
    pub violations: Vec<Violation>,
    pub edits: Vec<Edit>,
    pub accuracy_gate: Option<AccuracyGateDecision>,
    pub certainty: Option<PolicyCertainty>,
}

#[derive(Clone, Debug, Default)]
pub struct FileResult {
    pub meta: FileMeta,
    pub outcome: FormatOutcome,
    pub traces: Vec<PolicyExecutionTrace>,
    pub error: Option<String>,
    pub warnings: Vec<String>,
}
