use serde::{Deserialize, Serialize};

use crate::graph::state::ProjectGraphState;
use crate::graph::symbol_id::SymbolId;

// ── Edge ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum GraphEdgeKind {
    Reference,
    Call,
    Contains,
    Include,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GraphEdge {
    pub from: SymbolId,
    pub to: SymbolId,
    pub kind: GraphEdgeKind,
    pub weight: u32,
    pub last_seen_unix_ms: u64,
}

impl GraphEdge {
    pub fn new(from: SymbolId, to: SymbolId, kind: GraphEdgeKind) -> Self {
        Self {
            from,
            to,
            kind,
            weight: 1,
            last_seen_unix_ms: 0,
        }
    }

    pub fn same_identity(&self, other: &Self) -> bool {
        self.from == other.from && self.to == other.to && self.kind == other.kind
    }
}

// ── Node ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum GraphNodeKind {
    File,
    Namespace,
    Type,
    Function,
    Method,
    Variable,
    Field,
    Parameter,
    Macro,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphNode {
    pub symbol_id: SymbolId,
    pub name: String,
    pub kind: GraphNodeKind,
    pub file_path: String,
    pub line: usize,
    pub column: usize,
    pub scope_symbol_id: Option<SymbolId>,
    pub parser_consensus: f64,
    pub last_seen_unix_ms: u64,
}

impl GraphNode {
    pub fn new(
        symbol_id: SymbolId,
        name: impl Into<String>,
        kind: GraphNodeKind,
        file_path: impl Into<String>,
        line: usize,
        column: usize,
    ) -> Self {
        Self {
            symbol_id,
            name: name.into(),
            kind,
            file_path: file_path.into(),
            line,
            column,
            scope_symbol_id: None,
            parser_consensus: 1.0,
            last_seen_unix_ms: 0,
        }
    }
}

// ── Metrics & signals ─────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeMetrics {
    pub reference_count: u64,
    pub file_count: u32,
    pub consensus_score: f64,
    pub last_updated_unix_ms: u64,
}

impl Default for NodeMetrics {
    fn default() -> Self {
        Self {
            reference_count: 0,
            file_count: 0,
            consensus_score: 0.0,
            last_updated_unix_ms: 0,
        }
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ProjectSignal {
    pub reference_count: u64,
    pub file_count: u32,
    pub consensus_score: f64,
    pub from_tombstone: bool,
}

#[cfg(test)]
impl ProjectSignal {
    pub fn is_empty(self) -> bool {
        self.reference_count == 0 && self.file_count == 0 && self.consensus_score <= 0.0
    }
}

// ── Tombstone ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SymbolTombstone {
    pub removed_unix_ms: u64,
    pub reference_count: u64,
    pub file_count: u32,
    pub consensus_score: f64,
}

// ── Shape ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GraphShape {
    pub nodes: usize,
    pub edges: usize,
    pub metrics: usize,
    pub tombstones: usize,
}

impl GraphShape {
    pub fn from_state(state: &ProjectGraphState) -> Self {
        Self {
            nodes: state.nodes.len(),
            edges: state.edges.len(),
            metrics: state.metrics.len(),
            tombstones: state.tombstones.len(),
        }
    }
}
