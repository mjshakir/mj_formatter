use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::engine::edit_candidate::PolicyDecisionOutcome;
use crate::engine::edit_candidate::CandidateRiskTier;
use crate::engine::run_options::RetryScopeStage;
use crate::engine::zone::PolicyZone;
use crate::engine::semantic_contract::SemanticInvariantClause;
use crate::model::policy_name::PolicyName;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum PolicyCandidateOutcome {
    BlockedHardConstraint,
    BlockedZone,
    DroppedConflict,
    #[default]
    DroppedConvergence,
    Selected,
}

impl PolicyCandidateOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BlockedHardConstraint => "blocked_hard_constraint",
            Self::BlockedZone => "blocked_zone",
            Self::DroppedConflict => "dropped_conflict",
            Self::DroppedConvergence => "dropped_convergence",
            Self::Selected => "selected",
        }
    }

    fn from_serialized(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "blocked_hard_constraint" => Some(Self::BlockedHardConstraint),
            "blocked_zone" => Some(Self::BlockedZone),
            "dropped_conflict" => Some(Self::DroppedConflict),
            "dropped_convergence" => Some(Self::DroppedConvergence),
            "selected" => Some(Self::Selected),
            _ => None,
        }
    }
}

impl Serialize for PolicyCandidateOutcome {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PolicyCandidateOutcome {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_serialized(value.as_str())
            .ok_or_else(|| serde::de::Error::custom(format!("unknown candidate outcome '{value}'")))
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PolicyCandidateTrace {
    pub line: usize,
    pub confidence: f64,
    pub style_gain: f64,
    pub utility: f64,
    pub risk_tier: CandidateRiskTier,
    pub impact_radius: usize,
    pub symbol_footprint_count: usize,
    pub range_footprint_count: usize,
    pub hard_constraints_touched: Vec<SemanticInvariantClause>,
    pub zone: PolicyZone,
    pub outcome: PolicyCandidateOutcome,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PolicyExecutionTrace {
    pub policy: PolicyName,
    /// "hybrid" for semantic-rewrite policies, "tree-sitter" for syntactic policies.
    pub parse_mode: String,
    pub context_cluster: u64,
    pub candidate_line_count: usize,
    pub dropped_line_count: usize,
    pub semantic_impact_radius: usize,
    pub confidence_outcome: Option<PolicyDecisionOutcome>,
    pub confidence_score: Option<f64>,
    pub confidence_threshold: Option<f64>,
    pub executor_scope: RetryScopeStage,
    #[serde(default)]
    pub elapsed_ms: f64,
    #[serde(default)]
    pub candidate_trace: Vec<PolicyCandidateTrace>,
}

#[cfg(test)]
mod tests {
    use crate::engine::edit_candidate::PolicyDecisionOutcome;
    use crate::engine::edit_candidate::CandidateRiskTier;
    use crate::engine::run_options::RetryScopeStage;
    use crate::engine::zone::PolicyZone;
    use crate::engine::semantic_contract::SemanticInvariantClause;
    use crate::model::exec_trace::{
        PolicyCandidateOutcome, PolicyCandidateTrace, PolicyExecutionTrace,
    };

    #[test]
    fn serde_serializes_trace_fields_as_legacy_strings() {
        let trace = PolicyExecutionTrace {
            policy: "naming_conventions".into(),
            parse_mode: "hybrid".to_string(),
            context_cluster: 9,
            candidate_line_count: 2,
            dropped_line_count: 1,
            semantic_impact_radius: 4,
            confidence_outcome: Some(PolicyDecisionOutcome::ApplyPartial),
            confidence_score: Some(0.81),
            confidence_threshold: Some(0.80),
            executor_scope: RetryScopeStage::Full,
            elapsed_ms: 0.0,
            candidate_trace: vec![PolicyCandidateTrace {
                line: 12,
                confidence: 0.9,
                style_gain: 1.2,
                utility: 2.1,
                risk_tier: CandidateRiskTier::High,
                impact_radius: 3,
                symbol_footprint_count: 2,
                range_footprint_count: 1,
                hard_constraints_touched: vec![SemanticInvariantClause::SymbolIdentity],
                zone: PolicyZone::Code,
                outcome: PolicyCandidateOutcome::Selected,
            }],
        };

        let value = serde_json::to_value(&trace).expect("serialize trace");
        assert_eq!(value["policy"], "naming_conventions");
        assert_eq!(value["parse_mode"], "hybrid");
        assert_eq!(value["executor_scope"], "full");
        assert_eq!(value["candidate_trace"][0]["risk_tier"], "high");
        assert_eq!(value["candidate_trace"][0]["zone"], "code");
        assert_eq!(
            value["candidate_trace"][0]["hard_constraints_touched"][0],
            "symbol_identity"
        );
        assert_eq!(value["candidate_trace"][0]["outcome"], "selected");
    }

    #[test]
    fn serde_deserializes_legacy_string_trace_fields() {
        let value = serde_json::json!({
            "policy": "naming_conventions",
            "parse_mode": "hybrid",
            "context_cluster": 7,
            "candidate_line_count": 3,
            "dropped_line_count": 1,
            "semantic_impact_radius": 2,
            "confidence_outcome": "ApplyPartial",
            "confidence_score": 0.77,
            "confidence_threshold": 0.70,
            "executor_scope": "node_local",
            "candidate_trace": [{
                "line": 5,
                "confidence": 0.5,
                "style_gain": 0.2,
                "utility": 0.9,
                "risk_tier": "medium",
                "impact_radius": 1,
                "symbol_footprint_count": 0,
                "range_footprint_count": 0,
                "hard_constraints_touched": ["scope_integrity"],
                "zone": "comments",
                "outcome": "blocked_zone"
            }]
        });

        let trace: PolicyExecutionTrace = serde_json::from_value(value).expect("deserialize");
        assert_eq!(trace.policy, "naming_conventions");
        assert_eq!(trace.parse_mode, "hybrid");
        assert_eq!(trace.executor_scope, RetryScopeStage::NodeLocal);
        assert_eq!(
            trace.candidate_trace[0].risk_tier,
            CandidateRiskTier::Medium
        );
        assert_eq!(trace.candidate_trace[0].zone, PolicyZone::Comments);
        assert_eq!(
            trace.candidate_trace[0].hard_constraints_touched,
            vec![SemanticInvariantClause::ScopeIntegrity]
        );
        assert_eq!(
            trace.candidate_trace[0].outcome,
            PolicyCandidateOutcome::BlockedZone
        );
    }
}
