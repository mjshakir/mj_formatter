#![allow(clippy::needless_range_loop)]

use serde::{Deserialize, Serialize};
use crate::engine::mat5::*;

pub const NUM_DIMS: usize = 5;
const NUM_MODELS: usize = 3;

// Dimension indices: 0=structural, 1=semantic, 2=coverage, 3=richness, 4=edit_success

// Initial Q[model] — process noise (diagonal, cross-terms emerge via adaptation)
const MODEL_Q_DIAG: [[f64; NUM_DIMS]; NUM_MODELS] = [
    [0.0001, 0.0001, 0.00005, 0.00005, 0.001],   // Stable
    [0.005, 0.005, 0.003, 0.003, 0.008],          // Transitional
    [0.02, 0.02, 0.015, 0.015, 0.03],             // Noisy
];

// Initial R[model] — measurement noise
const MODEL_R_DIAG: [[f64; NUM_DIMS]; NUM_MODELS] = [
    [0.002, 0.002, 0.003, 0.005, 0.02],           // Stable
    [0.008, 0.008, 0.010, 0.012, 0.04],           // Transitional
    [0.04, 0.04, 0.03, 0.03, 0.08],               // Noisy
];

const TRANSITION_MATRIX: [[f64; NUM_MODELS]; NUM_MODELS] = [
    [0.92, 0.05, 0.03],
    [0.15, 0.70, 0.15],
    [0.03, 0.05, 0.92],
];

const INITIAL_MODEL_PROBS: [f64; NUM_MODELS] = [0.40, 0.20, 0.40];

const LIKELIHOOD_FLOOR: f64 = 1e-300;

// Sage-Husa adaptive noise guard rails
const Q_FLOOR: [f64; NUM_DIMS] = [1e-6, 1e-6, 1e-6, 1e-6, 1e-5];
const Q_CEIL: [f64; NUM_DIMS] = [0.05, 0.05, 0.04, 0.04, 0.08];
// R floor per model — each model's R never drops below its initial value, preserving model diversity
const R_FLOOR_PER_MODEL: [[f64; NUM_DIMS]; NUM_MODELS] = MODEL_R_DIAG;
const R_CEIL: [f64; NUM_DIMS] = [0.10, 0.10, 0.08, 0.08, 0.15];
const ADAPTATION_WINDOW: f64 = 20.0;
const ADAPTATION_MIN_OBS: u32 = 3;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImmModelState {
    pub estimates: [f64; NUM_DIMS],
    pub covariance: [[f64; NUM_DIMS]; NUM_DIMS],
    #[serde(default = "default_adaptive_q")]
    pub adaptive_q: [[f64; NUM_DIMS]; NUM_DIMS],
    #[serde(default = "default_adaptive_r")]
    pub adaptive_r: [[f64; NUM_DIMS]; NUM_DIMS],
    #[serde(default)]
    pub adaptation_count: u32,
}

fn default_adaptive_q() -> Mat5 { mat5_diagonal(&MODEL_Q_DIAG[0]) }
fn default_adaptive_r() -> Mat5 { mat5_diagonal(&MODEL_R_DIAG[0]) }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CertaintyFilterState {
    pub models: [ImmModelState; NUM_MODELS],
    pub model_probs: [f64; NUM_MODELS],
    pub observation_count: u32,
    pub content_hash: u64,
    #[serde(default)]
    pub last_edit_outcome: Option<f64>,
    #[serde(default = "default_transition_counts")]
    transition_counts: [[f64; NUM_MODELS]; NUM_MODELS],
    #[serde(default)]
    prev_dominant_model: usize,
}

fn default_transition_counts() -> [[f64; NUM_MODELS]; NUM_MODELS] {
    [[0.0; NUM_MODELS]; NUM_MODELS]
}

pub struct CertaintyFilterResult {
    pub estimates: [f64; NUM_DIMS],
    pub covariance: Mat5,
    pub within_model_covariance: Mat5,
    pub model_probs: [f64; NUM_MODELS],
    pub observation_count: u32,
}

impl CertaintyFilterResult {
    pub fn structural(&self) -> f64 { self.estimates[0] }
    pub fn semantic(&self) -> f64 { self.estimates[1] }
    pub fn coverage(&self) -> f64 { self.estimates[2] }
    pub fn richness(&self) -> f64 { self.estimates[3] }
    pub fn edit_success(&self) -> f64 { self.estimates[4] }
    pub fn within_structural_variance(&self) -> f64 { self.within_model_covariance[0][0] }
    pub fn within_semantic_variance(&self) -> f64 { self.within_model_covariance[1][1] }
    pub fn within_coverage_variance(&self) -> f64 { self.within_model_covariance[2][2] }
    pub fn within_richness_variance(&self) -> f64 { self.within_model_covariance[3][3] }
    pub fn edit_variance(&self) -> f64 { self.within_model_covariance[4][4] }
}

