use crate::config::enums::Enforcement;
use crate::engine::gate_decision::{ConfidenceGateDecision, ConfidenceReasonCode};
use crate::engine::edit_candidate::PolicyDecisionOutcome;
use crate::model::policy_result::PolicyResult;
use crate::model::violation::Violation;
use crate::runtime::cluster_telemetry::ClusterEnforcementBias;

use super::PolicyPipeline;

impl PolicyPipeline {
    pub(super) fn apply_confidence_decision(
        policy_name: &str,
        before_text: &str,
        result: PolicyResult,
        decision: ConfidenceGateDecision,
    ) -> PolicyResult {
        let message_line = result.edits.first().map(|item| item.line).unwrap_or(1);
        let reason_text = decision.rendered_reason_summary();

        match decision.outcome {
            PolicyDecisionOutcome::Apply => {
                if decision.base_enforcement != decision.effective_enforcement {
                    let mut violations = result.violations;
                    violations.push(Violation {
                        policy: "confidence_guard".into(),
                        message: format!(
                            "Adaptive tier for '{}': {:?}->{:?} ({})",
                            policy_name,
                            decision.base_enforcement,
                            decision.effective_enforcement,
                            reason_text
                        ),
                        line: message_line,
                        column: Some(1),
                    });
                    PolicyResult {
                        text: result.text,
                        violations,
                        edits: result.edits,
                        warnings: result.warnings,
                        changed: result.changed,
                    }
                } else {
                    result
                }
            }
            PolicyDecisionOutcome::ApplyPartial => {
                let dropped_count = decision.dropped_lines.len();
                let mut suppressed =
                    Self::apply_line_suppression(before_text, result, &decision.dropped_lines);
                suppressed.violations.push(Violation {
                    policy: "confidence_guard".into(),
                    message: format!(
                        "Adaptive partial apply for '{}' (dropped_lines={}, score={:.2}, threshold={:.2}, reasons={})",
                        policy_name, dropped_count, decision.score, decision.threshold, reason_text
                    ),
                    line: message_line,
                    column: Some(1),
                });
                suppressed
            }
            PolicyDecisionOutcome::Block => {
                let mode_label = "blocked";
                let mut violations = result.violations;
                violations.push(Violation {
                    policy: "confidence_guard".into(),
                    message: format!(
                        "Adaptive decision {} for '{}' (score={:.2}, threshold={:.2}, effective={:?}, reasons={})",
                        mode_label,
                        policy_name,
                        decision.score,
                        decision.threshold,
                        decision.effective_enforcement,
                        reason_text
                    ),
                    line: message_line,
                    column: Some(1),
                });
                PolicyResult {
                    text: before_text.to_string(),
                    violations,
                    edits: Vec::new(),
                    warnings: result.warnings,
                    changed: false,
                }
            }
        }
    }

    pub(super) fn apply_cluster_bias(
        mut decision: ConfidenceGateDecision,
        bias: ClusterEnforcementBias,
    ) -> ConfidenceGateDecision {
        match bias {
            ClusterEnforcementBias::Neutral => decision,
            ClusterEnforcementBias::Relax => {
                if decision.base_enforcement != Enforcement::Must
                    && decision.outcome == PolicyDecisionOutcome::Block
                {
                    decision.outcome = PolicyDecisionOutcome::Apply;
                    Self::push_reason_code(
                        &mut decision.reason_codes,
                        ConfidenceReasonCode::ClusterAdaptiveRelaxed,
                    );
                }
                if decision.effective_enforcement != Enforcement::Must {
                    decision.effective_enforcement =
                        Self::relax_enforcement(decision.effective_enforcement);
                }
                Self::push_reason_code(
                    &mut decision.reason_codes,
                    ConfidenceReasonCode::ClusterAdaptiveRelaxed,
                );
                decision
            }
            ClusterEnforcementBias::Harden => {
                decision.effective_enforcement =
                    Self::harden_enforcement(decision.effective_enforcement);
                Self::push_reason_code(
                    &mut decision.reason_codes,
                    ConfidenceReasonCode::ClusterAdaptiveHardened,
                );
                decision
            }
        }
    }

    pub(super) fn push_reason_code(codes: &mut Vec<ConfidenceReasonCode>, code: ConfidenceReasonCode) {
        if !codes.contains(&code) {
            codes.push(code);
        }
    }

    pub(super) fn relax_enforcement(value: Enforcement) -> Enforcement {
        match value {
            Enforcement::Must => Enforcement::Must,
            Enforcement::Hard => Enforcement::Soft,
            Enforcement::Soft => Enforcement::Advisory,
            Enforcement::Advisory => Enforcement::Advisory,
        }
    }

    pub(super) fn harden_enforcement(value: Enforcement) -> Enforcement {
        match value {
            Enforcement::Must => Enforcement::Must,
            Enforcement::Hard => Enforcement::Must,
            Enforcement::Soft => Enforcement::Hard,
            Enforcement::Advisory => Enforcement::Soft,
        }
    }
}
