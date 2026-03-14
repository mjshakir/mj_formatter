use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::gate_config::AccuracyGateConfig;
use crate::parser::manager::SemanticCompdbContextKind;

#[derive(Clone, Copy, Debug, Default)]
pub struct AccuracyGateInput {
    pub semantic_ready: bool,
    pub attempted_edits: usize,
    pub attempted_violations: usize,
    pub accepted_edits: usize,
    pub semantic_context_kind: SemanticCompdbContextKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum AccuracyGateStatus {
    Passed,
    WarningOnly,
    FailedClosed,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub enum AccuracyGateReason {
    SemanticRequiredUnmet,
    PrecisionBelowThreshold { actual: f64, minimum: f64 },
    RecallBelowThreshold { actual: f64, minimum: f64 },
    FailClosedRelaxedForPairedSourceHeuristic,
    FailClosedRelaxedForConsensusContext,
    FailClosedRelaxedForTreeOnlyContext,
}

impl AccuracyGateReason {
    pub fn render(&self) -> String {
        match self {
            Self::SemanticRequiredUnmet => "semantic_required_unmet".to_string(),
            Self::PrecisionBelowThreshold { actual, minimum } => {
                format!("precision_below_threshold({actual:.3} < {minimum:.3})")
            }
            Self::RecallBelowThreshold { actual, minimum } => {
                format!("recall_below_threshold({actual:.3} < {minimum:.3})")
            }
            Self::FailClosedRelaxedForPairedSourceHeuristic => {
                "fail_closed_relaxed_for_paired_source_heuristic".to_string()
            }
            Self::FailClosedRelaxedForConsensusContext => {
                "fail_closed_relaxed_for_consensus_context".to_string()
            }
            Self::FailClosedRelaxedForTreeOnlyContext => {
                "fail_closed_relaxed_for_tree_only_context".to_string()
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct AccuracyGateDecision {
    pub status: AccuracyGateStatus,
    pub precision: f64,
    pub recall: f64,
    pub reasons: Vec<AccuracyGateReason>,
}

impl AccuracyGateDecision {
    pub fn passed(&self) -> bool {
        self.status == AccuracyGateStatus::Passed
    }

    pub fn render_reasons(&self) -> Vec<String> {
        self.reasons
            .iter()
            .map(AccuracyGateReason::render)
            .collect()
    }

    pub fn rendered_reason_summary(&self) -> String {
        let rendered = self.render_reasons();
        if rendered.is_empty() {
            "unknown".to_string()
        } else {
            rendered.join(",")
        }
    }

    pub fn summary(&self) -> String {
        format!(
            "accuracy_gate: precision={:.3} recall={:.3} reasons=[{}]",
            self.precision,
            self.recall,
            self.rendered_reason_summary()
        )
    }
}

#[derive(Clone, Debug)]
pub enum AccuracyGateFailureKind {
    SemanticRequiredUnmet { detail: String },
    ThresholdMiss,
}

#[derive(Clone, Debug)]
pub struct AccuracyGateFailure {
    path: PathBuf,
    decision: AccuracyGateDecision,
    kind: AccuracyGateFailureKind,
}

impl AccuracyGateFailure {
    pub fn semantic_required_unmet(
        path: &Path,
        detail: impl Into<String>,
        decision: AccuracyGateDecision,
    ) -> Self {
        Self {
            path: path.to_path_buf(),
            decision,
            kind: AccuracyGateFailureKind::SemanticRequiredUnmet {
                detail: detail.into(),
            },
        }
    }

    pub fn threshold_miss(path: &Path, decision: AccuracyGateDecision) -> Self {
        Self {
            path: path.to_path_buf(),
            decision,
            kind: AccuracyGateFailureKind::ThresholdMiss,
        }
    }

    pub fn decision(&self) -> &AccuracyGateDecision {
        &self.decision
    }
}

impl fmt::Display for AccuracyGateFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            AccuracyGateFailureKind::SemanticRequiredUnmet { detail } => write!(
                f,
                "accuracy gate fail-closed: semantic_required unmet for {} ({detail})",
                self.path.display()
            ),
            AccuracyGateFailureKind::ThresholdMiss => {
                write!(
                    f,
                    "accuracy gate fail-closed for {}: {}",
                    self.path.display(),
                    self.decision.summary()
                )
            }
        }
    }
}

impl std::error::Error for AccuracyGateFailure {}

pub struct AccuracyGate;

impl AccuracyGate {
    pub fn evaluate(config: &AccuracyGateConfig, input: AccuracyGateInput) -> AccuracyGateDecision {
        let precision = if input.attempted_edits == 0 {
            1.0
        } else {
            (input.accepted_edits as f64 / input.attempted_edits as f64).clamp(0.0, 1.0)
        };
        let recall_denominator = input.attempted_violations.min(input.attempted_edits.max(1));
        let recall = if input.attempted_violations == 0 || input.attempted_edits == 0 {
            1.0
        } else {
            (input.accepted_edits as f64 / recall_denominator as f64).clamp(0.0, 1.0)
        };

        let mut reasons = Vec::<AccuracyGateReason>::new();
        if config.semantic_required && !input.semantic_ready {
            reasons.push(AccuracyGateReason::SemanticRequiredUnmet);
        }

        let sample_size = input
            .attempted_edits
            .saturating_add(input.attempted_violations);
        let threshold_checks_enabled = config.enabled && sample_size >= config.min_samples;
        if threshold_checks_enabled {
            if precision + f64::EPSILON < config.min_precision {
                reasons.push(AccuracyGateReason::PrecisionBelowThreshold {
                    actual: precision,
                    minimum: config.min_precision,
                });
            }
            if recall + f64::EPSILON < config.min_recall {
                reasons.push(AccuracyGateReason::RecallBelowThreshold {
                    actual: recall,
                    minimum: config.min_recall,
                });
            }
        }

        let status = if reasons.is_empty() {
            AccuracyGateStatus::Passed
        } else if config.fail_closed {
            if let Some(reason) =
                Self::fail_closed_relaxation_reason(input.semantic_context_kind, reasons.as_slice())
            {
                reasons.push(reason);
                AccuracyGateStatus::WarningOnly
            } else {
                AccuracyGateStatus::FailedClosed
            }
        } else {
            AccuracyGateStatus::WarningOnly
        };

        AccuracyGateDecision {
            status,
            precision,
            recall,
            reasons,
        }
    }

    fn fail_closed_relaxation_reason(
        context_kind: SemanticCompdbContextKind,
        reasons: &[AccuracyGateReason],
    ) -> Option<AccuracyGateReason> {
        if reasons.is_empty() {
            return None;
        }
        let semantic_required_unmet = reasons
            .iter()
            .any(|reason| matches!(reason, AccuracyGateReason::SemanticRequiredUnmet));
        let threshold_miss_count = reasons
            .iter()
            .filter(|reason| {
                matches!(
                    reason,
                    AccuracyGateReason::PrecisionBelowThreshold { .. }
                        | AccuracyGateReason::RecallBelowThreshold { .. }
                )
            })
            .count();
        match context_kind {
            SemanticCompdbContextKind::Exact => None,
            SemanticCompdbContextKind::PairedSourceHeuristic => {
                if !semantic_required_unmet && threshold_miss_count >= 2 {
                    None
                } else {
                    Some(AccuracyGateReason::FailClosedRelaxedForPairedSourceHeuristic)
                }
            }
            SemanticCompdbContextKind::HeaderConsensus
            | SemanticCompdbContextKind::SourceConsensus => {
                Some(AccuracyGateReason::FailClosedRelaxedForConsensusContext)
            }
            SemanticCompdbContextKind::None => {
                Some(AccuracyGateReason::FailClosedRelaxedForTreeOnlyContext)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::gate_config::AccuracyGateConfig;
    use crate::engine::accuracy_gate::AccuracyGateReason;
    use crate::engine::accuracy_gate::{AccuracyGate, AccuracyGateInput, AccuracyGateStatus};
    use crate::parser::manager::SemanticCompdbContextKind;

    #[test]
    fn passes_when_thresholds_are_met() {
        let config = AccuracyGateConfig {
            enabled: true,
            min_precision: 0.5,
            min_recall: 0.2,
            min_samples: 1,
            ..AccuracyGateConfig::default()
        };
        let decision = AccuracyGate::evaluate(
            &config,
            AccuracyGateInput {
                semantic_ready: true,
                attempted_edits: 4,
                attempted_violations: 6,
                accepted_edits: 4,
                semantic_context_kind: SemanticCompdbContextKind::Exact,
            },
        );
        assert_eq!(decision.status, AccuracyGateStatus::Passed);
        assert!(decision.reasons.is_empty());
    }

    #[test]
    fn warns_when_semantic_required_is_not_met_fail_open() {
        let config = AccuracyGateConfig {
            semantic_required: true,
            fail_closed: false,
            ..AccuracyGateConfig::default()
        };
        let decision = AccuracyGate::evaluate(
            &config,
            AccuracyGateInput {
                semantic_ready: false,
                attempted_edits: 0,
                attempted_violations: 0,
                accepted_edits: 0,
                semantic_context_kind: SemanticCompdbContextKind::Exact,
            },
        );
        assert_eq!(decision.status, AccuracyGateStatus::WarningOnly);
        assert!(decision
            .reasons
            .contains(&AccuracyGateReason::SemanticRequiredUnmet));
    }

    #[test]
    fn fail_closed_blocks_on_threshold_miss() {
        let config = AccuracyGateConfig {
            enabled: true,
            fail_closed: true,
            min_precision: 0.90,
            min_recall: 0.90,
            min_samples: 1,
            ..AccuracyGateConfig::default()
        };
        let decision = AccuracyGate::evaluate(
            &config,
            AccuracyGateInput {
                semantic_ready: true,
                attempted_edits: 10,
                attempted_violations: 10,
                accepted_edits: 2,
                semantic_context_kind: SemanticCompdbContextKind::Exact,
            },
        );
        assert_eq!(decision.status, AccuracyGateStatus::FailedClosed);
        assert_eq!(decision.precision, 0.2);
        assert_eq!(decision.recall, 0.2);
    }

    #[test]
    fn grouped_edits_do_not_artificially_deflate_recall() {
        let config = AccuracyGateConfig {
            enabled: true,
            fail_closed: true,
            min_precision: 0.90,
            min_recall: 0.90,
            min_samples: 1,
            ..AccuracyGateConfig::default()
        };
        let decision = AccuracyGate::evaluate(
            &config,
            AccuracyGateInput {
                semantic_ready: true,
                attempted_edits: 1,
                attempted_violations: 3,
                accepted_edits: 1,
                semantic_context_kind: SemanticCompdbContextKind::Exact,
            },
        );
        assert_eq!(decision.status, AccuracyGateStatus::Passed);
        assert_eq!(decision.precision, 1.0);
        assert_eq!(decision.recall, 1.0);
    }

    #[test]
    fn zero_edit_samples_do_not_fail_recall_threshold() {
        let config = AccuracyGateConfig {
            enabled: true,
            fail_closed: true,
            min_precision: 0.90,
            min_recall: 0.90,
            min_samples: 1,
            ..AccuracyGateConfig::default()
        };
        let decision = AccuracyGate::evaluate(
            &config,
            AccuracyGateInput {
                semantic_ready: true,
                attempted_edits: 0,
                attempted_violations: 3,
                accepted_edits: 0,
                semantic_context_kind: SemanticCompdbContextKind::Exact,
            },
        );
        assert_eq!(decision.status, AccuracyGateStatus::Passed);
        assert_eq!(decision.precision, 1.0);
        assert_eq!(decision.recall, 1.0);
    }

    #[test]
    fn renders_reason_enums_at_boundary() {
        let decision = crate::engine::accuracy_gate::AccuracyGateDecision {
            status: AccuracyGateStatus::WarningOnly,
            precision: 0.923,
            recall: 0.811,
            reasons: vec![
                AccuracyGateReason::SemanticRequiredUnmet,
                AccuracyGateReason::PrecisionBelowThreshold {
                    actual: 0.923,
                    minimum: 0.940,
                },
            ],
        };
        assert_eq!(
            decision.summary(),
            "accuracy_gate: precision=0.923 recall=0.811 reasons=[semantic_required_unmet,precision_below_threshold(0.923 < 0.940)]"
        );
    }

    #[test]
    fn serializes_accuracy_gate_reasons() {
        let reason = AccuracyGateReason::RecallBelowThreshold {
            actual: 0.250,
            minimum: 0.600,
        };
        let json = serde_json::to_string(&reason).expect("serialize reason");
        let restored: AccuracyGateReason =
            serde_json::from_str(json.as_str()).expect("deserialize reason");
        assert_eq!(restored, reason);
    }

    #[test]
    fn fail_closed_relaxes_for_consensus_contexts() {
        let config = AccuracyGateConfig {
            enabled: true,
            fail_closed: true,
            min_precision: 0.95,
            min_recall: 0.95,
            min_samples: 1,
            ..AccuracyGateConfig::default()
        };
        let decision = AccuracyGate::evaluate(
            &config,
            AccuracyGateInput {
                semantic_ready: true,
                attempted_edits: 10,
                attempted_violations: 10,
                accepted_edits: 2,
                semantic_context_kind: SemanticCompdbContextKind::HeaderConsensus,
            },
        );
        assert_eq!(decision.status, AccuracyGateStatus::WarningOnly);
        assert!(decision
            .reasons
            .contains(&AccuracyGateReason::FailClosedRelaxedForConsensusContext));
    }

    #[test]
    fn paired_source_heuristic_still_fail_closes_on_strong_threshold_miss() {
        let config = AccuracyGateConfig {
            enabled: true,
            fail_closed: true,
            min_precision: 0.95,
            min_recall: 0.95,
            min_samples: 1,
            ..AccuracyGateConfig::default()
        };
        let decision = AccuracyGate::evaluate(
            &config,
            AccuracyGateInput {
                semantic_ready: true,
                attempted_edits: 10,
                attempted_violations: 10,
                accepted_edits: 2,
                semantic_context_kind: SemanticCompdbContextKind::PairedSourceHeuristic,
            },
        );
        assert_eq!(decision.status, AccuracyGateStatus::FailedClosed);
        assert!(!decision
            .reasons
            .contains(&AccuracyGateReason::FailClosedRelaxedForPairedSourceHeuristic));
    }

    #[test]
    fn paired_source_heuristic_relaxes_semantic_required_unmet() {
        let config = AccuracyGateConfig {
            semantic_required: true,
            fail_closed: true,
            ..AccuracyGateConfig::default()
        };
        let decision = AccuracyGate::evaluate(
            &config,
            AccuracyGateInput {
                semantic_ready: false,
                attempted_edits: 0,
                attempted_violations: 0,
                accepted_edits: 0,
                semantic_context_kind: SemanticCompdbContextKind::PairedSourceHeuristic,
            },
        );
        assert_eq!(decision.status, AccuracyGateStatus::WarningOnly);
        assert!(decision
            .reasons
            .contains(&AccuracyGateReason::FailClosedRelaxedForPairedSourceHeuristic));
    }
}
