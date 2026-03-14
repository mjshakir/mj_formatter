use std::collections::HashMap;
use std::path::Path;

use dashmap::DashMap;
use crate::engine::certainty_filter::{CertaintyFilterResult, CertaintyFilterState, NUM_DIMS};

pub struct CertaintyFilterStore {
    entries: DashMap<String, CertaintyFilterState>,
    population_priors: Option<([f64; NUM_DIMS], [f64; NUM_DIMS])>,
}

impl CertaintyFilterStore {
    pub fn new(
        population_context: Option<&crate::engine::population_context::PopulationContext>,
    ) -> Self {
        let population_priors = population_context.map(|ctx| {
            (ctx.prior_estimates, ctx.prior_variances)
        });
        Self {
            entries: DashMap::new(),
            population_priors,
        }
    }

    pub fn save_to_path(&self, path: &Path) -> anyhow::Result<()> {
        let map: HashMap<String, CertaintyFilterState> = self
            .entries
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect();
        let bytes = bincode::serde::encode_to_vec(&map, bincode::config::standard())
            .map_err(|e| anyhow::anyhow!("bincode encode: {}", e))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, bytes)?;
        Ok(())
    }

    pub fn load_from_path(
        path: &Path,
        population_context: Option<&crate::engine::population_context::PopulationContext>,
    ) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        let (map, _): (HashMap<String, CertaintyFilterState>, _) =
            bincode::serde::decode_from_slice(&bytes, bincode::config::standard()).ok()?;
        if map.is_empty() {
            return None;
        }
        let population_priors = population_context.map(|ctx| {
            (ctx.prior_estimates, ctx.prior_variances)
        });
        let entries = DashMap::new();
        for (k, v) in map {
            entries.insert(k, v);
        }
        Some(Self {
            entries,
            population_priors,
        })
    }

    pub fn has_sufficient_observations(&self, min_count: u32) -> bool {
        let total = self.entries.len();
        if total == 0 {
            return false;
        }
        let sufficient = self
            .entries
            .iter()
            .filter(|e| e.value().observation_count >= min_count)
            .count();
        sufficient * 2 >= total
    }

    pub fn extract_measurements(&self) -> Vec<[f64; NUM_DIMS]> {
        self.entries
            .iter()
            .filter(|e| e.value().observation_count > 0)
            .map(|e| e.value().models[0].estimates)
            .collect()
    }

    pub fn observe(
        &self,
        file_path: &str,
        measurement: [f64; NUM_DIMS],
        content_hash: u64,
    ) -> CertaintyFilterResult {
        let priors = self.population_priors;
        let mut entry = self
            .entries
            .entry(file_path.to_string())
            .or_insert_with(|| {
                if let Some((est, var)) = priors {
                    CertaintyFilterState::new_with_prior(est, var)
                } else {
                    CertaintyFilterState::new()
                }
            });
        entry.observe(measurement, content_hash)
    }

    pub fn last_edit_outcome(&self, file_path: &str) -> Option<f64> {
        self.entries.get(file_path).and_then(|e| e.last_edit_outcome)
    }

    pub fn current_edit_success_estimate(&self, file_path: &str) -> f64 {
        self.entries
            .get(file_path)
            .map(|e| {
                if e.observation_count == 0 {
                    0.50
                } else {
                    e.models[0].estimates[4]
                }
            })
            .unwrap_or(0.50)
    }

    pub fn record_edit_outcome(&self, file_path: &str, outcome: f64) {
        if let Some(mut entry) = self.entries.get_mut(file_path) {
            entry.last_edit_outcome = Some(outcome);
        }
    }

    pub fn correlate_paired_observation(
        &self,
        companion_path: &str,
        source_estimates: [f64; NUM_DIMS],
        damping: f64,
    ) {
        if let Some(mut entry) = self.entries.get_mut(companion_path) {
            if entry.observation_count == 0 {
                return;
            }
            let mut blended = [0.0f64; NUM_DIMS];
            for d in 0..NUM_DIMS {
                let current = entry.models[0].estimates[d];
                blended[d] = (current + damping * (source_estimates[d] - current)).clamp(0.0, 1.0);
            }
            for model in &mut entry.models {
                model.estimates[..NUM_DIMS].copy_from_slice(&blended[..NUM_DIMS]);
            }
        }
    }
}
