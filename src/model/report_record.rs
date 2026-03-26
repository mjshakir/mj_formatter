use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::engine::edit_candidate::PolicyDecisionOutcome;
use crate::model::file_result::FileResult;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReportRecord {
    pub path: PathBuf,
    pub changed: bool,
    pub error: Option<String>,
    pub warnings: Vec<String>,
    pub elapsed_engine_ms: f64,
    pub elapsed_total_ms: f64,
    pub boot_parse_ms: f64,
    pub policies: Vec<PolicyReport>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PolicyReport {
    pub policy: String,
    pub outcome: PolicyOutcome,
    pub reason: Option<String>,
    pub elapsed_ms: f64,
    #[serde(default)]
    pub parse_ms: f64,
    #[serde(default)]
    pub execute_ms: f64,
    #[serde(default)]
    pub checkpoint_ms: f64,
    pub edits: Vec<EditReport>,
    pub blocked_lines: Vec<BlockedLineReport>,
    pub confidence_score: Option<f64>,
    pub confidence_threshold: Option<f64>,
    pub parse_mode: String,
    pub candidate_count: usize,
    pub dropped_count: usize,
    pub semantic_impact_radius: usize,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub enum PolicyOutcome {
    Applied,
    PartiallyApplied,
    Blocked,
    NoChange,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EditReport {
    pub line: usize,
    pub before: String,
    pub after: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlockedLineReport {
    pub line: usize,
    pub reason: String,
}

impl From<&FileResult> for ReportRecord {
    fn from(result: &FileResult) -> Self {
        use crate::model::exec_trace::PolicyCandidateOutcome;

        let mut policies = Vec::with_capacity(result.traces.len());
        for trace in &result.traces {
            let policy_name = trace.policy.as_str().to_string();
            let edits: Vec<EditReport> = result
                .outcome
                .edits
                .iter()
                .filter(|edit| edit.policy.as_str() == trace.policy.as_str())
                .map(|edit| EditReport {
                    line: edit.line,
                    before: edit.before.clone(),
                    after: edit.after.clone(),
                })
                .collect();

            let blocked_lines: Vec<BlockedLineReport> = trace
                .candidate_trace
                .iter()
                .filter(|ct| ct.outcome != PolicyCandidateOutcome::Selected)
                .map(|ct| BlockedLineReport {
                    line: ct.line,
                    reason: ct.outcome.as_str().to_string(),
                })
                .collect();

            let mut violations_for_policy: Vec<BlockedLineReport> = result
                .outcome
                .violations
                .iter()
                .filter(|v| v.policy.as_str() == trace.policy.as_str())
                .map(|v| BlockedLineReport {
                    line: v.line,
                    reason: v.message.clone(),
                })
                .collect();
            let mut all_blocked = blocked_lines;
            all_blocked.append(&mut violations_for_policy);

            let outcome = derive_outcome(trace.confidence_outcome, &edits, &all_blocked);
            let reason = derive_reason(trace.confidence_outcome, trace.confidence_score, trace.confidence_threshold, &all_blocked);

            policies.push(PolicyReport {
                policy: policy_name,
                outcome,
                reason,
                elapsed_ms: trace.elapsed_ms,
                parse_ms: trace.parse_ms,
                execute_ms: trace.execute_ms,
                checkpoint_ms: trace.checkpoint_ms,
                edits,
                blocked_lines: all_blocked,
                confidence_score: trace.confidence_score,
                confidence_threshold: trace.confidence_threshold,
                parse_mode: trace.parse_mode.clone(),
                candidate_count: trace.candidate_line_count,
                dropped_count: trace.dropped_line_count,
                semantic_impact_radius: trace.semantic_impact_radius,
            });
        }

        Self {
            path: result.meta.path.clone(),
            changed: result.outcome.changed,
            error: result.error.clone(),
            warnings: result.warnings.clone(),
            elapsed_engine_ms: result.meta.engine_ms,
            elapsed_total_ms: result.meta.total_ms,
            boot_parse_ms: result.meta.boot_parse_ms,
            policies,
        }
    }
}

fn derive_outcome(
    confidence_outcome: Option<PolicyDecisionOutcome>,
    edits: &[EditReport],
    blocked: &[BlockedLineReport],
) -> PolicyOutcome {
    if let Some(outcome) = confidence_outcome {
        return match outcome {
            PolicyDecisionOutcome::Apply => PolicyOutcome::Applied,
            PolicyDecisionOutcome::ApplyPartial => PolicyOutcome::PartiallyApplied,
            PolicyDecisionOutcome::Block => PolicyOutcome::Blocked,
        };
    }
    if edits.is_empty() && blocked.is_empty() {
        return PolicyOutcome::NoChange;
    }
    if edits.is_empty() {
        return PolicyOutcome::Blocked;
    }
    if !blocked.is_empty() {
        return PolicyOutcome::PartiallyApplied;
    }
    PolicyOutcome::Applied
}

fn derive_reason(
    confidence_outcome: Option<PolicyDecisionOutcome>,
    score: Option<f64>,
    threshold: Option<f64>,
    blocked: &[BlockedLineReport],
) -> Option<String> {
    if let Some(PolicyDecisionOutcome::Block) = confidence_outcome {
        return Some(format!(
            "blocked by confidence gate (score={:.2}, threshold={:.2})",
            score.unwrap_or(0.0),
            threshold.unwrap_or(0.0)
        ));
    }
    if !blocked.is_empty() {
        return Some(blocked.first().map(|b| b.reason.clone()).unwrap_or_default());
    }
    None
}
