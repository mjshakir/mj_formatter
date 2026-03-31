use std::path::Path;

use anyhow::Result;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use crate::files::codec::StateCodec;

const DEFAULT_ESTIMATE: f64 = 0.0;
const DEFAULT_VARIANCE: f64 = 0.10;
const DEFAULT_Q: f64 = 0.0001;
const DEFAULT_R: f64 = 0.02;
const Q_FLOOR: f64 = 1e-6;
const Q_CEIL: f64 = 0.05;
const R_FLOOR: f64 = 0.002;
const R_CEIL: f64 = 0.15;
const ADAPTATION_MIN_OBS: u32 = 3;
const ADAPTATION_WINDOW: f64 = 20.0;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScalarKalmanState {
    pub estimate: f64,
    pub variance: f64,
    pub adaptive_q: f64,
    pub adaptive_r: f64,
    pub observation_count: u32,
}

impl ScalarKalmanState {
    pub fn new() -> Self {
        Self {
            estimate: DEFAULT_ESTIMATE,
            variance: DEFAULT_VARIANCE,
            adaptive_q: DEFAULT_Q,
            adaptive_r: DEFAULT_R,
            observation_count: 0,
        }
    }

    pub fn stddev(&self) -> f64 {
        self.variance.max(0.0).sqrt()
    }

    pub fn observe(&mut self, measurement: f64) {
        let z = measurement.clamp(0.0, 1.0);

        if self.observation_count == 0 {
            self.estimate = z;
            self.variance = DEFAULT_R;
            self.observation_count = 1;
            return;
        }

        // Predict: P_pred = P + Q
        let p_pred = self.variance + self.adaptive_q;

        // Innovation: y = z - x
        let innovation = z - self.estimate;

        // Innovation covariance: S = P_pred + R
        let s = p_pred + self.adaptive_r;
        if s.abs() < 1e-15 {
            self.observation_count = self.observation_count.saturating_add(1);
            return;
        }

        // Kalman gain: K = P_pred / S
        let k = p_pred / s;

        // Update
        let new_estimate = (self.estimate + k * innovation).clamp(0.0, 1.0);
        let new_variance = ((1.0 - k) * p_pred).max(1e-12);

        // Sage-Husa adaptive Q/R
        if self.observation_count >= ADAPTATION_MIN_OBS {
            let alpha = 1.0 / (self.observation_count.saturating_sub(1) as f64).min(ADAPTATION_WINDOW);
            let maturity = (self.observation_count as f64 / ADAPTATION_WINDOW).min(1.0);
            let innov_sq = innovation * innovation;

            // Adaptive R
            let r_update = innov_sq - p_pred;
            self.adaptive_r = (1.0 - alpha) * self.adaptive_r + alpha * r_update;
            let r_floor = R_FLOOR * (1.0 - maturity) + R_FLOOR * maturity;
            let r_ceil = R_CEIL * (1.0 - maturity * 0.5) + R_CEIL * 0.5;
            self.adaptive_r = self.adaptive_r.clamp(r_floor, r_ceil);

            // Adaptive Q
            let k_innov_sq = (k * innovation) * (k * innovation);
            let q_update = k_innov_sq + new_variance - self.variance;
            self.adaptive_q = (1.0 - alpha) * self.adaptive_q + alpha * q_update;
            let q_floor = Q_FLOOR * (1.0 - maturity) + Q_FLOOR * maturity;
            let q_ceil = Q_CEIL * (1.0 - maturity * 0.5) + Q_CEIL * 0.5;
            self.adaptive_q = self.adaptive_q.clamp(q_floor, q_ceil);
        }

        self.estimate = new_estimate;
        self.variance = new_variance;
        self.observation_count = self.observation_count.saturating_add(1);
    }
}

impl Default for ScalarKalmanState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PolicyInterferenceState {
    pub interference: FxHashMap<String, ScalarKalmanState>,
    pub repair_need: FxHashMap<String, ScalarKalmanState>,
}

impl PolicyInterferenceState {
    pub fn new() -> Self {
        Self {
            interference: FxHashMap::default(),
            repair_need: FxHashMap::default(),
        }
    }

    pub fn interference_estimate(&self, policy_name: &str) -> f64 {
        self.interference
            .get(policy_name)
            .map_or(0.0, |s| s.estimate)
    }

    pub fn interference_stddev(&self, policy_name: &str) -> f64 {
        self.interference
            .get(policy_name)
            .map_or(DEFAULT_VARIANCE.sqrt(), |s| s.stddev())
    }

    pub fn repair_need_estimate(&self, policy_name: &str) -> f64 {
        self.repair_need
            .get(policy_name)
            .map_or(0.0, |s| s.estimate)
    }

    pub fn repair_need_stddev(&self, policy_name: &str) -> f64 {
        self.repair_need
            .get(policy_name)
            .map_or(DEFAULT_VARIANCE.sqrt(), |s| s.stddev())
    }

    pub fn observe_interference(&mut self, policy_name: &str, ratio: f64) {
        self.interference
            .entry(policy_name.to_string())
            .or_default()
            .observe(ratio);
    }

    pub fn observe_repair_need(&mut self, policy_name: &str, ratio: f64) {
        self.repair_need
            .entry(policy_name.to_string())
            .or_default()
            .observe(ratio);
    }

    pub fn dynamic_priority(
        &self,
        policy_name: &str,
        base_priority: u8,
        global_edit_success: f64,
    ) -> f64 {
        let interference = self.interference_estimate(policy_name);
        let uncertainty = self.interference_stddev(policy_name);
        let sensitivity = global_edit_success * (1.0 - uncertainty.min(1.0));
        let interference_shift = -interference * sensitivity * 30.0;
        base_priority as f64 + interference_shift
    }