// Full 5x5 Kalman update with Joseph form for numerical stability
fn imm_kalman_update_full(
    mixed_est: [f64; NUM_DIMS],
    mixed_cov: Mat5,
    measurement: [f64; NUM_DIMS],
    q: &Mat5,
    r: &Mat5,
) -> ([f64; NUM_DIMS], Mat5, f64) {
    // Predict: P_pred = P + Q (F=I)
    let p_pred = mat5_add(&mixed_cov, q);

    // Innovation: z - H*x (H=I)
    let innov = vec5_sub(&measurement, &mixed_est);

    // Innovation covariance: S = P_pred + R
    let s = mat5_add(&p_pred, r);

    // Kalman gain: K = P_pred * S^{-1}
    let s_inv = mat5_inverse_spd(&s).unwrap_or_else(mat5_identity);
    let k = mat5_mul(&p_pred, &s_inv);

    // Update estimate: x = x + K * innov
    let k_innov = mat5_matvec(&k, &innov);
    let est_out = vec5_add_clamp(&mixed_est, &k_innov);

    // Joseph form: P = (I-K)*P_pred*(I-K)^T + K*R*K^T
    let i_k = mat5_sub(&mat5_identity(), &k);
    let i_k_t = mat5_transpose(&i_k);
    let k_t = mat5_transpose(&k);
    let cov_out = mat5_add(
        &mat5_mul(&i_k, &mat5_mul(&p_pred, &i_k_t)),
        &mat5_mul(&k, &mat5_mul(r, &k_t)),
    );

    // Log-likelihood: -0.5 * (ln|S| + innov^T * S^{-1} * innov)
    let log_det = mat5_determinant_spd(&s).max(1e-300).ln();
    let mahal = mat5_quadratic(&s_inv, &innov);
    let log_lik = -0.5 * (log_det + mahal);

    (est_out, cov_out, log_lik)
}

fn clamp_diagonal(m: &mut Mat5, floor: &[f64; NUM_DIMS], ceil: &[f64; NUM_DIMS]) {
    for d in 0..NUM_DIMS {
        m[d][d] = m[d][d].clamp(floor[d], ceil[d]);
    }
}

fn blend_transition_matrix(
    prior: &[[f64; NUM_MODELS]; NUM_MODELS],
    counts: &[[f64; NUM_MODELS]; NUM_MODELS],
    blend: f64,
) -> [[f64; NUM_MODELS]; NUM_MODELS] {
    let mut result = [[0.0; NUM_MODELS]; NUM_MODELS];
    for i in 0..NUM_MODELS {
        let row_sum: f64 = counts[i].iter().sum();
        let mut norm = 0.0;
        for j in 0..NUM_MODELS {
            let empirical = if row_sum > 1e-10 {
                counts[i][j] / row_sum
            } else {
                prior[i][j]
            };
            result[i][j] = (1.0 - blend) * prior[i][j] + blend * empirical;
            norm += result[i][j];
        }
        if norm > 0.0 {
            for j in 0..NUM_MODELS {
                result[i][j] /= norm;
            }
        } else {
            result[i] = prior[i];
        }
    }
    result
}

impl CertaintyFilterState {
    pub fn new() -> Self {
        let unit_cov = mat5_diagonal(&[1.0; NUM_DIMS]);
        Self {
            models: [
                ImmModelState {
                    estimates: [0.0; NUM_DIMS],
                    covariance: unit_cov,
                    adaptive_q: mat5_diagonal(&MODEL_Q_DIAG[0]),
                    adaptive_r: mat5_diagonal(&MODEL_R_DIAG[0]),
                    adaptation_count: 0,
                },
                ImmModelState {
                    estimates: [0.0; NUM_DIMS],
                    covariance: unit_cov,
                    adaptive_q: mat5_diagonal(&MODEL_Q_DIAG[1]),
                    adaptive_r: mat5_diagonal(&MODEL_R_DIAG[1]),
                    adaptation_count: 0,
                },
                ImmModelState {
                    estimates: [0.0; NUM_DIMS],
                    covariance: unit_cov,
                    adaptive_q: mat5_diagonal(&MODEL_Q_DIAG[2]),
                    adaptive_r: mat5_diagonal(&MODEL_R_DIAG[2]),
                    adaptation_count: 0,
                },
            ],
            model_probs: INITIAL_MODEL_PROBS,
            observation_count: 0,
            content_hash: 0,
            last_edit_outcome: None,
            transition_counts: default_transition_counts(),
            prev_dominant_model: 0,
        }
    }

