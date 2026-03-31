use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::policy::zone::PolicyZone;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum CandidateRiskTier {
    Low,
    #[default]
    Medium,
    High,
}

impl CandidateRiskTier {
    pub fn precedence_score(&self) -> u8 {
        match self {
            Self::Low => 3,
            Self::Medium => 2,
            Self::High => 1,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    fn from_serialized(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            _ => None,
        }
    }
}

impl Serialize for CandidateRiskTier {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CandidateRiskTier {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_serialized(value.as_str())
            .ok_or_else(|| serde::de::Error::custom(format!("unknown risk tier '{value}'")))
    }
}

#[derive(Clone, Debug)]
pub struct PolicyEditCandidate {
    pub policy: Arc<str>,
    pub line: usize,
    pub confidence: f64,
    pub style_gain: f64,
    pub risk_tier: CandidateRiskTier,
    pub impact_radius: usize,
    pub symbol_footprint: Arc<[u64]>,
    pub range_footprint: Arc<[(usize, usize)]>,
    pub hard_constraints_touched: u16,
    pub zone: PolicyZone,
    pub after_fingerprint: u64,
}

// ── Decision outcome ──────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum PolicyDecisionOutcome {
    Apply,
    ApplyPartial,
    Block,
}