    pub fn repair_weight(
        &self,
        policy_name: &str,
        global_edit_success: f64,
    ) -> f64 {
        let repair = self.repair_need_estimate(policy_name);
        let uncertainty = self.repair_need_stddev(policy_name);
        let sensitivity = global_edit_success * (1.0 - uncertainty.min(1.0));
        repair * sensitivity
    }

    #[allow(dead_code)] // called from multi-process shard aggregation path (pending)
    pub fn merge_shards(shards: &[Self]) -> Self {
        let total_policies: std::collections::HashSet<&str> = shards
            .iter()
            .flat_map(|s| {
                s.interference
                    .keys()
                    .chain(s.repair_need.keys())
                    .map(|k| k.as_str())
            })
            .collect();

        let mut merged = Self::new();
        for policy in &total_policies {
            let int_shards: Vec<&ScalarKalmanState> = shards
                .iter()
                .filter_map(|s| s.interference.get(*policy))
                .collect();
            if !int_shards.is_empty() {
                merged.interference.insert(
                    policy.to_string(),
                    Self::merge_scalar_shards(&int_shards),
                );
            }

            let rep_shards: Vec<&ScalarKalmanState> = shards
                .iter()
                .filter_map(|s| s.repair_need.get(*policy))
                .collect();
            if !rep_shards.is_empty() {
                merged.repair_need.insert(
                    policy.to_string(),
                    Self::merge_scalar_shards(&rep_shards),
                );
            }
        }
        merged
    }

    fn merge_scalar_shards(shards: &[&ScalarKalmanState]) -> ScalarKalmanState {
        let total_obs: u64 = shards.iter().map(|s| s.observation_count as u64).sum();
        if total_obs == 0 {
            return ScalarKalmanState::new();
        }
        let mut est = 0.0f64;
        let mut var = 0.0f64;
        for shard in shards {
            let w = shard.observation_count as f64 / total_obs as f64;
            est += w * shard.estimate;
            var += w * shard.variance;
        }
        ScalarKalmanState {
            estimate: est.clamp(0.0, 1.0),
            variance: var.max(1e-12),
            adaptive_q: DEFAULT_Q,
            adaptive_r: DEFAULT_R,
            observation_count: total_obs.min(u32::MAX as u64) as u32,
        }
    }

    pub fn load_from_path(path: &Path) -> Option<Self> {
        StateCodec::read_decode_binary(path).ok()
    }

    pub fn save_to_path(&self, path: &Path) -> Result<()> {
        let bytes = StateCodec::encode_binary(self)?;
        crate::files::atomic_writer::AtomicWriter::write_bytes(path, bytes.as_slice())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_observation_trusts() {
        let mut s = ScalarKalmanState::new();
        s.observe(0.8);
        assert!((s.estimate - 0.8).abs() < 1e-9);
        assert_eq!(s.observation_count, 1);
    }

    #[test]
    fn converges_toward_observation() {
        let mut s = ScalarKalmanState::new();
        for _ in 0..20 {
            s.observe(0.6);
        }
        assert!((s.estimate - 0.6).abs() < 0.05, "estimate={}", s.estimate);
    }

    #[test]
    fn zero_interference_stays_near_zero() {
        let mut s = ScalarKalmanState::new();
        for _ in 0..20 {
            s.observe(0.0);
        }
        assert!(s.estimate < 0.05, "estimate={}", s.estimate);
    }

    #[test]
    fn dynamic_priority_no_data_equals_base() {
        let state = PolicyInterferenceState::new();
        let dp = state.dynamic_priority("naming_conventions", 10, 0.85);
        assert!((dp - 10.0).abs() < 1e-9, "dp={}", dp);
    }

    #[test]
    fn dynamic_priority_high_interference_lowers() {
        let mut state = PolicyInterferenceState::new();
        for _ in 0..20 {
            state.observe_interference("naming_conventions", 0.8);
        }
        let dp = state.dynamic_priority("naming_conventions", 10, 0.85);
        assert!(dp < 10.0, "high interference should lower priority, dp={}", dp);
    }

    #[test]
    fn repair_weight_no_data_is_zero() {
        let state = PolicyInterferenceState::new();
        let w = state.repair_weight("declaration_alignment", 0.85);
        assert!(w.abs() < 1e-9, "w={}", w);
    }

    #[test]
    fn repair_weight_high_need() {
        let mut state = PolicyInterferenceState::new();
        for _ in 0..20 {
            state.observe_repair_need("declaration_alignment", 0.9);
        }
        let w = state.repair_weight("declaration_alignment", 0.85);
        assert!(w > 0.3, "high repair need should produce high weight, w={}", w);
    }

    #[test]
    fn merge_shards_weighted() {
        let mut s1 = PolicyInterferenceState::new();
        for _ in 0..10 {
            s1.observe_interference("policy_a", 0.8);
        }
        let mut s2 = PolicyInterferenceState::new();
        for _ in 0..10 {
            s2.observe_interference("policy_a", 0.2);
        }
        let merged = PolicyInterferenceState::merge_shards(&[s1, s2]);
        let est = merged.interference_estimate("policy_a");
        assert!(est > 0.3 && est < 0.7, "merged estimate should be between shards, est={}", est);
    }

    #[test]
    fn estimate_clamped_to_unit() {
        let mut s = ScalarKalmanState::new();
        s.observe(1.5);
        assert!(s.estimate <= 1.0);
        s.observe(-0.5);
        assert!(s.estimate >= 0.0);
    }
}
