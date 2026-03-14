use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::model::policy_name::PolicyName;
use crate::graph::types::GraphEdge;
use crate::graph::types::GraphEdgeKind;
use crate::graph::types::GraphNode;
use crate::graph::types::GraphNodeKind;
use crate::graph::types::NodeMetrics;
use crate::graph::types::ProjectSignal;
use crate::graph::symbol_id::SymbolId;
use crate::graph::types::SymbolTombstone;

pub const PROJECT_GRAPH_SCHEMA_VERSION: u32 = 7;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PolicyClusterLearningStats {
    pub decisions: u64,
    pub apply: u64,
    pub apply_partial: u64,
    pub advisory_only: u64,
    pub block: u64,
    pub accepted: u64,
    pub regressed: u64,
    pub reverted: u64,
    #[serde(default)]
    pub decision_ema_bp: u16,
    #[serde(default)]
    pub outcome_ema_bp: u16,
    #[serde(default)]
    pub decision_events: u64,
    #[serde(default)]
    pub outcome_events: u64,
}

impl PolicyClusterLearningStats {
    pub fn decision_total(&self) -> u64 {
        self.decisions
    }

    pub fn outcome_total(&self) -> u64 {
        self.accepted
            .saturating_add(self.regressed)
            .saturating_add(self.reverted)
    }

