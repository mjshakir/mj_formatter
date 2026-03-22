use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::files::atomic_writer::AtomicWriter;
use crate::files::codec::StateCodec;
use crate::graph::types::GraphEdge;
use crate::graph::types::GraphNode;
use crate::graph::types::GraphShape;
use crate::graph::types::NodeMetrics;
use crate::graph::snapshot::ProjectGraphSnapshot;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PersistStats {
    pub generation: u64,
    pub prune_enabled: bool,
    pub tombstone_enabled: bool,
    pub before: GraphShape,
    pub after: GraphShape,
}

impl PersistStats {
    pub fn nodes_removed(self) -> usize {
        self.before.nodes.saturating_sub(self.after.nodes)
    }

    pub fn edges_removed(self) -> usize {
        self.before.edges.saturating_sub(self.after.edges)
    }

    pub fn metrics_removed(self) -> usize {
        self.before.metrics.saturating_sub(self.after.metrics)
    }

    pub fn tombstones_added(self) -> usize {
        self.after.tombstones.saturating_sub(self.before.tombstones)
    }

    pub fn tombstones_removed(self) -> usize {
        self.before.tombstones.saturating_sub(self.after.tombstones)
    }

    pub fn changed(self) -> bool {
        self.before != self.after
    }
}
use crate::graph::state::{
    PolicyClusterLearningStats, ProjectGraphState, RetryStats,
    PROJECT_GRAPH_SCHEMA_VERSION,
};
use crate::graph::symbol_id::SymbolId;
use crate::graph::types::SymbolTombstone;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PersistedProjectGraph {
    generation: u64,
    state: ProjectGraphState,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct PolicyClusterLearningStatsV6 {
    decisions: u64,
    apply: u64,
    apply_partial: u64,
    advisory_only: u64,
    block: u64,
    accepted: u64,
    regressed: u64,
    reverted: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ProjectGraphStateV6 {
    schema_version: u32,
    nodes: BTreeMap<SymbolId, GraphNode>,
    edges: Vec<GraphEdge>,
    metrics: BTreeMap<SymbolId, NodeMetrics>,
    #[serde(default)]
    tombstones: BTreeMap<SymbolId, SymbolTombstone>,
    #[serde(default)]
    convergence_pairs: BTreeMap<String, u64>,
    #[serde(default)]
    pair_last_seen_ms: BTreeMap<String, u64>,
    #[serde(default)]
    policy_cluster_learning: BTreeMap<String, PolicyClusterLearningStatsV6>,
    #[serde(default)]
    retry_strategy_learning: BTreeMap<String, RetryStats>,
    #[serde(default)]
    retry_culprit_pairs: BTreeMap<String, u64>,
}

impl From<ProjectGraphStateV6> for ProjectGraphState {
    fn from(value: ProjectGraphStateV6) -> Self {
        let mut cluster_learning = BTreeMap::<String, PolicyClusterLearningStats>::new();
        for (key, legacy_stats) in value.policy_cluster_learning {
            cluster_learning.insert(
                key,
                PolicyClusterLearningStats {
                    decisions: legacy_stats.decisions,
                    apply: legacy_stats.apply,
                    apply_partial: legacy_stats.apply_partial,
                    advisory_only: legacy_stats.advisory_only,
                    block: legacy_stats.block,
                    accepted: legacy_stats.accepted,
                    regressed: legacy_stats.regressed,
                    reverted: legacy_stats.reverted,
                    decision_ema_bp: 0,
                    outcome_ema_bp: 0,
                    decision_events: 0,
                    outcome_events: 0,
                },
            );
        }
        ProjectGraphState {
            schema_version: value.schema_version,
            nodes: value.nodes,
            edges: value.edges,
            metrics: value.metrics,
            tombstones: value.tombstones,
            convergence_pairs: value.convergence_pairs,
            pair_last_seen_ms: value.pair_last_seen_ms,
            cluster_learning,
            retry_learning: value.retry_strategy_learning,
            culprit_pairs: value.retry_culprit_pairs,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PersistedProjectGraphV6 {
    generation: u64,
    state: ProjectGraphStateV6,
}

#[derive(Clone, Debug)]
pub struct ProjectGraphStoreOptions {
    pub prune_enabled: bool,
    pub retention_ms: u64,
    pub max_nodes: usize,
    pub max_edges: usize,
    pub tombstone_enabled: bool,
    pub tombstone_retention_ms: u64,
    pub tombstone_decay_ms: u64,
    pub convergence_decay_enabled: bool,
    pub convergence_decay_half_life_ms: u64,
    pub convergence_decay_min_count: u64,
}

impl Default for ProjectGraphStoreOptions {
    fn default() -> Self {
        Self {
            prune_enabled: true,
            retention_ms: 30 * 24 * 60 * 60 * 1000,
            max_nodes: 250_000,
            max_edges: 1_000_000,
            tombstone_enabled: true,
            tombstone_retention_ms: 90 * 24 * 60 * 60 * 1000,
            tombstone_decay_ms: 30 * 24 * 60 * 60 * 1000,
            convergence_decay_enabled: true,
            convergence_decay_half_life_ms: 30 * 24 * 60 * 60 * 1000,
            convergence_decay_min_count: 1,
        }
    }
}

pub struct ProjectGraphStore {
    path: PathBuf,
    generation: AtomicU64,
    options: ProjectGraphStoreOptions,
}

impl ProjectGraphStore {
    #[cfg(test)]
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_options(path, ProjectGraphStoreOptions::default())
    }

    pub fn open_with_options(
        path: impl AsRef<Path>,
        options: ProjectGraphStoreOptions,
    ) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let generation = Self::read_generation(path.as_path())?;
        Ok(Self {
            path,
            generation: AtomicU64::new(generation),
            options,
        })
    }

    pub fn load_snapshot(&self) -> Result<ProjectGraphSnapshot> {
        let persisted = Self::read_persisted(self.path.as_path())?;
        let (generation, mut state) = match persisted {
            Some(value) => (value.generation, value.state),
            None => (0, ProjectGraphState::new()),
        };

        if state.schema_version != PROJECT_GRAPH_SCHEMA_VERSION {
            state = Self::migrate_schema(state);
        }

        self.generation.store(generation, Ordering::Release);
        Ok(ProjectGraphSnapshot::with_tombstone_decay(
            Arc::new(state),
            self.options.tombstone_decay_ms,
        ))
    }

    #[cfg(test)]
    pub fn persist_state(&self, state: &ProjectGraphState) -> Result<ProjectGraphSnapshot> {
        let (snapshot, _) = self.persist_with_stats(state)?;
        Ok(snapshot)
    }

    pub fn persist_with_stats(
        &self,
        state: &ProjectGraphState,
    ) -> Result<(ProjectGraphSnapshot, PersistStats)> {
        let next_generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let mut to_persist = state.clone();
        to_persist.normalize_schema();
        let now_unix_ms = current_unix_ms();
        if self.options.convergence_decay_enabled {
            to_persist.decay_convergence_pairs(
                now_unix_ms,
                self.options.convergence_decay_half_life_ms,
                self.options.convergence_decay_min_count,
            );
        }
        let before = GraphShape::from_state(&to_persist);
        if self.options.prune_enabled {
            to_persist.compact(
                now_unix_ms,
                self.options.retention_ms,
                self.options.max_nodes,
                self.options.max_edges,
                self.options.tombstone_enabled,
                self.options.tombstone_retention_ms,
            );
        }
        let after = GraphShape::from_state(&to_persist);
        let payload = PersistedProjectGraph {
            generation: next_generation,
            state: to_persist,
        };
        let bytes = StateCodec::encode_binary(&payload)
            .context("failed to serialize project graph state")?;
        AtomicWriter::write_bytes(self.path.as_path(), bytes.as_slice()).with_context(|| {
            format!("failed writing project graph state {}", self.path.display())
        })?;

        let snapshot = ProjectGraphSnapshot::with_tombstone_decay(
            Arc::new(payload.state),
            self.options.tombstone_decay_ms,
        );
        let stats = PersistStats {
            generation: next_generation,
            prune_enabled: self.options.prune_enabled,
            tombstone_enabled: self.options.tombstone_enabled,
            before,
            after,
        };

        Ok((snapshot, stats))
    }

    #[cfg(test)]
    pub fn update<F>(&self, mutator: F) -> Result<ProjectGraphSnapshot>
    where
        F: FnOnce(&mut ProjectGraphState),
    {
        let snapshot = self.load_snapshot()?;
        let mut state = snapshot.to_state_clone();
        mutator(&mut state);
        self.persist_state(&state)
    }

    fn read_generation(path: &Path) -> Result<u64> {
        let Some(value) = Self::read_persisted(path)? else {
            return Ok(0);
        };
        Ok(value.generation)
    }

    fn read_persisted(path: &Path) -> Result<Option<PersistedProjectGraph>> {
        if !path.exists() {
            return Ok(None);
        }
        let metadata = fs::metadata(path)
            .with_context(|| format!("failed reading project graph metadata {}", path.display()))?;
        if metadata.len() == 0 {
            return Ok(None);
        }
        if metadata.len() as usize > StateCodec::max_state_bytes() {
            anyhow::bail!(
                "project graph state too large: {} bytes (max {}) at {}",
                metadata.len(),
                StateCodec::max_state_bytes(),
                path.display()
            );
        }
        let payload = match StateCodec::read_decode_binary::<PersistedProjectGraph>(path) {
            Ok(value) => value,
            Err(primary_err) => {
                match StateCodec::read_decode_binary::<PersistedProjectGraphV6>(path) {
                    Ok(legacy) => PersistedProjectGraph {
                        generation: legacy.generation,
                        state: legacy.state.into(),
                    },
                    Err(legacy_err) => {
                        Self::quarantine_corrupted_store(path, &primary_err, &legacy_err);
                        return Ok(None);
                    }
                }
            }
        };
        Ok(Some(payload))
    }

    fn quarantine_corrupted_store(
        path: &Path,
        primary_err: &anyhow::Error,
        legacy_err: &anyhow::Error,
    ) {
        let timestamp = current_unix_ms();
        let file_name = path
            .file_name()
            .and_then(|item| item.to_str())
            .unwrap_or("project_graph_state.bin");
        let quarantined = path.with_file_name(format!("{file_name}.corrupt.{timestamp}"));
        match fs::rename(path, quarantined.as_path()) {
            Ok(_) => warn!(
                path = %path.display(),
                quarantined = %quarantined.display(),
                primary_error = %primary_err,
                legacy_error = %legacy_err,
                "project graph store decode failed; quarantined corrupt state and starting with empty graph"
            ),
            Err(rename_err) => warn!(
                path = %path.display(),
                quarantined = %quarantined.display(),
                rename_error = %rename_err,
                primary_error = %primary_err,
                legacy_error = %legacy_err,
                "project graph store decode failed and quarantine rename failed; starting with empty graph"
            ),
        }
    }

    fn migrate_schema(mut state: ProjectGraphState) -> ProjectGraphState {
        // v1..v6 keeps existing data and upgrades metadata; canonical USR IDs,
        // tombstone entries, convergence memory, and learning-state metadata are managed by runtime updates.
        if state.schema_version < PROJECT_GRAPH_SCHEMA_VERSION {
            state.normalize_schema();
            return state;
        }
        if state.schema_version > PROJECT_GRAPH_SCHEMA_VERSION {
            // Unknown future schema; start from a clean compatible state.
            return ProjectGraphState::new();
        }
        state
    }
}

fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::files::atomic_writer::AtomicWriter;
    use crate::files::codec::StateCodec;
    use crate::graph::types::GraphEdge;
    use crate::graph::types::GraphEdgeKind;
    use crate::graph::types::GraphNode;
    use crate::graph::types::GraphNodeKind;
    use crate::graph::types::NodeMetrics;
    use crate::graph::state::ProjectGraphState;
    use crate::graph::store::{ProjectGraphStore, ProjectGraphStoreOptions};
    use crate::graph::symbol_id::SymbolId;

    fn temp_graph_path() -> std::path::PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("fmt_graph_{stamp}.json"))
    }

    #[test]
    fn store_roundtrip() {
        let path = temp_graph_path();
        let store = ProjectGraphStore::open(path.as_path()).expect("open store");

        let mut state = ProjectGraphState::new();
        state.upsert_node(GraphNode::new(
            SymbolId::new("usr:demo"),
            "demo",
            GraphNodeKind::Function,
            "src/demo.cpp",
            12,
            3,
        ));

        let written = store.persist_state(&state).expect("persist");
        assert_eq!(written.node_count(), 1);

        let read_back = store.load_snapshot().expect("load");
        assert_eq!(read_back.node_count(), 1);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn api_mutates_state() {
        let path = temp_graph_path();
        let store = ProjectGraphStore::open(path.as_path()).expect("open store");

        let snapshot = store
            .update(|state| {
                state.upsert_node(GraphNode::new(
                    SymbolId::new("usr:update"),
                    "updated",
                    GraphNodeKind::Variable,
                    "src/update.cpp",
                    9,
                    1,
                ));
            })
            .expect("update");

        assert_eq!(snapshot.node_count(), 1);

        let loaded = store.load_snapshot().expect("load");
        assert!(loaded.node(&SymbolId::new("usr:update")).is_some());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn persist_compaction_options() {
        let path = temp_graph_path();
        let store = ProjectGraphStore::open_with_options(
            path.as_path(),
            ProjectGraphStoreOptions {
                prune_enabled: true,
                retention_ms: 1,
                max_nodes: 2,
                max_edges: 1,
                tombstone_enabled: true,
                tombstone_retention_ms: u64::MAX,
                tombstone_decay_ms: 10_000,
                convergence_decay_enabled: true,
                convergence_decay_half_life_ms: 10_000,
                convergence_decay_min_count: 1,
            },
        )
        .expect("open store");

        let file_id = SymbolId::new("file|src/demo.cpp");
        let left = SymbolId::new("usr:left");
        let right = SymbolId::new("usr:right");

        let mut state = ProjectGraphState::new();
        let mut file = GraphNode::new(
            file_id.clone(),
            "src/demo.cpp",
            GraphNodeKind::File,
            "src/demo.cpp",
            0,
            0,
        );
        file.last_seen_unix_ms = u64::MAX;
        state.upsert_node(file);

        let mut left_node = GraphNode::new(
            left.clone(),
            "left",
            GraphNodeKind::Function,
            "src/demo.cpp",
            1,
            1,
        );
        left_node.last_seen_unix_ms = u64::MAX;
        state.upsert_node(left_node);
        let mut right_node = GraphNode::new(
            right.clone(),
            "right",
            GraphNodeKind::Function,
            "src/demo.cpp",
            2,
            1,
        );
        right_node.last_seen_unix_ms = u64::MAX;
        state.upsert_node(right_node);

        let mut edge_left = GraphEdge::new(file_id.clone(), left.clone(), GraphEdgeKind::Contains);
        edge_left.last_seen_unix_ms = u64::MAX;
        state.upsert_edge(edge_left);
        let mut edge_right =
            GraphEdge::new(file_id.clone(), right.clone(), GraphEdgeKind::Contains);
        edge_right.last_seen_unix_ms = u64::MAX;
        state.upsert_edge(edge_right);

        state.set_metrics(
            left,
            NodeMetrics {
                reference_count: 1,
                file_count: 1,
                consensus_score: 0.9,
                last_updated_unix_ms: u64::MAX,
            },
        );
        state.set_metrics(
            right,
            NodeMetrics {
                reference_count: 1,
                file_count: 1,
                consensus_score: 0.9,
                last_updated_unix_ms: u64::MAX,
            },
        );

        let (written, stats) = store.persist_with_stats(&state).expect("persist");
        assert_eq!(written.node_count(), 2);
        assert_eq!(written.edge_count(), 1);
        assert_eq!(written.state().metrics.len(), 1);
        assert_eq!(written.state().tombstones.len(), 1);
        assert!(stats.changed());
        assert_eq!(stats.nodes_removed(), 1);
        assert_eq!(stats.edges_removed(), 1);
        assert_eq!(stats.metrics_removed(), 1);
        assert_eq!(stats.tombstones_added(), 1);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn persist_decay_options() {
        let path = temp_graph_path();
        let store = ProjectGraphStore::open_with_options(
            path.as_path(),
            ProjectGraphStoreOptions {
                prune_enabled: false,
                retention_ms: 1,
                max_nodes: 2,
                max_edges: 1,
                tombstone_enabled: false,
                tombstone_retention_ms: u64::MAX,
                tombstone_decay_ms: 10_000,
                convergence_decay_enabled: true,
                convergence_decay_half_life_ms: 1_000,
                convergence_decay_min_count: 1,
            },
        )
        .expect("open store");

        let mut state = ProjectGraphState::new();
        state.record_pair_at("naming_conventions", "clang_format", 100, 1);

        let written = store.persist_state(&state).expect("persist");
        assert_eq!(
            written
                .state()
                .convergence_pair_count("naming_conventions", "clang_format"),
            0
        );
        assert!(written.state().convergence_pairs.is_empty());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn decodes_legacy_v6() {
        let path = temp_graph_path();
        let mut legacy_state = super::ProjectGraphStateV6 {
            schema_version: 6,
            ..super::ProjectGraphStateV6::default()
        };
        legacy_state.policy_cluster_learning.insert(
            "naming_conventions|42".to_string(),
            super::PolicyClusterLearningStatsV6 {
                decisions: 4,
                apply: 3,
                accepted: 2,
                ..super::PolicyClusterLearningStatsV6::default()
            },
        );
        let legacy_payload = super::PersistedProjectGraphV6 {
            generation: 9,
            state: legacy_state,
        };
        let bytes = StateCodec::encode_binary(&legacy_payload).expect("encode legacy payload");
        AtomicWriter::write_bytes(path.as_path(), bytes.as_slice()).expect("write payload");

        let store = ProjectGraphStore::open(path.as_path()).expect("open store");
        let snapshot = store.load_snapshot().expect("load legacy snapshot");
        let clusters = snapshot.state().cluster_snapshot();
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].policy, "naming_conventions");
        assert_eq!(clusters[0].cluster, 42);
        assert_eq!(clusters[0].stats.decisions, 4);
        assert_eq!(clusters[0].stats.apply, 3);
        assert_eq!(clusters[0].stats.accepted, 2);
        assert_eq!(clusters[0].stats.decision_events, 0);
        assert_eq!(clusters[0].stats.outcome_events, 0);

        let _ = fs::remove_file(path);
    }
}
