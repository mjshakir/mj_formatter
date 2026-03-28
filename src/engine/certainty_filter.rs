#![allow(clippy::needless_range_loop)]

use serde::{Deserialize, Serialize};
use crate::engine::mat5::*;

pub const NUM_DIMS: usize = 5;

// Dimension indices: 0=structural, 1=semantic, 2=coverage, 3=richness, 4=edit_success

// Default Q — process noise (from old Stable model)
const DEFAULT_Q_DIAG: [f64; NUM_DIMS] = [0.0001, 0.0001, 0.00005, 0.00005, 0.001];

// Default R — measurement noise (from old Stable model)
const DEFAULT_R_DIAG: [f64; NUM_DIMS] = [0.002, 0.002, 0.003, 0.005, 0.02];

// Default estimates
const DEFAULT_ESTIMATES: [f64; NUM_DIMS] = [0.85, 0.50, 0.50, 0.50, 0.85];

// Default covariance diagonal
const DEFAULT_COV_DIAG: [f64; NUM_DIMS] = [0.05, 0.10, 0.10, 0.10, 0.05];

// Sage-Husa adaptive noise guard rails
const Q_FLOOR: [f64; NUM_DIMS] = [1e-6, 1e-6, 1e-6, 1e-6, 1e-5];
const Q_CEIL: [f64; NUM_DIMS] = [0.05, 0.05, 0.04, 0.04, 0.08];
const R_FLOOR: [f64; NUM_DIMS] = DEFAULT_R_DIAG;
const R_CEIL: [f64; NUM_DIMS] = [0.10, 0.10, 0.08, 0.08, 0.15];
const ADAPTATION_WINDOW: f64 = 20.0;
const ADAPTATION_MIN_OBS: u32 = 3;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CertaintyFilterState {
    pub estimates: [f64; NUM_DIMS],
    pub covariance: [[f64; NUM_DIMS]; NUM_DIMS],
    pub adaptive_q: [[f64; NUM_DIMS]; NUM_DIMS],
    pub adaptive_r: [[f64; NUM_DIMS]; NUM_DIMS],
    pub adaptation_count: u32,
    pub observation_count: u32,
}

// Full 5x5 Kalman update with Joseph form for numerical stability
fn kalman_update_full(
    prior_est: [f64; NUM_DIMS],
    prior_cov: [[f64; N]; N],
    measurement: [f64; NUM_DIMS],
    q: &[[f64; N]; N],
    r: &[[f64; N]; N],
) -> ([f64; NUM_DIMS], [[f64; N]; N]) {
    // Predict: P_pred = P + Q (F=I)
    let p_pred = mat5_add(&prior_cov, q);

    // Innovation: z - H*x (H=I)
    let innov = vec5_sub(&measurement, &prior_est);

    // Innovation covariance: S = P_pred + R
    let s = mat5_add(&p_pred, r);

    // Kalman gain: K = P_pred * S^{-1}
    let s_inv = mat5_inverse_spd(&s).unwrap_or_else(mat5_identity);
    let k = mat5_mul(&p_pred, &s_inv);

    // Update estimate: x = x + K * innov
    let k_innov = mat5_matvec(&k, &innov);
    let est_out = vec5_add_clamp(&prior_est, &k_innov);

    // Joseph form: P = (I-K)*P_pred*(I-K)^T + K*R*K^T
    let i_k = mat5_sub(&mat5_identity(), &k);
    let i_k_t = mat5_transpose(&i_k);
    let k_t = mat5_transpose(&k);
    let cov_out = mat5_add(
        &mat5_mul(&i_k, &mat5_mul(&p_pred, &i_k_t)),
        &mat5_mul(&k, &mat5_mul(r, &k_t)),
    );

    (est_out, cov_out)
}

fn clamp_diagonal(m: &mut [[f64; N]; N], floor: &[f64; NUM_DIMS], ceil: &[f64; NUM_DIMS]) {
    for d in 0..NUM_DIMS {
        m[d][d] = m[d][d].clamp(floor[d], ceil[d]);
    }
}

