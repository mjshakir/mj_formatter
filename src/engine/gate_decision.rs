use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::config::enums::Enforcement;
use crate::engine::edit_candidate::PolicyDecisionOutcome;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Deserialize, Serialize)]
pub enum ConfidenceReasonCode {
    TierAdjusted { from: Enforcement, to: Enforcement },
    LowConfidence,
    LowConsensus,
    ParserConsensusStrict,
    #[serde(alias = "ParserConsensusAdaptiveHardened")]
    ParserHardened,
    #[serde(alias = "ParserConsensusAdaptiveRelaxed")]
    ParserRelaxed,
    SemanticConsensusLow,
    ProjectEvidenceLow,
    ContextCoverageLow,
    #[serde(alias = "SemanticEvidenceMissing")]
    SemanticMissing,
    ParserUnavailable,
    ParserDisagreement,
    ClangDiagnostics,
    #[serde(alias = "RecoverableHybridContext")]
    HybridRecovery,
    RetrySafetyHarden,
    ClusterAdaptiveRelaxed,
    ClusterAdaptiveHardened,
    Stable,
}

impl ConfidenceReasonCode {
    pub fn render(&self) -> String {
        match self {
            Self::TierAdjusted { from, to } => {
                format!(
                    "tier_adjusted:{}->{}",
                    Self::enforcement_label(*from),
                    Self::enforcement_label(*to)
                )
            }
            Self::LowConfidence => "low_confidence".to_string(),
            Self::LowConsensus => "low_consensus".to_string(),
            Self::ParserConsensusStrict => "parser_consensus_strict".to_string(),
            Self::ParserHardened => "parser_hardened".to_string(),
            Self::ParserRelaxed => "parser_relaxed".to_string(),
            Self::SemanticConsensusLow => "semantic_consensus_low".to_string(),
            Self::ProjectEvidenceLow => "project_evidence_low".to_string(),
            Self::ContextCoverageLow => "context_coverage_low".to_string(),
            Self::SemanticMissing => "semantic_missing".to_string(),
            Self::ParserUnavailable => "parser_unavailable".to_string(),
            Self::ParserDisagreement => "parser_disagreement".to_string(),
            Self::ClangDiagnostics => "clang_diagnostics".to_string(),
            Self::HybridRecovery => "hybrid_recovery".to_string(),
            Self::RetrySafetyHarden => "retry_safety_harden".to_string(),
            Self::ClusterAdaptiveRelaxed => "cluster_adaptive_relaxed".to_string(),
            Self::ClusterAdaptiveHardened => "cluster_adaptive_hardened".to_string(),
            Self::Stable => "stable".to_string(),
        }
    }

    fn enforcement_label(value: Enforcement) -> &'static str {
        match value {
            Enforcement::Must => "must",
            Enforcement::Hard => "hard",
            Enforcement::Soft => "soft",
            Enforcement::Advisory => "advisory",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ConfidenceGateDecision {
    pub outcome: PolicyDecisionOutcome,
    pub score: f64,
    pub threshold: f64,
    pub base_enforcement: Enforcement,
    pub effective_enforcement: Enforcement,
    pub reason_codes: Vec<ConfidenceReasonCode>,
    pub dropped_lines: BTreeSet<usize>,
}

impl ConfidenceGateDecision {
    pub fn render_reason_codes(&self) -> Vec<String> {
        self.reason_codes
            .iter()
            .map(ConfidenceReasonCode::render)
            .collect()
    }

    pub fn rendered_reason_summary(&self) -> String {
        let rendered = self.render_reason_codes();
        if rendered.is_empty() {
            "stable".to_string()
        } else {
            rendered.join(",")
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::enums::Enforcement;
    use crate::engine::gate_decision::ConfidenceReasonCode;

    #[test]
    fn renders_tier_adjustment() {
        let reason = ConfidenceReasonCode::TierAdjusted {
            from: Enforcement::Hard,
            to: Enforcement::Soft,
        };
        assert_eq!(reason.render(), "tier_adjusted:hard->soft");
    }

    #[test]
    fn serializes_reason_codes() {
        let reason = ConfidenceReasonCode::ParserHardened;
        let json = serde_json::to_string(&reason).expect("serialize confidence reason");
        let restored: ConfidenceReasonCode =
            serde_json::from_str(json.as_str()).expect("deserialize confidence reason");
        assert_eq!(restored, reason);
    }
}
