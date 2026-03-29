use std::cmp::Ordering;
use std::collections::BTreeMap;

use rustc_hash::{FxHashMap, FxHashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(test)]
use std::time::{SystemTime, UNIX_EPOCH};

use crate::graph::types::GraphEdgeKind;
#[cfg(test)]
use crate::graph::types::GraphNode;
use crate::graph::types::GRAPH_NODE_KIND_FILE;
use crate::graph::types::NodeMetrics;
use crate::graph::state::ProjectGraphState;
#[cfg(test)]
use crate::graph::types::ProjectSignal;
use crate::graph::symbol_bucket::file_symbol_id;
use crate::graph::symbol_id::SymbolId;

#[derive(Clone, Debug)]
pub struct ProjectGraphSnapshot {
    state: Arc<ProjectGraphState>,
}

impl ProjectGraphSnapshot {
    #[cfg(test)]
    pub fn new(state: Arc<ProjectGraphState>) -> Self {
        Self { state }
    }

    pub fn from_state(state: Arc<ProjectGraphState>) -> Self {
        Self { state }
    }

    #[cfg(test)]
    pub fn state(&self) -> &ProjectGraphState {
        self.state.as_ref()
    }

    pub fn to_state_clone(&self) -> ProjectGraphState {
        self.state.as_ref().clone()
    }

    #[cfg(test)]
    pub fn node(&self, symbol_id: &SymbolId) -> Option<&GraphNode> {
        self.state.node(symbol_id)
    }

    #[cfg(test)]
    pub fn project_signal(&self, symbol_id: &SymbolId) -> Option<ProjectSignal> {
        self.state
            .symbol_project_signal(symbol_id, current_unix_ms(), 0)
    }

