use crate::graph::types::GraphShape;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProjectGraphPersistStats {
    pub generation: u64,
    pub prune_enabled: bool,
    pub tombstone_enabled: bool,
    pub before: GraphShape,
    pub after: GraphShape,
}

impl ProjectGraphPersistStats {
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
