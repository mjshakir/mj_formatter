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
    pub certainty: Option<FileCertaintyReport>,
    pub policies: Vec<PolicyReport>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileCertaintyReport {
    pub structural: f64,
    pub semantic: f64,
    pub coverage: f64,
    pub richness: f64,
    pub edit_success: f64,
    pub structural_variance: f64,
    pub semantic_variance: f64,
    pub coverage_variance: f64,
    pub richness_variance: f64,
    pub edit_success_variance: f64,
    pub model_prob_stable: f64,
    pub model_prob_transitional: f64,
    pub model_prob_noisy: f64,
    pub trust_semantic_rewrite: f64,
    pub trust_structural: f64,
    pub trust_general: f64,
    pub observation_count: u32,
    pub raw_observation: Option<[f64; 5]>,
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
        use crate::engine::fuzzy_inference;
        use crate::model::exec_trace::PolicyCandidateOutcome;

        let certainty = result.policy_certainty.as_ref().map(|cert| {
            let trust_semantic_rewrite = fuzzy_inference::fuzzy_trust_semantic_rewrite(cert);
            let trust_structural = fuzzy_inference::fuzzy_trust_structural(cert);
            let trust_general = fuzzy_inference::fuzzy_trust_general(cert);
            let (model_prob_transitional, model_prob_noisy) =
                derive_model_probs(cert.stable_model_prob);

            FileCertaintyReport {
                structural: cert.structural,
                semantic: cert.semantic,
                coverage: cert.coverage,
                richness: cert.richness,
                edit_success: cert.edit_success,
                structural_variance: cert.structural_variance,
                semantic_variance: cert.semantic_variance,
                coverage_variance: cert.coverage_variance,
                richness_variance: cert.richness_variance,
                edit_success_variance: cert.edit_success_variance,
                model_prob_stable: cert.stable_model_prob,
                model_prob_transitional,
                model_prob_noisy,
                trust_semantic_rewrite,
                trust_structural,
                trust_general,
                observation_count: cert.observation_count,
                raw_observation: cert.raw_observation,
            }
        });

        let mut policies = Vec::with_capacity(result.policy_traces.len());
        for trace in &result.policy_traces {
            let policy_name = trace.policy.as_str().to_string();
            let edits: Vec<EditReport> = result
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
            path: result.path.clone(),
            changed: result.changed,
            error: result.error.clone(),
            warnings: result.warnings.clone(),
            elapsed_engine_ms: result.elapsed_engine_ms,
            elapsed_total_ms: result.elapsed_total_ms,
            boot_parse_ms: result.boot_parse_ms,
            certainty,
            policies,
        }
    }
}

fn derive_model_probs(stable_prob: f64) -> (f64, f64) {
    let remaining = (1.0 - stable_prob).max(0.0);
    (remaining * 0.6, remaining * 0.4)
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