    pub fn affected_file_paths(
        &self,
        changed_files: &[PathBuf],
        hops: usize,
        max_files: usize,
    ) -> Vec<PathBuf> {
        if changed_files.is_empty() || hops == 0 || max_files == 0 {
            return Vec::new();
        }
        let frontier_cap = max_files.saturating_mul(8).clamp(64usize, 4_096usize);
        let per_file_symbol_cap = max_files.saturating_mul(4).clamp(24usize, 768usize);
        let per_symbol_file_cap = max_files.saturating_mul(3).clamp(32usize, 1_024usize);

        let mut changed_file_ids: FxHashSet<SymbolId> = FxHashSet::default();
        let mut changed_path_keys: FxHashSet<String> = FxHashSet::default();
        for path in changed_files {
            changed_file_ids.insert(file_symbol_id(path.as_path()));
            changed_path_keys.insert(Self::path_key(path.as_path()));
            if let Ok(canonical) = std::fs::canonicalize(path) {
                changed_file_ids.insert(file_symbol_id(canonical.as_path()));
                changed_path_keys.insert(Self::path_key(canonical.as_path()));
            }
        }
        for (symbol_id, node) in &self.state.nodes {
            if node.kind != GRAPH_NODE_KIND_FILE || node.file_path.is_empty() {
                continue;
            }
            let node_key = Self::path_key(Path::new(node.file_path.as_str()));
            if changed_path_keys.contains(node_key.as_str()) {
                changed_file_ids.insert(symbol_id.clone());
            }
        }
        if changed_file_ids.is_empty() {
            return Vec::new();
        }

        let mut file_to_symbols: FxHashMap<SymbolId, Vec<(SymbolId, u32)>> = FxHashMap::default();
        let mut symbol_to_files: FxHashMap<SymbolId, Vec<(SymbolId, u32)>> = FxHashMap::default();
        for edge in &self.state.edges {
            if edge.kind != GraphEdgeKind::Contains && edge.kind != GraphEdgeKind::Reference {
                continue;
            }
            let Some(from_node) = self.state.nodes.get(&edge.from) else {
                continue;
            };
            if from_node.kind != GRAPH_NODE_KIND_FILE {
                continue;
            }
            let weight = edge.weight.max(1);
            file_to_symbols
                .entry(edge.from.clone())
                .or_default()
                .push((edge.to.clone(), weight));
            symbol_to_files
                .entry(edge.to.clone())
                .or_default()
                .push((edge.from.clone(), weight));
        }
        for links in file_to_symbols.values_mut() {
            Self::sort_and_dedup_weighted_links(links);
        }
        for links in symbol_to_files.values_mut() {
            Self::sort_and_dedup_weighted_links(links);
        }

        let mut visited = changed_file_ids.clone();
        let mut frontier = changed_file_ids.into_iter().collect::<Vec<_>>();
        let mut file_scores: FxHashMap<SymbolId, f64> = FxHashMap::default();
        let mut file_hops: FxHashMap<SymbolId, usize> = FxHashMap::default();
        for file_id in &frontier {
            file_scores.insert(file_id.clone(), 1.0);
            file_hops.insert(file_id.clone(), 0);
        }

        for hop in 1..=hops {
            if frontier.is_empty() {
                break;
            }
            let mut next_scores: FxHashMap<SymbolId, f64> = FxHashMap::default();
            for file_id in &frontier {
                let base_score = file_scores.get(file_id).copied().unwrap_or(1.0);
                let Some(symbol_links) = file_to_symbols.get(file_id) else {
                    continue;
                };
                for (symbol_id, file_symbol_weight) in symbol_links.iter().take(per_file_symbol_cap)
                {
                    let Some(candidate_files) = symbol_to_files.get(symbol_id) else {
                        continue;
                    };
                    let quality = Self::symbol_neighbor_quality(
                        self.state.metrics.get(symbol_id),
                        candidate_files.len(),
                    );
                    let fanout_penalty =
                        (1.0 / (candidate_files.len().max(1) as f64).sqrt()).clamp(0.06, 1.0);
                    let mut emitted = 0usize;
                    for (candidate_file_id, symbol_file_weight) in candidate_files {
                        if visited.contains(candidate_file_id) {
                            continue;
                        }
                        let edge_weight = ((*file_symbol_weight as u64 + *symbol_file_weight as u64)
                            as f64)
                            * 0.5;
                        let next_score = base_score + edge_weight * quality * fanout_penalty;
                        next_scores
                            .entry(candidate_file_id.clone())
                            .and_modify(|value| *value = value.max(next_score))
                            .or_insert(next_score);
                        emitted = emitted.saturating_add(1);
                        if emitted >= per_symbol_file_cap {
                            break;
                        }
                    }
                }
            }
            if next_scores.is_empty() {
                break;
            }
            let mut ranked = next_scores.into_iter().collect::<Vec<_>>();
            ranked.sort_by(|(left_id, left_score), (right_id, right_score)| {
                right_score
                    .partial_cmp(left_score)
                    .unwrap_or(Ordering::Equal)
                    .then_with(|| left_id.cmp(right_id))
            });
            if ranked.len() > frontier_cap {
                ranked.truncate(frontier_cap);
            }
            frontier.clear();
            for (candidate_file_id, score) in ranked {
                if visited.insert(candidate_file_id.clone()) {
                    file_scores.insert(candidate_file_id.clone(), score);
                    file_hops.insert(candidate_file_id.clone(), hop);
                    frontier.push(candidate_file_id);
                }
            }
        }

        let mut impacted_ranked = Vec::<(PathBuf, usize, f64, String)>::new();
        for file_id in visited {
            let Some(node) = self.state.nodes.get(&file_id) else {
                continue;
            };
            if node.kind != GRAPH_NODE_KIND_FILE || node.file_path.is_empty() {
                continue;
            }
            let key = Self::path_key(Path::new(node.file_path.as_str()));
            if changed_path_keys.contains(key.as_str()) {
                continue;
            }
            impacted_ranked.push((
                PathBuf::from(node.file_path.as_str()),
                file_hops
                    .get(&file_id)
                    .copied()
                    .unwrap_or(hops.saturating_add(1)),
                file_scores.get(&file_id).copied().unwrap_or(0.0),
                key,
            ));
        }
        impacted_ranked.sort_by(|left, right| {
            left.1
                .cmp(&right.1)
                .then_with(|| right.2.partial_cmp(&left.2).unwrap_or(Ordering::Equal))
                .then_with(|| left.3.cmp(&right.3))
        });
        impacted_ranked
            .into_iter()
            .take(max_files)
            .map(|(path, _, _, _)| path)
            .collect()
    }