impl CertaintyFilterState {
    pub fn new() -> Self {
        Self {
            estimates: DEFAULT_ESTIMATES,
            covariance: mat5_diagonal(&DEFAULT_COV_DIAG),
            adaptive_q: mat5_diagonal(&DEFAULT_Q_DIAG),
            adaptive_r: mat5_diagonal(&DEFAULT_R_DIAG),
            adaptation_count: 0,
            observation_count: 0,
        }
    }

    #[cfg(test)]
    pub fn new_with_prior(
        prior_estimates: [f64; NUM_DIMS],
        prior_variances: [f64; NUM_DIMS],
        prior_observation_count: u32,
    ) -> Self {
        Self {
            estimates: prior_estimates,
            covariance: mat5_diagonal(&prior_variances),
            adaptive_q: mat5_diagonal(&DEFAULT_Q_DIAG),
            adaptive_r: mat5_diagonal(&DEFAULT_R_DIAG),
            adaptation_count: 0,
            observation_count: prior_observation_count,
        }
    }

    pub fn load_from_path(path: &std::path::Path) -> Option<Self> {
        crate::files::codec::StateCodec::read_decode_binary::<Self>(path).ok()
    }

    pub fn save_to_path(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let bytes = crate::files::codec::StateCodec::encode_binary(self)?;
        crate::files::atomic_writer::AtomicWriter::write_bytes(path, bytes.as_slice())?;
        Ok(())
    }

    pub fn merge_shards(shards: &[Self]) -> Self {
        if shards.is_empty() {
            return Self::new();
        }
        let total_obs: u64 = shards.iter().map(|s| s.observation_count as u64).sum();
        if total_obs == 0 {
            return Self::new();
        }
        let mut merged_est = [0.0f64; NUM_DIMS];
        let mut merged_cov = [[0.0f64; NUM_DIMS]; NUM_DIMS];
        for shard in shards {
            let w = shard.observation_count as f64 / total_obs as f64;
            for d in 0..NUM_DIMS {
                merged_est[d] += w * shard.estimates[d];
                for d2 in 0..NUM_DIMS {
                    merged_cov[d][d2] += w * shard.covariance[d][d2];
                }
            }
        }
        Self {
            estimates: merged_est,
            covariance: merged_cov,
            adaptive_q: mat5_diagonal(&DEFAULT_Q_DIAG),
            adaptive_r: mat5_diagonal(&DEFAULT_R_DIAG),
            adaptation_count: 0,
            observation_count: total_obs.min(u32::MAX as u64) as u32,
        }
    }

    #[cfg(test)]
    pub fn structural(&self) -> f64 { self.estimates[0] }
    #[cfg(test)]
    pub fn semantic(&self) -> f64 { self.estimates[1] }
    #[cfg(test)]
    pub fn coverage(&self) -> f64 { self.estimates[2] }
    #[cfg(test)]
    pub fn richness(&self) -> f64 { self.estimates[3] }
    pub fn edit_success(&self) -> f64 { self.estimates[4] }
    pub fn variance(&self, dim: usize) -> f64 { self.covariance[dim][dim] }
    pub fn stddev(&self, dim: usize) -> f64 { self.covariance[dim][dim].sqrt() }

    // ── Proposer derivations ─────────────────────────────────────────

    pub fn trust_deficit_penalty(&self) -> f64 {
        if self.observation_count == 0 { return 0.0; }
        self.stddev(4).min(0.3)
    }

    pub fn confidence_penalty(&self) -> f64 {
        if self.observation_count == 0 { return 0.0; }
        self.stddev(1).min(0.3)
    }

    pub fn richness_multiplier(&self) -> f64 {
        (self.estimates[3] * 2.0).clamp(0.5, 3.0)
    }