    pub fn has_adaptive_hints(&self) -> bool {
        self.decision_events > 0 || self.outcome_events > 0
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RetryStrategyLearningStats {
    pub attempts: u64,
    pub successes: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PolicyClusterLearningSnapshotEntry {
    pub policy: PolicyName,
    pub cluster: u64,
    pub stats: PolicyClusterLearningStats,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RetryCulpritPairSnapshotEntry {
    pub culprit_policy: PolicyName,
    pub peer_policy: PolicyName,
    pub count: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProjectGraphState {
    pub schema_version: u32,
    pub nodes: BTreeMap<SymbolId, GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub metrics: BTreeMap<SymbolId, NodeMetrics>,
    #[serde(default)]
    pub tombstones: BTreeMap<SymbolId, SymbolTombstone>,
    #[serde(default)]
    pub convergence_pairs: BTreeMap<String, u64>,
    #[serde(default)]
    pub convergence_pair_last_seen_unix_ms: BTreeMap<String, u64>,
    #[serde(default)]
    pub policy_cluster_learning: BTreeMap<String, PolicyClusterLearningStats>,
    #[serde(default)]
    pub retry_strategy_learning: BTreeMap<String, RetryStrategyLearningStats>,
    #[serde(default)]
    pub retry_culprit_pairs: BTreeMap<String, u64>,
}

impl ProjectGraphState {
    pub fn new() -> Self {
        Self {
            schema_version: PROJECT_GRAPH_SCHEMA_VERSION,
            nodes: BTreeMap::new(),
            edges: Vec::new(),
            metrics: BTreeMap::new(),
            tombstones: BTreeMap::new(),
            convergence_pairs: BTreeMap::new(),
            convergence_pair_last_seen_unix_ms: BTreeMap::new(),
            policy_cluster_learning: BTreeMap::new(),
            retry_strategy_learning: BTreeMap::new(),
            retry_culprit_pairs: BTreeMap::new(),
        }
    }

    pub fn normalize_schema(&mut self) {
        self.schema_version = PROJECT_GRAPH_SCHEMA_VERSION;
    }

    pub fn upsert_node(&mut self, node: GraphNode) {
        self.nodes.insert(node.symbol_id.clone(), node);
    }

    pub fn upsert_edge(&mut self, edge: GraphEdge) {
        if let Some(existing) = self
            .edges
            .iter_mut()
            .find(|existing| existing.same_identity(&edge))
        {
            existing.weight = existing.weight.saturating_add(edge.weight.max(1));
            if edge.last_seen_unix_ms > existing.last_seen_unix_ms {
                existing.last_seen_unix_ms = edge.last_seen_unix_ms;
            }
            return;
        }
        self.edges.push(edge);
    }

    pub fn set_metrics(&mut self, symbol_id: SymbolId, metrics: NodeMetrics) {
        self.metrics.insert(symbol_id, metrics);
    }

    pub fn node(&self, symbol_id: &SymbolId) -> Option<&GraphNode> {
        self.nodes.get(symbol_id)
    }

    pub fn node_metrics(&self, symbol_id: &SymbolId) -> Option<&NodeMetrics> {
        self.metrics.get(symbol_id)
    }

    #[cfg(test)]
    pub fn symbol_reference_count(&self, symbol_id: &SymbolId) -> u64 {
        self.metrics
            .get(symbol_id)
            .map(|entry| entry.reference_count)
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub fn symbol_file_count(&self, symbol_id: &SymbolId) -> u32 {
        self.metrics
            .get(symbol_id)
            .map(|entry| entry.file_count)
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub fn convergence_pair_count(&self, loser: &str, winner: &str) -> u64 {
        let key = Self::convergence_pair_key(loser, winner);
        self.convergence_pairs.get(&key).copied().unwrap_or(0)
    }

    pub fn record_convergence_pair(&mut self, loser: &str, winner: &str, count: u64) {
        self.record_convergence_pair_at(loser, winner, count, current_unix_ms());
    }

    pub fn record_convergence_pair_at(
        &mut self,
        loser: &str,
        winner: &str,
        count: u64,
        now_unix_ms: u64,
    ) {
        if loser.is_empty() || winner.is_empty() || count == 0 {
            return;
        }
        let key = Self::convergence_pair_key(loser, winner);
        *self.convergence_pairs.entry(key.clone()).or_insert(0) += count;
        self.convergence_pair_last_seen_unix_ms
            .insert(key, now_unix_ms);
    }

    pub fn record_convergence_pairs(&mut self, pairs: &BTreeMap<(String, String), usize>) {
        for ((loser, winner), count) in pairs {
            self.record_convergence_pair(loser, winner, *count as u64);
        }
    }

    pub fn decay_convergence_pairs(&mut self, now_unix_ms: u64, half_life_ms: u64, min_count: u64) {
        if self.convergence_pairs.is_empty() || half_life_ms == 0 {
            return;
        }
        let floor = min_count.max(1);
        let mut updated_pairs = BTreeMap::<String, u64>::new();
        let mut updated_last_seen = BTreeMap::<String, u64>::new();
        for (key, count) in &self.convergence_pairs {
            let last_seen = self
                .convergence_pair_last_seen_unix_ms
                .get(key)
                .copied()
                .unwrap_or(now_unix_ms);
            let elapsed_ms = now_unix_ms.saturating_sub(last_seen);
            let decay_factor = if elapsed_ms == 0 {
                1.0
            } else {
                2f64.powf(-(elapsed_ms as f64 / half_life_ms as f64))
            };
            let decayed = ((*count as f64) * decay_factor).round() as u64;
            if decayed >= floor {
                updated_pairs.insert(key.clone(), decayed);
                updated_last_seen.insert(key.clone(), now_unix_ms);
            }
        }
        self.convergence_pairs = updated_pairs;
        self.convergence_pair_last_seen_unix_ms = updated_last_seen;
    }

    #[cfg(test)]
    pub fn record_policy_cluster_learning(
        &mut self,
        policy: &str,
        cluster: u64,
        stats: &PolicyClusterLearningStats,
    ) {
        if policy.trim().is_empty() {
            return;
        }
        let key = Self::policy_cluster_key(policy, cluster);
        let entry = self.policy_cluster_learning.entry(key).or_default();
        entry.decisions = entry.decisions.saturating_add(stats.decisions);
        entry.apply = entry.apply.saturating_add(stats.apply);
        entry.apply_partial = entry.apply_partial.saturating_add(stats.apply_partial);
        entry.advisory_only = entry.advisory_only.saturating_add(stats.advisory_only);
        entry.block = entry.block.saturating_add(stats.block);
        entry.accepted = entry.accepted.saturating_add(stats.accepted);
        entry.regressed = entry.regressed.saturating_add(stats.regressed);
        entry.reverted = entry.reverted.saturating_add(stats.reverted);
        Self::merge_adaptive_hints(entry, stats);
    }

    pub fn replace_policy_cluster_learning_entries(
        &mut self,
        entries: &[PolicyClusterLearningSnapshotEntry],
    ) {
        self.policy_cluster_learning.clear();
        for entry in entries {
            if entry.policy.as_str().trim().is_empty() {
                continue;
            }
            let key = Self::policy_cluster_key(entry.policy.as_str(), entry.cluster);
            self.policy_cluster_learning
                .insert(key, entry.stats.clone());
        }
    }

    pub fn policy_cluster_learning_snapshot(&self) -> Vec<PolicyClusterLearningSnapshotEntry> {
        let mut snapshot = Vec::with_capacity(self.policy_cluster_learning.len());
        for (key, stats) in &self.policy_cluster_learning {
            if let Some((policy, cluster)) = Self::parse_policy_cluster_key(key.as_str()) {
                snapshot.push(PolicyClusterLearningSnapshotEntry {
                    policy,
                    cluster,
                    stats: stats.clone(),
                });
            }
        }
        snapshot
    }

    pub fn record_retry_strategy_learning(
        &mut self,
        strategy: &str,
        context: &str,
        attempts: u64,
        successes: u64,
    ) {
        if strategy.trim().is_empty() || attempts == 0 {
            return;
        }
        let key = Self::retry_strategy_key(strategy, context);
        let entry = self.retry_strategy_learning.entry(key).or_default();
        entry.attempts = entry.attempts.saturating_add(attempts);
        entry.successes = entry.successes.saturating_add(successes);
    }

    pub fn retry_strategy_learning_snapshot(
        &self,
    ) -> Vec<(String, String, RetryStrategyLearningStats)> {
        let mut snapshot = Vec::with_capacity(self.retry_strategy_learning.len());
        for (key, stats) in &self.retry_strategy_learning {
            if let Some((strategy, context)) = Self::parse_retry_strategy_key(key.as_str()) {
                snapshot.push((strategy, context, stats.clone()));
            }
        }
        snapshot
    }

    pub fn record_retry_culprit_pair(&mut self, culprit: &str, peer: &str, count: u64) {
        if culprit.trim().is_empty() || peer.trim().is_empty() || count == 0 {
            return;
        }
        let key = Self::retry_culprit_pair_key(culprit, peer);
        *self.retry_culprit_pairs.entry(key).or_insert(0) += count;
    }

    pub fn record_retry_culprit_pairs(&mut self, pairs: &[RetryCulpritPairSnapshotEntry]) {
        for pair in pairs {
            self.record_retry_culprit_pair(
                pair.culprit_policy.as_str(),
                pair.peer_policy.as_str(),
                pair.count,
            );
        }
    }

    pub fn retry_culprit_pairs_snapshot(&self) -> Vec<RetryCulpritPairSnapshotEntry> {
        let mut snapshot = Vec::with_capacity(self.retry_culprit_pairs.len());
        for (key, count) in &self.retry_culprit_pairs {
            if let Some((culprit, peer)) = Self::parse_retry_culprit_pair_key(key.as_str()) {
                snapshot.push(RetryCulpritPairSnapshotEntry {
                    culprit_policy: culprit,
                    peer_policy: peer,
                    count: *count,
                });
            }
        }
        snapshot
    }

    pub fn symbol_project_signal(
        &self,
        symbol_id: &SymbolId,
        now_unix_ms: u64,
        tombstone_decay_ms: u64,
    ) -> Option<ProjectSignal> {
        if let Some(metrics) = self.metrics.get(symbol_id) {
            return Some(ProjectSignal {
                reference_count: metrics.reference_count,
                file_count: metrics.file_count,
                consensus_score: metrics.consensus_score.clamp(0.0, 1.0),
                from_tombstone: false,
            });
        }

        let tombstone = self.tombstones.get(symbol_id)?;
        let decay_factor = if tombstone_decay_ms == 0 {
            1.0
        } else {
            let age = now_unix_ms.saturating_sub(tombstone.removed_unix_ms) as f64;
            (-age / tombstone_decay_ms as f64).exp().clamp(0.0, 1.0)
        };
        let signal = ProjectSignal {
            reference_count: (tombstone.reference_count as f64 * decay_factor).round() as u64,
            file_count: (tombstone.file_count as f64 * decay_factor).round() as u32,
            consensus_score: (tombstone.consensus_score * decay_factor).clamp(0.0, 1.0),
            from_tombstone: true,
        };
        (!signal.is_empty()).then_some(signal)
    }

    pub fn compact(
        &mut self,
        now_unix_ms: u64,
        retention_ms: u64,
        max_nodes: usize,
        max_edges: usize,
        tombstone_enabled: bool,
        tombstone_retention_ms: u64,
    ) {
        let original_nodes = self.nodes.keys().cloned().collect::<BTreeSet<SymbolId>>();
        let cutoff = now_unix_ms.saturating_sub(retention_ms.max(1));
        self.prune_stale(cutoff);
        self.cap_edges(max_edges.max(1));
        self.cap_nodes(max_nodes.max(1));
        self.remove_orphan_edges();

        let remaining_nodes = self.nodes.keys().cloned().collect::<BTreeSet<SymbolId>>();
        let removed_nodes = original_nodes
            .difference(&remaining_nodes)
            .cloned()
            .collect::<Vec<_>>();
        if tombstone_enabled {
            self.capture_removed_nodes_as_tombstones(removed_nodes.as_slice(), now_unix_ms);
            self.tombstones
                .retain(|symbol_id, _| !self.nodes.contains_key(symbol_id));
            let cutoff = now_unix_ms.saturating_sub(tombstone_retention_ms.max(1));
            self.tombstones.retain(|_, tombstone| {
                tombstone.removed_unix_ms == 0 || tombstone.removed_unix_ms >= cutoff
            });
        } else {
            self.tombstones.clear();
        }

        self.metrics
            .retain(|symbol_id, _| self.nodes.contains_key(symbol_id));
        self.rebuild_metric_file_counts();
        self.cap_convergence_pairs(4_096);
        self.cap_learning_state(16_384, 16_384, 16_384);
    }

    fn capture_removed_nodes_as_tombstones(
        &mut self,
        removed_nodes: &[SymbolId],
        now_unix_ms: u64,
    ) {
        for symbol_id in removed_nodes {
            let Some(metrics) = self.metrics.get(symbol_id) else {
                continue;
            };
            self.tombstones.insert(
                symbol_id.clone(),
                SymbolTombstone {
                    removed_unix_ms: now_unix_ms,
                    reference_count: metrics.reference_count,
                    file_count: metrics.file_count,
                    consensus_score: metrics.consensus_score.clamp(0.0, 1.0),
                },
            );
        }
    }

    fn prune_stale(&mut self, cutoff_unix_ms: u64) {
        self.edges
            .retain(|edge| edge.last_seen_unix_ms == 0 || edge.last_seen_unix_ms >= cutoff_unix_ms);
        self.remove_orphan_edges();

        let connected = self.connected_symbols();
        self.nodes.retain(|symbol_id, node| {
            connected.contains(symbol_id)
                || node.last_seen_unix_ms == 0
                || node.last_seen_unix_ms >= cutoff_unix_ms
        });
        self.remove_orphan_edges();
    }

    fn cap_edges(&mut self, max_edges: usize) {
        if self.edges.len() <= max_edges {
            return;
        }
        self.edges.sort_by(|left, right| {
            right
                .last_seen_unix_ms
                .cmp(&left.last_seen_unix_ms)
                .then(right.weight.cmp(&left.weight))
                .then(left.from.cmp(&right.from))
                .then(left.to.cmp(&right.to))
        });
        self.edges.truncate(max_edges);
    }

    fn cap_nodes(&mut self, max_nodes: usize) {
        if self.nodes.len() <= max_nodes {
            return;
        }
        let mut to_remove = BTreeSet::<SymbolId>::new();
        let mut remaining = self.nodes.len().saturating_sub(max_nodes);

        let connected = self.connected_symbols();
        let mut unconnected = self
            .nodes
            .iter()
            .filter(|(symbol_id, _)| !connected.contains(*symbol_id))
            .map(|(symbol_id, node)| (node.last_seen_unix_ms, symbol_id.clone()))
            .collect::<Vec<_>>();
        unconnected.sort();
        for (_, symbol_id) in unconnected {
            if remaining == 0 {
                break;
            }
            if to_remove.insert(symbol_id) {
                remaining = remaining.saturating_sub(1);
            }
        }

        if remaining > 0 {
            let mut non_file_oldest = self
                .nodes
                .iter()
                .filter(|(_, node)| node.kind != GraphNodeKind::File)
                .map(|(symbol_id, node)| (node.last_seen_unix_ms, symbol_id.clone()))
                .collect::<Vec<_>>();
            non_file_oldest.sort();
            for (_, symbol_id) in non_file_oldest {
                if remaining == 0 {
                    break;
                }
                if to_remove.insert(symbol_id) {
                    remaining = remaining.saturating_sub(1);
                }
            }
        }

        if remaining > 0 {
            let mut oldest = self
                .nodes
                .iter()
                .map(|(symbol_id, node)| (node.last_seen_unix_ms, symbol_id.clone()))
                .collect::<Vec<_>>();
            oldest.sort();
            for (_, symbol_id) in oldest {
                if remaining == 0 {
                    break;
                }
                if to_remove.insert(symbol_id) {
                    remaining = remaining.saturating_sub(1);
                }
            }
        }

        if to_remove.is_empty() {
            return;
        }

        self.nodes
            .retain(|symbol_id, _| !to_remove.contains(symbol_id));
        self.edges
            .retain(|edge| !to_remove.contains(&edge.from) && !to_remove.contains(&edge.to));
    }

    fn remove_orphan_edges(&mut self) {
        self.edges.retain(|edge| {
            self.nodes.contains_key(&edge.from) && self.nodes.contains_key(&edge.to)
        });
    }

    fn connected_symbols(&self) -> BTreeSet<SymbolId> {
        let mut connected = BTreeSet::<SymbolId>::new();
        for edge in &self.edges {
            connected.insert(edge.from.clone());
            connected.insert(edge.to.clone());
        }
        connected
    }

    fn rebuild_metric_file_counts(&mut self) {
        let mut file_counts = BTreeMap::<SymbolId, u32>::new();
        for edge in &self.edges {
            if edge.kind != GraphEdgeKind::Contains {
                continue;
            }
            let count = file_counts.entry(edge.to.clone()).or_insert(0);
            *count = count.saturating_add(1);
        }
        for (symbol_id, metrics) in &mut self.metrics {
            metrics.file_count = file_counts.get(symbol_id).copied().unwrap_or(0);
        }
    }

    fn cap_convergence_pairs(&mut self, max_pairs: usize) {
        if self.convergence_pairs.len() <= max_pairs {
            return;
        }
        let mut ranked = self
            .convergence_pairs
            .iter()
            .map(|(key, count)| (key.clone(), *count))
            .collect::<Vec<_>>();
        ranked.sort_by(|(left_key, left_count), (right_key, right_count)| {
            right_count.cmp(left_count).then(left_key.cmp(right_key))
        });
        ranked.truncate(max_pairs);
        let keep_keys = ranked
            .iter()
            .map(|(key, _)| key.clone())
            .collect::<BTreeSet<_>>();
        self.convergence_pairs = ranked.into_iter().collect();
        self.convergence_pair_last_seen_unix_ms
            .retain(|key, _| keep_keys.contains(key));
    }

    fn cap_learning_state(
        &mut self,
        max_cluster_entries: usize,
        max_retry_entries: usize,
        max_retry_pairs: usize,
    ) {
        if self.policy_cluster_learning.len() > max_cluster_entries {
            let mut ranked = self
                .policy_cluster_learning
                .iter()
                .map(|(key, stats)| {
                    (
                        key.clone(),
                        stats.decision_total().saturating_add(stats.outcome_total()),
                    )
                })
                .collect::<Vec<_>>();
            ranked.sort_by(|(left_key, left_count), (right_key, right_count)| {
                right_count.cmp(left_count).then(left_key.cmp(right_key))
            });
            ranked.truncate(max_cluster_entries);
            let keep = ranked
                .iter()
                .map(|(key, _)| key.clone())
                .collect::<BTreeSet<_>>();
            self.policy_cluster_learning
                .retain(|key, _| keep.contains(key));
        }

        if self.retry_strategy_learning.len() > max_retry_entries {
            let mut ranked = self
                .retry_strategy_learning
                .iter()
                .map(|(key, stats)| (key.clone(), stats.attempts))
                .collect::<Vec<_>>();
            ranked.sort_by(|(left_key, left_count), (right_key, right_count)| {
                right_count.cmp(left_count).then(left_key.cmp(right_key))
            });
            ranked.truncate(max_retry_entries);
            let keep = ranked
                .iter()
                .map(|(key, _)| key.clone())
                .collect::<BTreeSet<_>>();
            self.retry_strategy_learning
                .retain(|key, _| keep.contains(key));
        }

        if self.retry_culprit_pairs.len() > max_retry_pairs {
            let mut ranked = self
                .retry_culprit_pairs
                .iter()
                .map(|(key, count)| (key.clone(), *count))
                .collect::<Vec<_>>();
            ranked.sort_by(|(left_key, left_count), (right_key, right_count)| {
                right_count.cmp(left_count).then(left_key.cmp(right_key))
            });
            ranked.truncate(max_retry_pairs);
            self.retry_culprit_pairs = ranked.into_iter().collect();
        }
    }

    fn convergence_pair_key(loser: &str, winner: &str) -> String {
        format!("{loser}|{winner}")
    }

    fn policy_cluster_key(policy: &str, cluster: u64) -> String {
        format!("{policy}|{cluster}")
    }

    fn parse_policy_cluster_key(value: &str) -> Option<(PolicyName, u64)> {
        let (policy, cluster) = value.split_once('|')?;
        if policy.is_empty() {
            return None;
        }
        let parsed_cluster = cluster.parse::<u64>().ok()?;
        Some((policy.into(), parsed_cluster))
    }

    fn retry_strategy_key(strategy: &str, context: &str) -> String {
        format!("{strategy}\n{context}")
    }

    fn parse_retry_strategy_key(value: &str) -> Option<(String, String)> {
        let (strategy, context) = value.split_once('\n')?;
        if strategy.is_empty() {
            return None;
        }
        Some((strategy.to_string(), context.to_string()))
    }

    fn retry_culprit_pair_key(culprit: &str, peer: &str) -> String {
        format!("{culprit}|{peer}")
    }

    fn parse_retry_culprit_pair_key(value: &str) -> Option<(PolicyName, PolicyName)> {
        let (culprit, peer) = value.split_once('|')?;
        if culprit.is_empty() || peer.is_empty() {
            return None;
        }
        Some((culprit.into(), peer.into()))
    }

    #[cfg(test)]
    fn merge_adaptive_hints(
        target: &mut PolicyClusterLearningStats,
        incoming: &PolicyClusterLearningStats,
    ) {
        let prior_decision_events = target.decision_events;
        let prior_outcome_events = target.outcome_events;
        target.decision_events = target
            .decision_events
            .saturating_add(incoming.decision_events);
        target.outcome_events = target
            .outcome_events
            .saturating_add(incoming.outcome_events);

        let decision_ema = weighted_basis_points_merge(
            target.decision_ema_bp,
            prior_decision_events,
            incoming.decision_ema_bp,
            incoming.decision_events,
        );
        let outcome_ema = weighted_basis_points_merge(
            target.outcome_ema_bp,
            prior_outcome_events,
            incoming.outcome_ema_bp,
            incoming.outcome_events,
        );
        target.decision_ema_bp = decision_ema;
        target.outcome_ema_bp = outcome_ema;
    }
}

fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
fn weighted_basis_points_merge(lhs_bp: u16, lhs_weight: u64, rhs_bp: u16, rhs_weight: u64) -> u16 {
    if lhs_weight == 0 {
        return rhs_bp;
    }
    if rhs_weight == 0 {
        return lhs_bp;
    }
    let lhs = lhs_bp as u128 * lhs_weight as u128;
    let rhs = rhs_bp as u128 * rhs_weight as u128;
    let total_weight = lhs_weight as u128 + rhs_weight as u128;
    ((lhs + rhs) / total_weight).min(u16::MAX as u128) as u16
}

#[cfg(test)]
mod tests {
    use crate::graph::types::GraphEdge;
    use crate::graph::types::GraphEdgeKind;
    use crate::graph::types::GraphNode;
    use crate::graph::types::GraphNodeKind;
    use crate::graph::types::NodeMetrics;
    use crate::graph::state::ProjectGraphState;
    use crate::graph::symbol_id::SymbolId;
    use crate::graph::types::SymbolTombstone;

    #[test]
    fn upsert_edge_accumulates_weight() {
        let mut state = ProjectGraphState::new();
        let id_a = SymbolId::new("usr:a");
        let id_b = SymbolId::new("usr:b");
        state.upsert_node(GraphNode::new(
            id_a.clone(),
            "a",
            GraphNodeKind::Function,
            "src/a.cpp",
            1,
            1,
        ));
        state.upsert_node(GraphNode::new(
            id_b.clone(),
            "b",
            GraphNodeKind::Function,
            "src/b.cpp",
            1,
            1,
        ));

        state.upsert_edge(GraphEdge::new(
            id_a.clone(),
            id_b.clone(),
            GraphEdgeKind::Reference,
        ));
        state.upsert_edge(GraphEdge::new(id_a, id_b, GraphEdgeKind::Reference));

        assert_eq!(state.edges.len(), 1);
        assert_eq!(state.edges[0].weight, 2);
    }

    #[test]
    fn compact_prunes_stale_nodes_edges_and_metrics() {
        let mut state = ProjectGraphState::new();
        let file_id = SymbolId::new("file|src/demo.cpp");
        let fresh = SymbolId::new("usr:fresh");
        let stale = SymbolId::new("usr:stale");

        let mut file_node = GraphNode::new(
            file_id.clone(),
            "src/demo.cpp",
            GraphNodeKind::File,
            "src/demo.cpp",
            0,
            0,
        );
        file_node.last_seen_unix_ms = 9_600;
        state.upsert_node(file_node);

        let mut fresh_node = GraphNode::new(
            fresh.clone(),
            "Fresh",
            GraphNodeKind::Function,
            "src/demo.cpp",
            10,
            1,
        );
        fresh_node.last_seen_unix_ms = 9_500;
        state.upsert_node(fresh_node);

        let mut stale_node = GraphNode::new(
            stale.clone(),
            "Stale",
            GraphNodeKind::Function,
            "src/demo.cpp",
            20,
            1,
        );
        stale_node.last_seen_unix_ms = 1_000;
        state.upsert_node(stale_node);

        let mut fresh_edge =
            GraphEdge::new(file_id.clone(), fresh.clone(), GraphEdgeKind::Contains);
        fresh_edge.last_seen_unix_ms = 9_700;
        state.upsert_edge(fresh_edge);
        let mut stale_edge =
            GraphEdge::new(file_id.clone(), stale.clone(), GraphEdgeKind::Contains);
        stale_edge.last_seen_unix_ms = 1_000;
        state.upsert_edge(stale_edge);

        state.set_metrics(
            fresh.clone(),
            NodeMetrics {
                reference_count: 8,
                file_count: 2,
                consensus_score: 0.9,
                last_updated_unix_ms: 9_600,
            },
        );
        state.set_metrics(
            stale.clone(),
            NodeMetrics {
                reference_count: 3,
                file_count: 1,
                consensus_score: 0.7,
                last_updated_unix_ms: 1_000,
            },
        );

        state.compact(10_000, 2_000, 100, 100, true, 90_000);

        assert!(state.node(&fresh).is_some());
        assert!(state.node(&stale).is_none());
        assert_eq!(state.edges.len(), 1);
        assert_eq!(state.symbol_file_count(&fresh), 1);
        assert!(state.node_metrics(&stale).is_none());
        assert!(state.tombstones.contains_key(&stale));
    }

    #[test]
    fn compact_respects_node_and_edge_caps() {
        let mut state = ProjectGraphState::new();
        let file_id = SymbolId::new("file|src/demo.cpp");
        let mut file_node = GraphNode::new(
            file_id.clone(),
            "src/demo.cpp",
            GraphNodeKind::File,
            "src/demo.cpp",
            0,
            0,
        );
        file_node.last_seen_unix_ms = 500;
        state.upsert_node(file_node);

        for index in 0..4usize {
            let symbol = SymbolId::new(format!("usr:symbol:{index}"));
            let mut node = GraphNode::new(
                symbol.clone(),
                format!("Sym{index}"),
                GraphNodeKind::Function,
                "src/demo.cpp",
                10 + index,
                1,
            );
            node.last_seen_unix_ms = 100 + index as u64;
            state.upsert_node(node);

            let mut edge = GraphEdge::new(file_id.clone(), symbol.clone(), GraphEdgeKind::Contains);
            edge.last_seen_unix_ms = 100 + (index as u64 * 100);
            state.upsert_edge(edge);

            state.set_metrics(
                symbol,
                NodeMetrics {
                    reference_count: index as u64,
                    file_count: 1,
                    consensus_score: 0.8,
                    last_updated_unix_ms: 600,
                },
            );
        }

        state.compact(1_000, 10_000, 3, 2, true, 90_000);

        assert_eq!(state.edges.len(), 2);
        assert_eq!(state.nodes.len(), 3);
        assert_eq!(state.metrics.len(), 2);
    }

    #[test]
    fn symbol_signal_falls_back_to_tombstone_with_decay() {
        let mut state = ProjectGraphState::new();
        let symbol = SymbolId::new("usr:stale");
        state.tombstones.insert(
            symbol.clone(),
            SymbolTombstone {
                removed_unix_ms: 1_000,
                reference_count: 100,
                file_count: 4,
                consensus_score: 0.8,
            },
        );

        let signal = state
            .symbol_project_signal(&symbol, 1_000, 10_000)
            .expect("signal");
        assert!(signal.from_tombstone);
        assert!(signal.reference_count > 0);
        assert!(signal.consensus_score > 0.0);
    }

    #[test]
    fn records_and_exports_convergence_pairs() {
        let mut state = ProjectGraphState::new();
        state.record_convergence_pair("naming_conventions", "clang_format", 3);
        state.record_convergence_pair("naming_conventions", "clang_format", 2);
        state.record_convergence_pair("include_order", "section_title_normalizer", 1);

        assert_eq!(
            state.convergence_pair_count("naming_conventions", "clang_format"),
            5
        );
        assert_eq!(
            state.convergence_pair_count("include_order", "section_title_normalizer"),
            1
        );

    }

    #[test]
    fn convergence_decay_initializes_timestamp_without_first_pass_loss() {
        let mut state = ProjectGraphState::new();
        state.record_convergence_pair_at("naming_conventions", "clang_format", 10, 1_000);

        state.decay_convergence_pairs(1_000, 1_000, 1);
        assert_eq!(
            state.convergence_pair_count("naming_conventions", "clang_format"),
            10
        );
        assert_eq!(
            state
                .convergence_pair_last_seen_unix_ms
                .get("naming_conventions|clang_format")
                .copied(),
            Some(1_000)
        );
    }

    #[test]
    fn convergence_decay_applies_half_life_and_prunes_low_counts() {
        let mut state = ProjectGraphState::new();
        state.record_convergence_pair_at("naming_conventions", "clang_format", 3, 1_000);

        state.decay_convergence_pairs(2_000, 1_000, 2);
        assert_eq!(
            state.convergence_pair_count("naming_conventions", "clang_format"),
            2
        );

        state.decay_convergence_pairs(3_000, 1_000, 2);
        assert_eq!(
            state.convergence_pair_count("naming_conventions", "clang_format"),
            0
        );
    }

    #[test]
    fn records_policy_cluster_learning_entries() {
        let mut state = ProjectGraphState::new();
        state.record_policy_cluster_learning(
            "naming_conventions",
            42,
            &super::PolicyClusterLearningStats {
                decisions: 4,
                apply: 3,
                accepted: 2,
                decision_ema_bp: 8_500,
                outcome_ema_bp: 8_200,
                decision_events: 4,
                outcome_events: 2,
                ..super::PolicyClusterLearningStats::default()
            },
        );
        state.record_policy_cluster_learning(
            "naming_conventions",
            42,
            &super::PolicyClusterLearningStats {
                decisions: 2,
                block: 1,
                regressed: 1,
                decision_ema_bp: 2_000,
                outcome_ema_bp: 3_000,
                decision_events: 2,
                outcome_events: 1,
                ..super::PolicyClusterLearningStats::default()
            },
        );

        let snapshot = state.policy_cluster_learning_snapshot();
        assert_eq!(snapshot.len(), 1);
        let stats = &snapshot[0].stats;
        assert_eq!(stats.decisions, 6);
        assert_eq!(stats.apply, 3);
        assert_eq!(stats.block, 1);
        assert_eq!(stats.regressed, 1);
        assert_eq!(stats.decision_events, 6);
        assert_eq!(stats.outcome_events, 3);
        assert!(stats.decision_ema_bp >= 6_000);
        assert!(stats.outcome_ema_bp >= 6_000);
    }

    #[test]
    fn replace_policy_cluster_learning_entries_overwrites_previous_snapshot() {
        let mut state = ProjectGraphState::new();
        state.record_policy_cluster_learning(
            "naming_conventions",
            42,
            &super::PolicyClusterLearningStats {
                decisions: 3,
                ..super::PolicyClusterLearningStats::default()
            },
        );
        state.replace_policy_cluster_learning_entries(&[
            super::PolicyClusterLearningSnapshotEntry {
                policy: "include_order".into(),
                cluster: 7,
                stats: super::PolicyClusterLearningStats {
                    decisions: 2,
                    apply: 1,
                    decision_events: 2,
                    decision_ema_bp: 7_200,
                    ..super::PolicyClusterLearningStats::default()
                },
            },
        ]);

        let snapshot = state.policy_cluster_learning_snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].policy, "include_order");
        assert_eq!(snapshot[0].cluster, 7);
        assert_eq!(snapshot[0].stats.decisions, 2);
        assert_eq!(snapshot[0].stats.decision_events, 2);
    }

    #[test]
    fn records_retry_learning_and_culprit_pairs() {
        let mut state = ProjectGraphState::new();
        state.record_retry_strategy_learning("block_culprit", "semantic", 3, 2);
        state.record_retry_strategy_learning("block_culprit", "semantic", 1, 1);
        state.record_retry_culprit_pair("naming_conventions", "clang_format", 2);
        state.record_retry_culprit_pair("naming_conventions", "clang_format", 3);

        let retry_snapshot = state.retry_strategy_learning_snapshot();
        assert_eq!(retry_snapshot.len(), 1);
        let (_, _, stats) = &retry_snapshot[0];
        assert_eq!(stats.attempts, 4);
        assert_eq!(stats.successes, 3);

        let pair_snapshot = state.retry_culprit_pairs_snapshot();
        assert_eq!(pair_snapshot.len(), 1);
        assert_eq!(pair_snapshot[0].count, 5);
    }
}