    fn sort_and_dedup_weighted_links(links: &mut Vec<(SymbolId, u32)>) {
        if links.is_empty() {
            return;
        }
        let mut merged = BTreeMap::<SymbolId, u32>::new();
        for (symbol_id, weight) in links.drain(..) {
            let entry = merged.entry(symbol_id).or_insert(0);
            *entry = (*entry).max(weight.max(1));
        }
        let mut normalized = merged.into_iter().collect::<Vec<_>>();
        normalized.sort_by(|(left_id, left_weight), (right_id, right_weight)| {
            right_weight
                .cmp(left_weight)
                .then_with(|| left_id.cmp(right_id))
        });
        *links = normalized;
    }

    fn symbol_neighbor_quality(metrics: Option<&NodeMetrics>, fanout: usize) -> f64 {
        let Some(metrics) = metrics else {
            return (1.0 / (fanout.max(1) as f64).sqrt()).clamp(0.08, 0.60);
        };
        let consensus = metrics.consensus_score.clamp(0.0, 1.0);
        let consensus_factor = (0.35 + consensus * 0.65).clamp(0.20, 1.00);
        let evidence_factor = ((metrics.reference_count as f64).ln_1p() / 5.0).clamp(0.25, 1.30);
        let spread = metrics.file_count.max(1) as f64;
        let spread_factor = (1.0 / spread.sqrt()).clamp(0.05, 1.0);
        let fanout_factor = (1.0 / (fanout.max(1) as f64).sqrt()).clamp(0.05, 1.0);
        let global_penalty = spread_factor.min(fanout_factor);
        (consensus_factor * evidence_factor * (0.35 + 0.65 * global_penalty)).clamp(0.05, 1.30)
    }

    fn path_key(path: &Path) -> String {
        std::fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .replace('\\', "/")
    }

    #[cfg(test)]
    pub fn node_count(&self) -> usize {
        self.state.nodes.len()
    }

    #[cfg(test)]
    pub fn edge_count(&self) -> usize {
        self.state.edges.len()
    }
}