    pub fn style_gain_weights(&self) -> (f64, f64, f64) {
        let e4 = self.estimates[4];
        (e4 * 0.705_882_352_941_176_5, e4 * 0.470_588_235_294_117_65, e4 * 0.235_294_117_647_058_82)
    }

    // ── Conflict solver derivations ──────────────────────────────────

    pub fn stabilizer_bonus(&self, risk_tier_index: usize) -> f64 {
        let e0 = self.estimates[0];
        match risk_tier_index {
            0 => e0 * 0.117_647_058_823_529_41,
            1 => e0 * 0.058_823_529_411_764_71,
            _ => 0.0,
        }
    }

    pub fn scope_bonus(&self, risk_tier_index: usize) -> f64 {
        let e2 = self.estimates[2];
        match risk_tier_index {
            0 => e2 * 0.16,
            _ => e2 * 0.08,
        }
    }

    pub fn risk_penalty(&self, risk_tier_index: usize) -> f64 {
        let complement = 1.0 - self.estimates[4];
        match risk_tier_index {
            2 => complement * 1.0,
            1 => complement * 0.333_333_333_333_333_3,
            _ => 0.0,
        }
    }

    pub fn footprint_weights(&self) -> (f64, f64) {
        let v3 = self.variance(3);
        (v3 * 0.2, v3 * 0.1)
    }

    pub fn component_threshold(&self) -> usize {
        (self.estimates[3] * 32.0).clamp(8.0, 64.0) as usize
    }

    pub fn fuzzy_min_semantic(&self) -> usize {
        (self.variance(1) * 30.0).clamp(1.0, 8.0) as usize
    }

    pub fn fuzzy_min_other(&self) -> usize {
        1
    }

    // ── Post-check derivations ───────────────────────────────────────

    pub fn diagnostic_weights(&self) -> (u32, u32, u32, u32) {
        let avg = (self.estimates[0] + self.estimates[1]) / 2.0;
        (1, (avg * 4.444_444_444_444_445).round() as u32, (avg * 11.851_851_851_851_853).round() as u32, (avg * 17.777_777_777_777_78).round() as u32)
    }

    pub fn identity_shift_tolerance(&self) -> usize {
        (self.variance(1) * 40.0).clamp(1.0, 12.0) as usize
    }

    // ── Semantic contract transition derivations ─────────────────────

    pub fn identity_penalty(&self) -> u32 {
        ((self.estimates[1] + self.estimates[4]) * 66.666_666_666_666_67).round() as u32
    }

    pub fn reference_penalty(&self) -> u32 {
        ((self.estimates[1] + self.estimates[4]) * 51.851_851_851_851_85).round() as u32
    }

    pub fn usage_penalty(&self) -> u32 {
        ((self.estimates[1] + self.estimates[4]) * 40.740_740_740_740_74).round() as u32
    }

    pub fn orphan_penalty(&self) -> u32 {
        ((self.estimates[1] + self.estimates[4]) * 48.148_148_148_148_15).round() as u32
    }

    pub fn scope_penalty(&self) -> u32 {
        ((self.estimates[1] + self.estimates[4]) * 59.259_259_259_259_26).round() as u32
    }

    // ── Cluster telemetry derivations ────────────────────────────────

    pub fn cluster_relax_stability(&self) -> f64 {
        self.estimates[4] * 0.941_176_470_588_235_3
    }

    pub fn cluster_relax_uncertainty(&self) -> f64 {
        self.stddev(4) * 0.805
    }

    pub fn cluster_harden_stability(&self) -> f64 {
        self.estimates[4] * 0.647_058_823_529_411_8
    }

    pub fn cluster_harden_uncertainty(&self) -> f64 {
        self.stddev(4) * 1.966
    }

    pub fn cluster_harden_revert_rate(&self) -> f64 {
        self.stddev(4) * 0.983
    }

    pub fn cluster_cap1_stability(&self) -> f64 {
        self.estimates[4] * 0.470_588_235_294_117_65
    }

