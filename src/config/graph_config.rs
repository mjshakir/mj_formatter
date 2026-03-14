use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct ProjectGraphConfig {
    pub enabled: bool,
    pub path: PathBuf,
    pub prune_enabled: bool,
    pub retention_days: u32,
    pub max_nodes: usize,
    pub max_edges: usize,
    pub tombstone_enabled: bool,
    pub tombstone_retention_days: u32,
    pub tombstone_decay_days: u32,
    pub convergence_decay_enabled: bool,
    pub convergence_decay_half_life_days: u32,
    pub convergence_decay_min_count: u64,
    pub incremental_neighborhood_enabled: bool,
    pub incremental_neighborhood_hops: usize,
    pub incremental_neighborhood_max_files: usize,
}

impl Default for ProjectGraphConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: PathBuf::from("var/cache/graph_state.bin"),
            prune_enabled: true,
            retention_days: 30,
            max_nodes: 250_000,
            max_edges: 1_000_000,
            tombstone_enabled: true,
            tombstone_retention_days: 90,
            tombstone_decay_days: 30,
            convergence_decay_enabled: true,
            convergence_decay_half_life_days: 30,
            convergence_decay_min_count: 1,
            incremental_neighborhood_enabled: true,
            incremental_neighborhood_hops: 1,
            incremental_neighborhood_max_files: 256,
        }
    }
}