#[cfg(test)]
fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::graph::types::GraphEdge;
    use crate::graph::types::GraphEdgeKind;
    use crate::graph::types::GraphNode;
    use crate::graph::types::GRAPH_NODE_KIND_FILE;
    use crate::graph::types::NodeMetrics;
    use crate::graph::snapshot::ProjectGraphSnapshot;
    use crate::graph::state::ProjectGraphState;
    use crate::graph::symbol_bucket::file_symbol_id;
    use crate::graph::symbol_id::SymbolId;

    #[test]
    fn returns_ref_neighbors() {
        let mut state = ProjectGraphState::new();
        let file_a = std::path::PathBuf::from("a.hpp");
        let file_b = std::path::PathBuf::from("b.cpp");
        let file_c = std::path::PathBuf::from("c.cpp");
        let file_a_id = file_symbol_id(file_a.as_path());
        let file_b_id = file_symbol_id(file_b.as_path());
        let file_c_id = file_symbol_id(file_c.as_path());
        let symbol_shared = SymbolId::new("usr|c:@S@Shared");
        let symbol_other = SymbolId::new("usr|c:@S@Other");

        state.upsert_node(GraphNode::new(
            file_a_id.clone(),
            "a.hpp",
            GRAPH_NODE_KIND_FILE,
            file_a.to_string_lossy(),
            0,
            0,
        ));
        state.upsert_node(GraphNode::new(
            file_b_id.clone(),
            "b.cpp",
            GRAPH_NODE_KIND_FILE,
            file_b.to_string_lossy(),
            0,
            0,
        ));
        state.upsert_node(GraphNode::new(
            file_c_id.clone(),
            "c.cpp",
            GRAPH_NODE_KIND_FILE,
            file_c.to_string_lossy(),
            0,
            0,
        ));

        state.upsert_edge(GraphEdge::new(
            file_a_id.clone(),
            symbol_shared.clone(),
            GraphEdgeKind::Contains,
        ));
        state.upsert_edge(GraphEdge::new(
            file_b_id.clone(),
            symbol_shared.clone(),
            GraphEdgeKind::Reference,
        ));
        state.upsert_edge(GraphEdge::new(
            file_c_id.clone(),
            symbol_other.clone(),
            GraphEdgeKind::Reference,
        ));

        let snapshot = ProjectGraphSnapshot::new(Arc::new(state));
        let impacted = snapshot.affected_file_paths(std::slice::from_ref(&file_a), 1, 16);
        assert!(impacted.contains(&file_b));
        assert!(!impacted.contains(&file_a));
        assert!(!impacted.contains(&file_c));
    }

    #[test]
    fn respects_max_files() {
        let mut state = ProjectGraphState::new();
        let changed = std::path::PathBuf::from("changed.hpp");
        let changed_id = file_symbol_id(changed.as_path());
        let symbol = SymbolId::new("usr|c:@S@Shared");
        state.upsert_node(GraphNode::new(
            changed_id.clone(),
            "changed.hpp",
            GRAPH_NODE_KIND_FILE,
            changed.to_string_lossy(),
            0,
            0,
        ));
        state.upsert_edge(GraphEdge::new(
            changed_id.clone(),
            symbol.clone(),
            GraphEdgeKind::Contains,
        ));
        for index in 0..6 {
            let path = std::path::PathBuf::from(format!("neighbor_{index}.cpp"));
            let file_id = file_symbol_id(path.as_path());
            state.upsert_node(GraphNode::new(
                file_id.clone(),
                path.to_string_lossy(),
                GRAPH_NODE_KIND_FILE,
                path.to_string_lossy(),
                0,
                0,
            ));
            state.upsert_edge(GraphEdge::new(
                file_id,
                symbol.clone(),
                GraphEdgeKind::Reference,
            ));
        }
        let snapshot = ProjectGraphSnapshot::new(Arc::new(state));
        let impacted = snapshot.affected_file_paths(&[changed], 1, 3);
        assert_eq!(impacted.len(), 3);
    }

    #[test]
    fn prefers_tight_consensus() {
        let mut state = ProjectGraphState::new();
        let changed = std::path::PathBuf::from("changed.hpp");
        let changed_id = file_symbol_id(changed.as_path());
        let focused_neighbor = std::path::PathBuf::from("focused.cpp");
        let focused_id = file_symbol_id(focused_neighbor.as_path());
        let focused_symbol = SymbolId::new("usr|c:@S@Focused");
        let noisy_symbol = SymbolId::new("usr|c:@S@Noisy");

        state.upsert_node(GraphNode::new(
            changed_id.clone(),
            "changed.hpp",
            GRAPH_NODE_KIND_FILE,
            changed.to_string_lossy(),
            0,
            0,
        ));
        state.upsert_node(GraphNode::new(
            focused_id.clone(),
            "focused.cpp",
            GRAPH_NODE_KIND_FILE,
            focused_neighbor.to_string_lossy(),
            0,
            0,
        ));
        state.upsert_edge(GraphEdge::new(
            changed_id.clone(),
            focused_symbol.clone(),
            GraphEdgeKind::Contains,
        ));
        state.upsert_edge(GraphEdge::new(
            focused_id,
            focused_symbol.clone(),
            GraphEdgeKind::Reference,
        ));
        state.set_metrics(
            focused_symbol.clone(),
            NodeMetrics {
                reference_count: 18,
                file_count: 2,
                consensus_score: 0.95,
                last_updated_unix_ms: 0,
            },
        );

        state.upsert_edge(GraphEdge::new(
            changed_id.clone(),
            noisy_symbol.clone(),
            GraphEdgeKind::Contains,
        ));
        for index in 0..32 {
            let noisy_path = std::path::PathBuf::from(format!("noisy_{index}.cpp"));
            let noisy_id = file_symbol_id(noisy_path.as_path());
            state.upsert_node(GraphNode::new(
                noisy_id.clone(),
                noisy_path.to_string_lossy(),
                GRAPH_NODE_KIND_FILE,
                noisy_path.to_string_lossy(),
                0,
                0,
            ));
            state.upsert_edge(GraphEdge::new(
                noisy_id,
                noisy_symbol.clone(),
                GraphEdgeKind::Reference,
            ));
        }
        state.set_metrics(
            noisy_symbol,
            NodeMetrics {
                reference_count: 2_000,
                file_count: 33,
                consensus_score: 0.40,
                last_updated_unix_ms: 0,
            },
        );

        let snapshot = ProjectGraphSnapshot::new(Arc::new(state));
        let impacted = snapshot.affected_file_paths(&[changed], 1, 4);
        assert!(impacted.contains(&focused_neighbor));
    }
}