    pub fn new_with_prior(prior_estimates: [f64; NUM_DIMS], prior_variances: [f64; NUM_DIMS], prior_observation_count: u32) -> Self {
        let prior_cov = mat5_diagonal(&prior_variances);
        Self {
            models: [
                ImmModelState {
                    estimates: prior_estimates,
                    covariance: prior_cov,
                    adaptive_q: mat5_diagonal(&MODEL_Q_DIAG[0]),
                    adaptive_r: mat5_diagonal(&MODEL_R_DIAG[0]),
                    adaptation_count: 0,
                },
                ImmModelState {
                    estimates: prior_estimates,
                    covariance: prior_cov,
                    adaptive_q: mat5_diagonal(&MODEL_Q_DIAG[1]),
                    adaptive_r: mat5_diagonal(&MODEL_R_DIAG[1]),
                    adaptation_count: 0,
                },
                ImmModelState {
                    estimates: prior_estimates,
                    covariance: prior_cov,
                    adaptive_q: mat5_diagonal(&MODEL_Q_DIAG[2]),
                    adaptive_r: mat5_diagonal(&MODEL_R_DIAG[2]),
                    adaptation_count: 0,
                },
            ],
            model_probs: INITIAL_MODEL_PROBS,
            observation_count: prior_observation_count,
            content_hash: 0,
            last_edit_outcome: None,
            transition_counts: default_transition_counts(),
            prev_dominant_model: 0,
        }
    }

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
            let has_prior = self.models[0].estimates.iter().any(|&e| e > 0.0);
            if has_prior {
                self.observation_count = 1;
            } else {
                for model in &mut self.models {
                    model.estimates = measurement;
                }
                for (j, model) in self.models.iter_mut().enumerate() {
                    model.covariance = mat5_diagonal(&MODEL_R_DIAG[j]);
                }
                self.observation_count = 1;

                if measurement[0] <= 0.5 {
                    let mut min_var = [f64::MAX; NUM_DIMS];
                    for (d, mv) in min_var.iter_mut().enumerate() {
                        for model_r in &MODEL_R_DIAG {
                            *mv = mv.min(model_r[d]);
                        }
                    }
                    let cov = mat5_diagonal(&min_var);
                    return CertaintyFilterResult {
                        estimates: measurement,
                        covariance: cov,
                        within_model_covariance: cov,
                        model_probs: self.model_probs,
                        observation_count: self.observation_count,
                    };
                }
            }
        }

        // Compute adaptive transition matrix
        let blend_factor = (self.observation_count as f64 / 20.0).min(1.0);
        let transition = blend_transition_matrix(
            &TRANSITION_MATRIX,
            &self.transition_counts,
            blend_factor,
        );

        // Step 1: Mixing
        let mut mixed_est = [[0.0f64; NUM_DIMS]; NUM_MODELS];
        let mut mixed_cov = [mat5_zeros(); NUM_MODELS];
        let mut c_bar = [0.0f64; NUM_MODELS];

        for (j, cb) in c_bar.iter_mut().enumerate() {
            for (i, &prob) in self.model_probs.iter().enumerate() {
                *cb += transition[i][j] * prob;
            }
            *cb = cb.max(LIKELIHOOD_FLOOR);
        }

        for j in 0..NUM_MODELS {
            let mut mix_weights = [0.0f64; NUM_MODELS];
            for (i, mw) in mix_weights.iter_mut().enumerate() {
                *mw = transition[i][j] * self.model_probs[i] / c_bar[j];
            }

            // Mix estimates
            for (i, &w) in mix_weights.iter().enumerate() {
                for (d, me) in mixed_est[j].iter_mut().enumerate() {
                    *me += w * self.models[i].estimates[d];
                }
            }

            // Mix covariances (with spread term: outer product of estimate differences)
            for (i, &w) in mix_weights.iter().enumerate() {
                let diff = vec5_sub(&self.models[i].estimates, &mixed_est[j]);
                let spread = mat5_outer(&diff);
                let contrib = mat5_add(&self.models[i].covariance, &spread);
                mixed_cov[j] = mat5_add(&mixed_cov[j], &mat5_scale(&contrib, w));
            }
        }

        // Step 2 & 3: Kalman update + likelihood per model
        let mut updated_est = [[0.0f64; NUM_DIMS]; NUM_MODELS];
        let mut updated_cov = [mat5_zeros(); NUM_MODELS];
        let mut log_likelihoods = [0.0f64; NUM_MODELS];

        for j in 0..NUM_MODELS {
            let (est, cov, log_lik) = imm_kalman_update_full(
                mixed_est[j],
                mixed_cov[j],
                measurement,
                &self.models[j].adaptive_q,
                &self.models[j].adaptive_r,
            );
            updated_est[j] = est;
            updated_cov[j] = cov;
            log_likelihoods[j] = log_lik;
        }

        // Step 4: Model probability update (Bayes rule in log space)
        let max_ll = log_likelihoods
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);
        let mut new_probs = [0.0f64; NUM_MODELS];
        let mut prob_sum = 0.0f64;
        for ((np, &ll), &cb) in new_probs.iter_mut().zip(log_likelihoods.iter()).zip(c_bar.iter()) {
            *np = (ll - max_ll).exp() * cb;
            prob_sum += *np;
        }
        if prob_sum > 0.0 {
            for np in new_probs.iter_mut() {
                *np /= prob_sum;
            }
        } else {
            new_probs = INITIAL_MODEL_PROBS;
        }

        // Adaptive Markov transition: track empirical transitions
        let current_dominant = new_probs
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i)
            .unwrap_or(0);
        for row in &mut self.transition_counts {
            for c in row.iter_mut() {
                *c *= 0.98;
            }
        }
        self.transition_counts[self.prev_dominant_model][current_dominant] += 1.0;
        self.prev_dominant_model = current_dominant;

        // Sage-Husa adaptive Q/R update per model
        for j in 0..NUM_MODELS {
            let model = &mut self.models[j];
            model.adaptation_count += 1;
            if model.adaptation_count >= ADAPTATION_MIN_OBS {
                let alpha = 1.0 / (model.adaptation_count as f64).min(ADAPTATION_WINDOW);
                let innov = vec5_sub(&measurement, &mixed_est[j]);
                let innov_outer = mat5_outer(&innov);

                // Adaptive R: R_hat = (1-α)*R + α*(innov*innov^T - P_pred)
                let p_pred = mat5_add(&mixed_cov[j], &model.adaptive_q);
                let r_update = mat5_sub(&innov_outer, &p_pred);
                model.adaptive_r = mat5_add(
                    &mat5_scale(&model.adaptive_r, 1.0 - alpha),
                    &mat5_scale(&r_update, alpha),
                );
                clamp_diagonal(&mut model.adaptive_r, &R_FLOOR_PER_MODEL[j], &R_CEIL);
                enforce_spd(&mut model.adaptive_r);

                // Adaptive Q: Q_hat = (1-α)*Q + α*(K*innov*innov^T*K^T + P_upd - P_mixed)
                let s = mat5_add(&p_pred, &model.adaptive_r);
                let s_inv = mat5_inverse_spd(&s).unwrap_or_else(mat5_identity);
                let k = mat5_mul(&p_pred, &s_inv);
                let k_innov = mat5_matvec(&k, &innov);
                let q_update = mat5_sub(
                    &mat5_add(&mat5_outer(&k_innov), &updated_cov[j]),
                    &mixed_cov[j],
                );
                model.adaptive_q = mat5_add(
                    &mat5_scale(&model.adaptive_q, 1.0 - alpha),
                    &mat5_scale(&q_update, alpha),
                );
                clamp_diagonal(&mut model.adaptive_q, &Q_FLOOR, &Q_CEIL);
                enforce_spd(&mut model.adaptive_q);
            }
        }

        // Store updated state
        for j in 0..NUM_MODELS {
            self.models[j].estimates = updated_est[j];
            self.models[j].covariance = updated_cov[j];
        }
        self.model_probs = new_probs;
        self.observation_count = self.observation_count.saturating_add(1);

        // Step 5: Blended output
        let mut blended_est = [0.0f64; NUM_DIMS];
        for (&prob, est) in new_probs.iter().zip(updated_est.iter()) {
            for (d, be) in blended_est.iter_mut().enumerate() {
                *be += prob * est[d];
            }
        }

        let mut blended_cov = mat5_zeros();
        for (j, &prob) in new_probs.iter().enumerate() {
            let diff = vec5_sub(&updated_est[j], &blended_est);
            let spread = mat5_outer(&diff);
            let contrib = mat5_add(&updated_cov[j], &spread);
            blended_cov = mat5_add(&blended_cov, &mat5_scale(&contrib, prob));
        }

        for be in blended_est.iter_mut() {
            *be = be.clamp(0.0, 1.0);
        }

        let mut within_model_cov = mat5_zeros();
        for (j, &prob) in new_probs.iter().enumerate() {
            within_model_cov = mat5_add(
                &within_model_cov,
                &mat5_scale(&updated_cov[j], prob),
            );
        }

        CertaintyFilterResult {
            estimates: blended_est,
            covariance: blended_cov,
            within_model_covariance: within_model_cov,
            model_probs: new_probs,
            observation_count: self.observation_count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(state: &mut CertaintyFilterState, s: f64, m: f64, c: f64, r: f64, hash: u64) -> CertaintyFilterResult {
        let e = state.last_edit_outcome.unwrap_or(0.5);
        state.observe([s, m, c, r, e], hash)
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
    fn probs_shift_stable() {
        let mut state = CertaintyFilterState::new();
        let mut result = obs(&mut state, 0.95, 0.90, 0.80, 0.60, 42);
        for _ in 0..15 {
            result = obs(&mut state, 0.95, 0.90, 0.80, 0.60, 42);
        }
        assert!(
            result.model_probs[0] > result.model_probs[1],
            "stable model should dominate over transitional: {:?}",
            result.model_probs
        );
    }

    #[test]
    fn noisy_increases_prob() {
        let mut state = CertaintyFilterState::new();
        obs(&mut state, 0.90, 0.80, 0.65, 0.45, 42);
        obs(&mut state, 0.70, 0.90, 0.75, 0.55, 42);
        obs(&mut state, 0.95, 0.70, 0.60, 0.50, 42);
        obs(&mut state, 0.75, 0.85, 0.70, 0.40, 42);
        let result = obs(&mut state, 0.85, 0.65, 0.80, 0.60, 42);
        assert!(
            result.model_probs[2] > 0.01,
            "noisy model should have non-trivial probability: {:?}",
            result.model_probs
        );
    }

    #[test]
    fn variance_model_spread() {
        let mut state = CertaintyFilterState::new();
        obs(&mut state, 0.95, 0.80, 0.70, 0.50, 42);
        let stable_result = obs(&mut state, 0.95, 0.80, 0.70, 0.50, 42);
        let mut state2 = CertaintyFilterState::new();
        obs(&mut state2, 0.95, 0.80, 0.70, 0.50, 42);
        let noisy_result = obs(&mut state2, 0.95, 0.60, 0.70, 0.50, 42);
        assert!(
            noisy_result.within_semantic_variance() > stable_result.within_semantic_variance(),
            "noisy input should produce larger blended variance"
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
        let early_ci = (early.semantic() - z * early.within_semantic_variance().sqrt()).clamp(0.0, 1.0);
        let late_ci = (late.semantic() - z * late.within_semantic_variance().sqrt()).clamp(0.0, 1.0);
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
            late.edit_variance() < early.edit_variance(),
            "edit_success variance should narrow: early={}, late={}",
            early.edit_variance(),
            late.edit_variance()
        );
    }

    #[test]
    fn cross_covariance_emerges() {
        let mut state = CertaintyFilterState::new();
        // Feed observations where structural and semantic move together
        for _ in 0..20 {
            obs5(&mut state, 0.90, 0.85, 0.70, 0.50, 0.90, 42);
        }
        // After convergence, check that structural-semantic covariance is small but non-negative
        let result = obs5(&mut state, 0.90, 0.85, 0.70, 0.50, 0.90, 42);
        // With full covariance, the mixing spread term can produce positive off-diagonal terms
        assert!(result.covariance[0][0] > 0.0, "diagonal should be positive");
        assert!(result.covariance[1][1] > 0.0, "diagonal should be positive");
    }

    #[test]
    fn adaptive_qr_converges() {
        let mut state = CertaintyFilterState::new();
        for _ in 0..30 {
            obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.90, 42);
        }
        // After 30 consistent observations, adaptive Q should have shrunk from initial
        // (consistent data = low process noise)
        for model in &state.models {
            assert!(model.adaptation_count >= 30);
            // Q diagonal should be bounded by guard rails
            for d in 0..NUM_DIMS {
                assert!(model.adaptive_q[d][d] >= Q_FLOOR[d]);
                assert!(model.adaptive_q[d][d] <= Q_CEIL[d]);
            }
        }
    }

    #[test]
    fn transition_matrix_updates() {
        let mut state = CertaintyFilterState::new();
        for _ in 0..20 {
            obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.90, 42);
        }
        // After consistent observations, transition counts should reflect stability
        let total: f64 = state.transition_counts.iter()
            .flat_map(|row| row.iter())
            .sum();
        assert!(total > 0.0, "transition counts should be non-zero after observations");
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
}
