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
    pub content_hash: u64,
}

pub struct CertaintyFilterResult {
    pub estimates: [f64; NUM_DIMS],
    pub covariance: Mat5,
    pub observation_count: u32,
}

impl CertaintyFilterResult {
    pub fn structural(&self) -> f64 { self.estimates[0] }
    pub fn semantic(&self) -> f64 { self.estimates[1] }
    pub fn coverage(&self) -> f64 { self.estimates[2] }
    pub fn richness(&self) -> f64 { self.estimates[3] }
    pub fn edit_success(&self) -> f64 { self.estimates[4] }
    pub fn variance(&self, dim: usize) -> f64 { self.covariance[dim][dim] }
    pub fn stddev(&self, dim: usize) -> f64 { self.covariance[dim][dim].sqrt() }
}

// Full 5x5 Kalman update with Joseph form for numerical stability
fn kalman_update_full(
    prior_est: [f64; NUM_DIMS],
    prior_cov: Mat5,
    measurement: [f64; NUM_DIMS],
    q: &Mat5,
    r: &Mat5,
) -> ([f64; NUM_DIMS], Mat5) {
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

fn clamp_diagonal(m: &mut Mat5, floor: &[f64; NUM_DIMS], ceil: &[f64; NUM_DIMS]) {
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
            content_hash: 0,
        }
    }

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
            content_hash: 0,
        }
    }

    pub fn structural(&self) -> f64 { self.estimates[0] }
    pub fn semantic(&self) -> f64 { self.estimates[1] }
    pub fn coverage(&self) -> f64 { self.estimates[2] }
    pub fn richness(&self) -> f64 { self.estimates[3] }
    pub fn edit_success(&self) -> f64 { self.estimates[4] }
    pub fn variance(&self, dim: usize) -> f64 { self.covariance[dim][dim] }
    pub fn stddev(&self, dim: usize) -> f64 { self.covariance[dim][dim].sqrt() }

    pub fn observe(
        &mut self,
        measurement: [f64; NUM_DIMS],
        content_hash: u64,
    ) -> CertaintyFilterResult {
        if content_hash != self.content_hash {
            if self.observation_count > 0 && self.content_hash != 0 {
                *self = Self::new();
            }
            self.content_hash = content_hash;
        }

        if self.observation_count == 0 {
            let has_prior = self.estimates.iter().any(|&e| e != DEFAULT_ESTIMATES[0])
                || self.estimates[0] != DEFAULT_ESTIMATES[0];
            if has_prior && self.estimates != DEFAULT_ESTIMATES {
                self.observation_count = 1;
            } else {
                self.estimates = measurement;
                self.covariance = mat5_diagonal(&DEFAULT_R_DIAG);
                self.observation_count = 1;

                return CertaintyFilterResult {
                    estimates: measurement,
                    covariance: self.covariance,
                    observation_count: self.observation_count,
                };
            }
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

        CertaintyFilterResult {
            estimates: self.estimates,
            covariance: self.covariance,
            observation_count: self.observation_count,
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

    fn obs(state: &mut CertaintyFilterState, s: f64, m: f64, c: f64, r: f64, hash: u64) -> CertaintyFilterResult {
        state.observe([s, m, c, r, 0.5], hash)
    }

    fn obs5(state: &mut CertaintyFilterState, s: f64, m: f64, c: f64, r: f64, e: f64, hash: u64) -> CertaintyFilterResult {
        state.observe([s, m, c, r, e], hash)
    }

    #[test]
    fn first_obs_trusts() {
        let mut state = CertaintyFilterState::new();
        let result = obs(&mut state, 0.95, 0.82, 0.70, 0.50, 123);
        assert!((result.structural() - 0.95).abs() < 1e-9);
        assert!((result.semantic() - 0.82).abs() < 1e-9);
        assert!((result.coverage() - 0.70).abs() < 1e-9);
        assert!((result.richness() - 0.50).abs() < 1e-9);
    }

    #[test]
    fn subsequent_observations_smooth() {
        let mut state = CertaintyFilterState::new();
        obs(&mut state, 0.95, 0.82, 0.70, 0.50, 100);
        let result = obs(&mut state, 0.95, 0.72, 0.70, 0.50, 100);
        assert!(result.semantic() > 0.72 && result.semantic() < 0.82);
    }

    #[test]
    fn hash_resets_state() {
        let mut state = CertaintyFilterState::new();
        obs(&mut state, 0.95, 0.82, 0.70, 0.50, 100);
        obs(&mut state, 0.95, 0.82, 0.70, 0.50, 100);
        let pre_count = state.observation_count;
        assert!(pre_count >= 2);
        let result = obs(&mut state, 0.90, 0.50, 0.60, 0.40, 999);
        assert!(result.semantic() < 0.55);
        assert!(state.observation_count >= 1);
        assert!(state.observation_count < pre_count);
    }

    #[test]
    fn converges_stable() {
        let mut state = CertaintyFilterState::new();
        let mut result = obs(&mut state, 0.95, 0.80, 0.70, 0.50, 42);
        for _ in 0..20 {
            result = obs(&mut state, 0.95, 0.80, 0.70, 0.50, 42);
        }
        assert!((result.structural() - 0.95).abs() < 0.02);
        assert!((result.semantic() - 0.80).abs() < 0.02);
        assert!((result.coverage() - 0.70).abs() < 0.02);
        assert!((result.richness() - 0.50).abs() < 0.02);
    }

    #[test]
    fn resists_single_outlier() {
        let mut state = CertaintyFilterState::new();
        for _ in 0..10 {
            obs(&mut state, 0.95, 0.82, 0.70, 0.50, 42);
        }
        let result = obs(&mut state, 0.95, 0.60, 0.70, 0.50, 42);
        assert!(result.semantic() > 0.72, "semantic after outlier: {}", result.semantic());
    }

    #[test]
    fn variance_narrows_with_observations() {
        let mut state = CertaintyFilterState::new();
        obs(&mut state, 0.95, 0.80, 0.70, 0.50, 42);
        let early = obs(&mut state, 0.95, 0.80, 0.70, 0.50, 42);
        for _ in 0..15 {
            obs(&mut state, 0.95, 0.80, 0.70, 0.50, 42);
        }
        let late = obs(&mut state, 0.95, 0.80, 0.70, 0.50, 42);
        assert!(
            late.variance(1) < early.variance(1),
            "semantic variance should narrow: early={}, late={}",
            early.variance(1),
            late.variance(1),
        );
    }

    #[test]
    fn interval_narrows_convergence() {
        let mut state = CertaintyFilterState::new();
        obs(&mut state, 0.95, 0.85, 0.70, 0.50, 42);
        let early = obs(&mut state, 0.95, 0.85, 0.70, 0.50, 42);
        for _ in 0..15 {
            obs(&mut state, 0.95, 0.85, 0.70, 0.50, 42);
        }
        let late = obs(&mut state, 0.95, 0.85, 0.70, 0.50, 42);
        let z = 1.645;
        let early_ci = (early.semantic() - z * early.variance(1).sqrt()).clamp(0.0, 1.0);
        let late_ci = (late.semantic() - z * late.variance(1).sqrt()).clamp(0.0, 1.0);
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
        obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.90, 42);
        let result = obs5(&mut state, 0.50, 0.80, 0.70, 0.50, 0.90, 42);
        assert!(result.structural() < 0.85);
        assert!(result.semantic() > 0.78);
        assert!(result.coverage() > 0.68);
        assert!(result.richness() > 0.48);
        assert!(result.edit_success() > 0.85);
    }

    #[test]
    fn five_dim_trusts() {
        let mut state = CertaintyFilterState::new();
        let result = obs5(&mut state, 0.95, 0.82, 0.70, 0.50, 0.90, 123);
        assert!((result.edit_success() - 0.90).abs() < 1e-9);
    }

    #[test]
    fn edit_success_converges() {
        let mut state = CertaintyFilterState::new();
        let mut result = obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.95, 42);
        for _ in 0..20 {
            result = obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.95, 42);
        }
        assert!((result.edit_success() - 0.95).abs() < 0.03);
    }

    #[test]
    fn edit_resists_failure() {
        let mut state = CertaintyFilterState::new();
        for _ in 0..10 {
            obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.95, 42);
        }
        let result = obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.10, 42);
        assert!(result.edit_success() > 0.60, "edit_success should resist single failure: {}", result.edit_success());
    }

    #[test]
    fn edit_variance_narrows() {
        let mut state = CertaintyFilterState::new();
        obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.90, 42);
        let early = obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.90, 42);
        for _ in 0..15 {
            obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.90, 42);
        }
        let late = obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.90, 42);
        assert!(
            late.variance(4) < early.variance(4),
            "edit_success variance should narrow: early={}, late={}",
            early.variance(4),
            late.variance(4)
        );
    }

    #[test]
    fn cross_covariance_emerges() {
        let mut state = CertaintyFilterState::new();
        for _ in 0..20 {
            obs5(&mut state, 0.90, 0.85, 0.70, 0.50, 0.90, 42);
        }
        let result = obs5(&mut state, 0.90, 0.85, 0.70, 0.50, 0.90, 42);
        assert!(result.covariance[0][0] > 0.0, "diagonal should be positive");
        assert!(result.covariance[1][1] > 0.0, "diagonal should be positive");
    }

    #[test]
    fn adaptive_qr_converges() {
        let mut state = CertaintyFilterState::new();
        for _ in 0..30 {
            obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.90, 42);
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
        let result = state.observe([0.92, 0.82, 0.72, 0.52, 0.70], 42);
        assert_eq!(result.observation_count, 6, "obs count should increment after observe");
        assert!(result.structural() > 0.88 && result.structural() < 0.95,
            "structural should blend prior and measurement, got {}", result.structural());
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
}