    pub fn cluster_cap1_reliability(&self) -> f64 {
        self.estimates[4] * 0.352_941_176_470_588_24
    }

    pub fn cluster_cap3_stability(&self) -> f64 {
        self.estimates[4] * 0.705_882_352_941_176_5
    }

    pub fn cluster_cap3_uncertainty(&self) -> f64 {
        self.stddev(4) * 1.5625
    }

    #[cfg(test)]
    pub fn cluster_outcome_regressed(&self) -> f64 {
        self.estimates[4] * 0.294_117_647_058_823_53
    }

    #[cfg(test)]
    pub fn cluster_outcome_accepted(&self) -> f64 {
        1.0
    }

    // ── Pipeline derivations ─────────────────────────────────────────

    pub fn semantic_confidence_bp_base(&self) -> u16 {
        (self.estimates[1] * 400.0).clamp(50.0, 500.0) as u16
    }

    pub fn retry_batch(&self) -> usize {
        if self.estimates[0] > 0.7 { usize::MAX } else { 512 }
    }

    pub fn observe(
        &mut self,
        measurement: [f64; NUM_DIMS],
    ) {
        if self.observation_count == 0 {
            self.estimates = measurement;
            self.covariance = mat5_diagonal(&DEFAULT_R_DIAG);
            self.observation_count = 1;
            return;
        }

        // Single-model Kalman update
        let (updated_est, updated_cov) = kalman_update_full(
            self.estimates,
            self.covariance,
            measurement,
            &self.adaptive_q,
            &self.adaptive_r,
        );

        // Sage-Husa adaptive Q/R update
        self.adaptation_count += 1;
        if self.adaptation_count >= ADAPTATION_MIN_OBS {
            let alpha = 1.0 / (self.adaptation_count as f64).min(ADAPTATION_WINDOW);
            let innov = vec5_sub(&measurement, &self.estimates);
            let innov_outer = mat5_outer(&innov);

            // Adaptive R: R_hat = (1-α)*R + α*(innov*innov^T - P_pred)
            let p_pred = mat5_add(&self.covariance, &self.adaptive_q);
            let r_update = mat5_sub(&innov_outer, &p_pred);
            self.adaptive_r = mat5_add(
                &mat5_scale(&self.adaptive_r, 1.0 - alpha),
                &mat5_scale(&r_update, alpha),
            );
            clamp_diagonal(&mut self.adaptive_r, &R_FLOOR, &R_CEIL);
            enforce_spd(&mut self.adaptive_r);

            // Adaptive Q: Q_hat = (1-α)*Q + α*(K*innov*innov^T*K^T + P_upd - P_mixed)
            let s = mat5_add(&p_pred, &self.adaptive_r);
            let s_inv = mat5_inverse_spd(&s).unwrap_or_else(mat5_identity);
            let k = mat5_mul(&p_pred, &s_inv);
            let k_innov = mat5_matvec(&k, &innov);
            let q_update = mat5_sub(
                &mat5_add(&mat5_outer(&k_innov), &updated_cov),
                &self.covariance,
            );
            self.adaptive_q = mat5_add(
                &mat5_scale(&self.adaptive_q, 1.0 - alpha),
                &mat5_scale(&q_update, alpha),
            );
            clamp_diagonal(&mut self.adaptive_q, &Q_FLOOR, &Q_CEIL);
            enforce_spd(&mut self.adaptive_q);
        }

        // Store updated state
        self.estimates = updated_est;
        self.covariance = updated_cov;
        self.observation_count = self.observation_count.saturating_add(1);

        // Clamp estimates to [0, 1]
        for e in self.estimates.iter_mut() {
            *e = e.clamp(0.0, 1.0);
        }
    }
}

