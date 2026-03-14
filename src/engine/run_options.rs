use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::engine::semantic_contract::SemanticInvariantClause;
use crate::graph::snapshot::ProjectGraphSnapshot;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Default)]
pub enum RetryScopeStage {
    #[default]
    Full,
    CulpritRegion,
    NodeLocal,
    LineLocal,
}

impl RetryScopeStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::CulpritRegion => "culprit_region",
            Self::NodeLocal => "node_local",
            Self::LineLocal => "line_local",
        }
    }

    fn from_serialized(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "full" => Some(Self::Full),
            "culprit_region" => Some(Self::CulpritRegion),
            "node_local" => Some(Self::NodeLocal),
            "line_local" => Some(Self::LineLocal),
            _ => None,
        }
    }
}

impl Serialize for RetryScopeStage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RetryScopeStage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_serialized(value.as_str())
            .ok_or_else(|| serde::de::Error::custom(format!("unknown retry scope '{value}'")))
    }
}

#[derive(Clone, Debug, Default)]
pub struct PolicyRunOptions {
    pub blocked_policies: HashSet<String>,
    pub project_graph_snapshot: Option<Arc<ProjectGraphSnapshot>>,
    pub allowed_edit_lines: Option<BTreeSet<usize>>,
    pub retry_scope_stage: RetryScopeStage,
    pub previous_contract_failures: BTreeSet<SemanticInvariantClause>,
}

impl PolicyRunOptions {
    pub fn is_policy_blocked(&self, policy_name: &str) -> bool {
        self.blocked_policies.contains(policy_name)
    }
}
