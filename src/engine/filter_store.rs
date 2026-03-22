#![allow(clippy::needless_range_loop)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use arc_swap::ArcSwap;
use dashmap::DashMap;
use crate::engine::adaptive_rules::AdaptiveRuleBases;
use crate::engine::certainty_filter::{CertaintyFilterResult, CertaintyFilterState, NUM_DIMS};
use crate::engine::fuzzy_inference::AdaptiveFiringRecord;
use crate::engine::mat5::{Mat5, mat5_add, mat5_diagonal, mat5_identity, mat5_inverse_spd, mat5_mul, mat5_matvec, mat5_sub, vec5_sub};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ProjectFilter {
    pub estimate: [f64; NUM_DIMS],
    pub covariance: Mat5,
    pub observation_count: u64,
}

impl ProjectFilter {
    pub fn new() -> Self {
        Self {
            estimate: [0.5; NUM_DIMS],
            covariance: mat5_diagonal(&[0.25; NUM_DIMS]),
            observation_count: 0,
        }
    }

    pub fn update(&mut self, file_estimate: &[f64; NUM_DIMS], file_covariance: &Mat5) {
        let inter_file_noise = mat5_diagonal(&[0.02; NUM_DIMS]);
        let r = mat5_add(file_covariance, &inter_file_noise);

        let s = mat5_add(&self.covariance, &r);
        let s_inv = mat5_inverse_spd(&s).unwrap_or(mat5_identity());
        let k = mat5_mul(&self.covariance, &s_inv);

        let innov = vec5_sub(file_estimate, &self.estimate);
        let k_innov = mat5_matvec(&k, &innov);
        for d in 0..NUM_DIMS {
            self.estimate[d] = (self.estimate[d] + k_innov[d]).clamp(0.0, 1.0);
        }

        let i_k = mat5_sub(&mat5_identity(), &k);
        self.covariance = mat5_mul(&i_k, &self.covariance);

        let q_project = mat5_diagonal(&[0.005; NUM_DIMS]);
        self.covariance = mat5_add(&self.covariance, &q_project);

        self.observation_count += 1;
    }

    pub fn new_file_prior(&self) -> Option<([f64; NUM_DIMS], [f64; NUM_DIMS], u32)> {
        if self.observation_count < 3 {
            return None;
        }
        let widened = mat5_add(&self.covariance, &mat5_diagonal(&[0.03; NUM_DIMS]));
        let variances = [
            widened[0][0],
            widened[1][1],
            widened[2][2],
            widened[3][3],
            widened[4][4],
        ];
        Some((self.estimate, variances, (self.observation_count as u32).min(5)))
    }
}

pub struct CertaintyFilterStore {
    entries: DashMap<String, CertaintyFilterState>,
    population_priors: ArcSwap<Option<([f64; NUM_DIMS], [f64; NUM_DIMS], u32)>>,
    project_filter: Mutex<ProjectFilter>,
    adaptive_rules: Mutex<AdaptiveRuleBases>,
}

impl CertaintyFilterStore {
    pub fn new(
        population_context: Option<&crate::engine::population_context::PopulationContext>,
    ) -> Self {
        let population_priors = population_context.map(|ctx| {
            (ctx.prior_estimates, ctx.prior_variances, ctx.prior_observation_count)
        });
        Self {
            entries: DashMap::new(),
            population_priors: ArcSwap::from_pointee(population_priors),
            project_filter: Mutex::new(ProjectFilter::new()),
            adaptive_rules: Mutex::new(AdaptiveRuleBases::new()),
        }
    }

    pub fn save_to_path(&self, path: &Path) -> anyhow::Result<()> {
        let map: HashMap<String, CertaintyFilterState> = self
            .entries
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect();
        let bytes = postcard::to_allocvec(&map)
            .map_err(|e| anyhow::anyhow!("postcard encode: {}", e))?;
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
        let map: HashMap<String, CertaintyFilterState> =
            postcard::from_bytes(&bytes).ok()?;
        if map.is_empty() {
            return None;
        }
        let population_priors = population_context.map(|ctx| {
            (ctx.prior_estimates, ctx.prior_variances, ctx.prior_observation_count)
        });
        let entries = DashMap::new();
        for (k, v) in map {
            entries.insert(k, v);
        }
        let adaptive_rules = path
            .with_extension("adaptive_rules.bin")
            .exists()
            .then(|| {
                let bytes = std::fs::read(path.with_extension("adaptive_rules.bin")).ok()?;
                let rules: AdaptiveRuleBases =
                    postcard::from_bytes(&bytes).ok()?;
                Some(rules)
            })
            .flatten()
            .unwrap_or_else(AdaptiveRuleBases::new);
        Some(Self {
            entries,
            population_priors: ArcSwap::from_pointee(population_priors),
            project_filter: Mutex::new(ProjectFilter::new()),
            adaptive_rules: Mutex::new(adaptive_rules),
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
        let priors = **self.population_priors.load();
        let project_prior = self.project_filter.lock().ok().and_then(|pf| pf.new_file_prior());
        let mut entry = self
            .entries
            .entry(file_path.to_string())
            .or_insert_with(|| {
                if let Some((est, var, obs)) = project_prior {
                    CertaintyFilterState::new_with_prior(est, var, obs)
                } else if let Some((est, var, obs)) = priors {
                    CertaintyFilterState::new_with_prior(est, var, obs)
                } else {
                    CertaintyFilterState::new()
                }
            });
        let result = entry.observe(measurement, content_hash);

        if result.observation_count >= 3 {
            if let Ok(mut pf) = self.project_filter.lock() {
                pf.update(&result.estimates, &result.covariance);
            }
        }

        result
    }

    pub fn last_edit_outcome(&self, file_path: &str) -> Option<f64> {
        self.entries.get(file_path).and_then(|e| e.last_edit_outcome)
    }

    pub fn edit_estimate(&self, file_path: &str) -> f64 {
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

    pub fn adaptive_rules(&self) -> MutexGuard<'_, AdaptiveRuleBases> {
        self.adaptive_rules.lock().expect("adaptive_rules lock poisoned")
    }

    pub fn update_adaptive_rules(&self, record: &AdaptiveFiringRecord, outcome: f64) {
        if let Ok(mut rules) = self.adaptive_rules.lock() {
            rules.failure_severity.update(&record.severity_firing, record.severity_value);
            rules.edit_acceptance.update(&record.acceptance_firing, record.acceptance_value);
            rules.edit_outcome.update(&record.outcome_firing, outcome);
        }
    }
}