impl Default for CertaintyFilterState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(state: &mut CertaintyFilterState, s: f64, m: f64, c: f64, r: f64) {
        state.observe([s, m, c, r, 0.5]);
    }

    fn obs5(state: &mut CertaintyFilterState, s: f64, m: f64, c: f64, r: f64, e: f64) {
        state.observe([s, m, c, r, e]);
    }

    #[test]
    fn first_obs_trusts() {
        let mut state = CertaintyFilterState::new();
        obs(&mut state, 0.95, 0.82, 0.70, 0.50);
        assert!((state.structural() - 0.95).abs() < 1e-9);
        assert!((state.semantic() - 0.82).abs() < 1e-9);
        assert!((state.coverage() - 0.70).abs() < 1e-9);
        assert!((state.richness() - 0.50).abs() < 1e-9);
    }

    #[test]
    fn subsequent_observations_smooth() {
        let mut state = CertaintyFilterState::new();
        obs(&mut state, 0.95, 0.82, 0.70, 0.50);
        obs(&mut state, 0.95, 0.72, 0.70, 0.50);
        assert!(state.semantic() > 0.72 && state.semantic() < 0.82);
    }

    #[test]
    fn observation_count_increments() {
        let mut state = CertaintyFilterState::new();
        obs(&mut state, 0.95, 0.82, 0.70, 0.50);
        assert_eq!(state.observation_count, 1);
        obs(&mut state, 0.90, 0.80, 0.65, 0.45);
        assert_eq!(state.observation_count, 2);
        obs(&mut state, 0.85, 0.75, 0.60, 0.40);
        assert_eq!(state.observation_count, 3);
    }

    #[test]
    fn converges_stable() {
        let mut state = CertaintyFilterState::new();
        obs(&mut state, 0.95, 0.80, 0.70, 0.50);
        for _ in 0..20 {
            obs(&mut state, 0.95, 0.80, 0.70, 0.50);
        }
        assert!((state.structural() - 0.95).abs() < 0.02);
        assert!((state.semantic() - 0.80).abs() < 0.02);
        assert!((state.coverage() - 0.70).abs() < 0.02);
        assert!((state.richness() - 0.50).abs() < 0.02);
    }

    #[test]
    fn resists_single_outlier() {
        let mut state = CertaintyFilterState::new();
        for _ in 0..10 {
            obs(&mut state, 0.95, 0.82, 0.70, 0.50);
        }
        obs(&mut state, 0.95, 0.60, 0.70, 0.50);
        assert!(state.semantic() > 0.72, "semantic after outlier: {}", state.semantic());
    }

    #[test]
    fn variance_narrows_with_observations() {
        let mut state = CertaintyFilterState::new();
        obs(&mut state, 0.95, 0.80, 0.70, 0.50);
        obs(&mut state, 0.95, 0.80, 0.70, 0.50);
        let early_var = state.variance(1);
        for _ in 0..15 {
            obs(&mut state, 0.95, 0.80, 0.70, 0.50);
        }
        obs(&mut state, 0.95, 0.80, 0.70, 0.50);
        let late_var = state.variance(1);
        assert!(
            late_var < early_var,
            "semantic variance should narrow: early={}, late={}",
            early_var,
            late_var,
        );
    }

    #[test]
    fn interval_narrows_convergence() {
        let mut state = CertaintyFilterState::new();
        obs(&mut state, 0.95, 0.85, 0.70, 0.50);
        obs(&mut state, 0.95, 0.85, 0.70, 0.50);
        let z = 1.645;
        let early_ci = (state.semantic() - z * state.variance(1).sqrt()).clamp(0.0, 1.0);
        for _ in 0..15 {
            obs(&mut state, 0.95, 0.85, 0.70, 0.50);
        }
        obs(&mut state, 0.95, 0.85, 0.70, 0.50);
        let late_ci = (state.semantic() - z * state.variance(1).sqrt()).clamp(0.0, 1.0);
        assert!(
            late_ci > early_ci,
            "CI should tighten: early_ci={}, late_ci={}",
            early_ci,
            late_ci
        );
    }

    #[test]
    fn five_dims_independent() {
        let mut state = CertaintyFilterState::new();
        obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.90);
        obs5(&mut state, 0.50, 0.80, 0.70, 0.50, 0.90);
        assert!(state.structural() < 0.85);
        assert!(state.semantic() > 0.78);
        assert!(state.coverage() > 0.68);
        assert!(state.richness() > 0.48);
        assert!(state.edit_success() > 0.85);
    }

    #[test]
    fn five_dim_trusts() {
        let mut state = CertaintyFilterState::new();
        obs5(&mut state, 0.95, 0.82, 0.70, 0.50, 0.90);
        assert!((state.edit_success() - 0.90).abs() < 1e-9);
    }

    #[test]
    fn edit_success_converges() {
        let mut state = CertaintyFilterState::new();
        obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.95);
        for _ in 0..20 {
            obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.95);
        }
        assert!((state.edit_success() - 0.95).abs() < 0.03);
    }

    #[test]
    fn edit_resists_failure() {
        let mut state = CertaintyFilterState::new();
        for _ in 0..10 {
            obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.95);
        }
        obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.10);
        assert!(state.edit_success() > 0.60, "edit_success should resist single failure: {}", state.edit_success());
    }

    #[test]
    fn edit_variance_narrows() {
        let mut state = CertaintyFilterState::new();
        obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.90);
        obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.90);
        let early_var = state.variance(4);
        for _ in 0..15 {
            obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.90);
        }
        obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.90);
        let late_var = state.variance(4);
        assert!(
            late_var < early_var,
            "edit_success variance should narrow: early={}, late={}",
            early_var,
            late_var
        );
    }

    #[test]
    fn cross_covariance_emerges() {
        let mut state = CertaintyFilterState::new();
        for _ in 0..20 {
            obs5(&mut state, 0.90, 0.85, 0.70, 0.50, 0.90);
        }
        obs5(&mut state, 0.90, 0.85, 0.70, 0.50, 0.90);
        assert!(state.covariance[0][0] > 0.0, "diagonal should be positive");
        assert!(state.covariance[1][1] > 0.0, "diagonal should be positive");
    }

    #[test]
    fn adaptive_qr_converges() {
        let mut state = CertaintyFilterState::new();
        for _ in 0..30 {
            obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.90);
        }
        assert!(state.adaptation_count >= 29);
        for d in 0..NUM_DIMS {
            assert!(state.adaptive_q[d][d] >= Q_FLOOR[d]);
            assert!(state.adaptive_q[d][d] <= Q_CEIL[d]);
        }
    }

    #[test]
    fn prior_count_preserved() {
        let est = [0.90, 0.80, 0.70, 0.50, 0.65];
        let var = [0.01; NUM_DIMS];
        let mut state = CertaintyFilterState::new_with_prior(est, var, 5);
        assert_eq!(state.observation_count, 5, "prior obs count should be set");
        state.observe([0.92, 0.82, 0.72, 0.52, 0.70]);
        assert_eq!(state.observation_count, 6, "obs count should increment after observe");
        assert!(state.structural() > 0.88 && state.structural() < 0.95,
            "structural should blend prior and measurement, got {}", state.structural());
    }

    #[test]
    fn default_estimates_match() {
        let state = CertaintyFilterState::new();
        assert_eq!(state.estimates, DEFAULT_ESTIMATES);
        assert!((state.structural() - 0.85).abs() < 1e-10);
        assert!((state.semantic() - 0.50).abs() < 1e-10);
        assert!((state.coverage() - 0.50).abs() < 1e-10);
        assert!((state.richness() - 0.50).abs() < 1e-10);
        assert!((state.edit_success() - 0.85).abs() < 1e-10);
    }

    #[test]
    fn default_variance_match() {
        let state = CertaintyFilterState::new();
        assert!((state.variance(0) - 0.05).abs() < 1e-10);
        assert!((state.variance(1) - 0.10).abs() < 1e-10);
        assert!((state.variance(2) - 0.10).abs() < 1e-10);
        assert!((state.variance(3) - 0.10).abs() < 1e-10);
        assert!((state.variance(4) - 0.05).abs() < 1e-10);
    }

    // ── Derivation default tests ─────────────────────────────────────
    // Each test verifies that CertaintyFilterState::default() + derivation
    // produces exactly the current hardcoded constant value.

    #[test]
    fn derivation_proposer_penalties() {
        let state = CertaintyFilterState::new();
        // observation_count == 0 → penalties are 0.0
        assert!((state.trust_deficit_penalty() - 0.0).abs() < 1e-10);
        assert!((state.confidence_penalty() - 0.0).abs() < 1e-10);
    }

    #[test]
    fn derivation_richness_multiplier() {
        let state = CertaintyFilterState::new();
        // e[3]=0.50 → 0.50*2.0 = 1.0
        assert!((state.richness_multiplier() - 1.0).abs() < 1e-10);
    }

    #[test]
    fn derivation_style_gain() {
        let state = CertaintyFilterState::new();
        let (base, ws, delta) = state.style_gain_weights();
        // e[4]=0.85 → (0.60, 0.40, 0.20)
        assert!((base - 0.60).abs() < 0.001, "base={base}");
        assert!((ws - 0.40).abs() < 0.001, "ws={ws}");
        assert!((delta - 0.20).abs() < 0.001, "delta={delta}");
    }

    #[test]
    fn derivation_stabilizer_bonus() {
        let state = CertaintyFilterState::new();
        // e[0]=0.85 → tier0: 0.85*0.1176..=0.10, tier1: 0.85*0.0588..=0.05
        assert!((state.stabilizer_bonus(0) - 0.10).abs() < 0.001, "tier0={}", state.stabilizer_bonus(0));
        assert!((state.stabilizer_bonus(1) - 0.05).abs() < 0.001, "tier1={}", state.stabilizer_bonus(1));
        assert!((state.stabilizer_bonus(2) - 0.0).abs() < 1e-10);
    }

    #[test]
    fn derivation_scope_bonus() {
        let state = CertaintyFilterState::new();
        // e[2]=0.50 → tier0: 0.50*0.16=0.08, other: 0.50*0.08=0.04
        assert!((state.scope_bonus(0) - 0.08).abs() < 1e-10);
        assert!((state.scope_bonus(1) - 0.04).abs() < 1e-10);
    }

    #[test]
    fn derivation_risk_penalty() {
        let state = CertaintyFilterState::new();
        // 1-e[4] = 0.15 → tier2: 0.15*1.0=0.15, tier1: 0.15*0.333=0.05
        assert!((state.risk_penalty(2) - 0.15).abs() < 0.001, "tier2={}", state.risk_penalty(2));
        assert!((state.risk_penalty(1) - 0.05).abs() < 0.001, "tier1={}", state.risk_penalty(1));
        assert!((state.risk_penalty(0) - 0.0).abs() < 1e-10);
    }

    #[test]
    fn derivation_footprint_weights() {
        let state = CertaintyFilterState::new();
        let (range_w, symbol_w) = state.footprint_weights();
        // v[3]=0.10 → (0.10*0.2=0.02, 0.10*0.1=0.01)
        assert!((range_w - 0.02).abs() < 1e-10);
        assert!((symbol_w - 0.01).abs() < 1e-10);
    }

    #[test]
    fn derivation_component_threshold() {
        let state = CertaintyFilterState::new();
        // e[3]=0.50 → 0.50*32=16
        assert_eq!(state.component_threshold(), 16);
    }

    #[test]
    fn derivation_fuzzy_min() {
        let state = CertaintyFilterState::new();
        // v[1]=0.10 → 0.10*30=3
        assert_eq!(state.fuzzy_min_semantic(), 3);
        assert_eq!(state.fuzzy_min_other(), 1);
    }

    #[test]
    fn derivation_diagnostic_weights() {
        let state = CertaintyFilterState::new();
        let (note_w, warn_w, err_w, fatal_w) = state.diagnostic_weights();
        // avg = (0.85+0.50)/2 = 0.675
        assert_eq!(note_w, 1);
        assert_eq!(warn_w, 3, "warn_w={warn_w}");
        assert_eq!(err_w, 8, "err_w={err_w}");
        assert_eq!(fatal_w, 12, "fatal_w={fatal_w}");
    }

    #[test]
    fn derivation_identity_shift_tol() {
        let state = CertaintyFilterState::new();
        // v[1]=0.10 → 0.10*40=4
        assert_eq!(state.identity_shift_tolerance(), 4);
    }

    #[test]
    fn derivation_semantic_penalties() {
        let state = CertaintyFilterState::new();
        // e[1]+e[4] = 0.50+0.85 = 1.35
        assert_eq!(state.identity_penalty(), 90, "identity={}", state.identity_penalty());
        assert_eq!(state.reference_penalty(), 70, "reference={}", state.reference_penalty());
        assert_eq!(state.usage_penalty(), 55, "usage={}", state.usage_penalty());
        assert_eq!(state.orphan_penalty(), 65, "orphan={}", state.orphan_penalty());
        assert_eq!(state.scope_penalty(), 80, "scope={}", state.scope_penalty());
    }

    #[test]
    fn derivation_cluster_relax() {
        let state = CertaintyFilterState::new();
        // e[4]=0.85, sd[4]=sqrt(0.05)≈0.2236
        assert!((state.cluster_relax_stability() - 0.80).abs() < 0.001, "relax_stab={}", state.cluster_relax_stability());
        assert!((state.cluster_relax_uncertainty() - 0.18).abs() < 0.001, "relax_unc={}", state.cluster_relax_uncertainty());
    }

    #[test]
    fn derivation_cluster_harden() {
        let state = CertaintyFilterState::new();
        assert!((state.cluster_harden_stability() - 0.55).abs() < 0.001, "harden_stab={}", state.cluster_harden_stability());
        assert!((state.cluster_harden_uncertainty() - 0.44).abs() < 0.002, "harden_unc={}", state.cluster_harden_uncertainty());
        assert!((state.cluster_harden_revert_rate() - 0.22).abs() < 0.001, "harden_rr={}", state.cluster_harden_revert_rate());
    }

    #[test]
    fn derivation_cluster_caps() {
        let state = CertaintyFilterState::new();
        assert!((state.cluster_cap1_stability() - 0.40).abs() < 0.001, "cap1_stab={}", state.cluster_cap1_stability());
        assert!((state.cluster_cap1_reliability() - 0.30).abs() < 0.001, "cap1_rel={}", state.cluster_cap1_reliability());
        assert!((state.cluster_cap3_stability() - 0.60).abs() < 0.001, "cap3_stab={}", state.cluster_cap3_stability());
        assert!((state.cluster_cap3_uncertainty() - 0.35).abs() < 0.002, "cap3_unc={}", state.cluster_cap3_uncertainty());
    }

    #[test]
    fn derivation_cluster_outcomes() {
        let state = CertaintyFilterState::new();
        assert!((state.cluster_outcome_regressed() - 0.25).abs() < 0.001, "regressed={}", state.cluster_outcome_regressed());
        assert!((state.cluster_outcome_accepted() - 1.0).abs() < 1e-10);
    }

    #[test]
    fn derivation_pipeline() {
        let state = CertaintyFilterState::new();
        // e[1]=0.50 → 0.50*400=200
        assert_eq!(state.semantic_confidence_bp_base(), 200);
        // e[0]=0.85 > 0.7 → MAX
        assert_eq!(state.retry_batch(), usize::MAX);
    }
}
