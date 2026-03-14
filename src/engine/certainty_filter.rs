use serde::{Deserialize, Serialize};

pub const NUM_DIMS: usize = 5;
const NUM_MODELS: usize = 3;

// Dimension indices: 0=structural, 1=semantic, 2=coverage, 3=richness, 4=edit_success

// Q[model][dim] — process noise
// [Stable, Transitional, Noisy]
const MODEL_Q: [[f64; NUM_DIMS]; NUM_MODELS] = [
    [0.0001, 0.0001, 0.00005, 0.00005, 0.001],
    [0.005, 0.005, 0.003, 0.003, 0.008],
    [0.02, 0.02, 0.015, 0.015, 0.03],
];

// R[model][dim] — measurement noise
const MODEL_R: [[f64; NUM_DIMS]; NUM_MODELS] = [
    [0.002, 0.002, 0.003, 0.005, 0.02],
    [0.008, 0.008, 0.010, 0.012, 0.04],
    [0.04, 0.04, 0.03, 0.03, 0.08],
];

const TRANSITION_MATRIX: [[f64; NUM_MODELS]; NUM_MODELS] = [
    [0.92, 0.05, 0.03],
    [0.15, 0.70, 0.15],
    [0.03, 0.05, 0.92],
];

const INITIAL_MODEL_PROBS: [f64; NUM_MODELS] = [0.40, 0.20, 0.40];

const LIKELIHOOD_FLOOR: f64 = 1e-300;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImmModelState {
    pub estimates: [f64; NUM_DIMS],
    pub variances: [f64; NUM_DIMS],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CertaintyFilterState {
    pub models: [ImmModelState; NUM_MODELS],
    pub model_probs: [f64; NUM_MODELS],
    pub observation_count: u32,
    pub content_hash: u64,
    #[serde(default)]
    pub last_edit_outcome: Option<f64>,
}

pub struct CertaintyFilterResult {
    pub estimates: [f64; NUM_DIMS],
    pub variances: [f64; NUM_DIMS],
    pub model_probs: [f64; NUM_MODELS],
    pub observation_count: u32,
}

impl CertaintyFilterResult {
    pub fn structural(&self) -> f64 { self.estimates[0] }
    pub fn semantic(&self) -> f64 { self.estimates[1] }
    pub fn coverage(&self) -> f64 { self.estimates[2] }
    pub fn richness(&self) -> f64 { self.estimates[3] }
    pub fn edit_success(&self) -> f64 { self.estimates[4] }
    pub fn structural_variance(&self) -> f64 { self.variances[0] }
    pub fn semantic_variance(&self) -> f64 { self.variances[1] }
    pub fn coverage_variance(&self) -> f64 { self.variances[2] }
    pub fn richness_variance(&self) -> f64 { self.variances[3] }
    pub fn edit_success_variance(&self) -> f64 { self.variances[4] }
}

#[cfg(target_arch = "aarch64")]
#[inline]
fn imm_kalman_update(
    mixed_est: [f64; NUM_DIMS],
    mixed_var: [f64; NUM_DIMS],
    measurement: [f64; NUM_DIMS],
    q: [f64; NUM_DIMS],
    r: [f64; NUM_DIMS],
) -> ([f64; NUM_DIMS], [f64; NUM_DIMS], [f64; NUM_DIMS]) {
    unsafe {
        use core::arch::aarch64::*;

        let mut est_out = [0.0f64; NUM_DIMS];
        let mut var_out = [0.0f64; NUM_DIMS];
        let mut lik_out = [0.0f64; NUM_DIMS];
        let one = vdupq_n_f64(1.0);

        // NEON: two pairs [0,1] and [2,3]
        for pair in 0..2 {
            let off = pair * 2;
            let est = vld1q_f64(mixed_est.as_ptr().add(off));
            let var = vld1q_f64(mixed_var.as_ptr().add(off));
            let meas = vld1q_f64(measurement.as_ptr().add(off));
            let q_vec = vld1q_f64(q.as_ptr().add(off));
            let r_vec = vld1q_f64(r.as_ptr().add(off));

            let pred_var = vaddq_f64(var, q_vec);
            let innov = vsubq_f64(meas, est);
            let s = vaddq_f64(pred_var, r_vec);
            let gain = vdivq_f64(pred_var, s);
            let new_est = vfmaq_f64(est, gain, innov);
            let new_var = vmulq_f64(vsubq_f64(one, gain), pred_var);
            let innov_sq = vmulq_f64(innov, innov);
            let innov_sq_over_s = vdivq_f64(innov_sq, s);

            vst1q_f64(est_out.as_mut_ptr().add(off), new_est);
            vst1q_f64(var_out.as_mut_ptr().add(off), new_var);
            vst1q_f64(lik_out.as_mut_ptr().add(off), innov_sq_over_s);
        }

        // Scalar: dim 4 (edit_success)
        {
            let d = 4;
            let pred_var = mixed_var[d] + q[d];
            let innov = measurement[d] - mixed_est[d];
            let s = pred_var + r[d];
            let gain = pred_var / s;
            est_out[d] = mixed_est[d] + gain * innov;
            var_out[d] = (1.0 - gain) * pred_var;
            lik_out[d] = innov * innov / s;
        }

        for est in est_out.iter_mut() {
            *est = est.clamp(0.0, 1.0);
        }

        (est_out, var_out, lik_out)
    }
}

#[cfg(not(target_arch = "aarch64"))]
#[inline]
fn imm_kalman_update(
    mixed_est: [f64; NUM_DIMS],
    mixed_var: [f64; NUM_DIMS],
    measurement: [f64; NUM_DIMS],
    q: [f64; NUM_DIMS],
    r: [f64; NUM_DIMS],
) -> ([f64; NUM_DIMS], [f64; NUM_DIMS], [f64; NUM_DIMS]) {
    let mut est_out = [0.0; NUM_DIMS];
    let mut var_out = [0.0; NUM_DIMS];
    let mut lik_out = [0.0; NUM_DIMS];
    for d in 0..NUM_DIMS {
        let pred_var = mixed_var[d] + q[d];
        let innov = measurement[d] - mixed_est[d];
        let s = pred_var + r[d];
        let gain = pred_var / s;
        est_out[d] = (mixed_est[d] + gain * innov).clamp(0.0, 1.0);
        var_out[d] = (1.0 - gain) * pred_var;
        lik_out[d] = innov * innov / s;
    }
    (est_out, var_out, lik_out)
}

impl CertaintyFilterState {
    pub fn new() -> Self {
        Self {
            models: [
                ImmModelState {
                    estimates: [0.0; NUM_DIMS],
                    variances: [1.0; NUM_DIMS],
                },
                ImmModelState {
                    estimates: [0.0; NUM_DIMS],
                    variances: [1.0; NUM_DIMS],
                },
                ImmModelState {
                    estimates: [0.0; NUM_DIMS],
                    variances: [1.0; NUM_DIMS],
                },
            ],
            model_probs: INITIAL_MODEL_PROBS,
            observation_count: 0,
            content_hash: 0,
            last_edit_outcome: None,
        }
    }

    pub fn new_with_prior(prior_estimates: [f64; NUM_DIMS], prior_variances: [f64; NUM_DIMS]) -> Self {
        Self {
            models: [
                ImmModelState {
                    estimates: prior_estimates,
                    variances: prior_variances,
                },
                ImmModelState {
                    estimates: prior_estimates,
                    variances: prior_variances,
                },
                ImmModelState {
                    estimates: prior_estimates,
                    variances: prior_variances,
                },
            ],
            model_probs: INITIAL_MODEL_PROBS,
            observation_count: 0,
            content_hash: 0,
            last_edit_outcome: None,
        }
    }

    pub fn observe(
        &mut self,
        measurement: [f64; NUM_DIMS],
        content_hash: u64,
    ) -> CertaintyFilterResult {
        if content_hash != self.content_hash {
            if self.observation_count > 0 {
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
                    model.variances = MODEL_R[j];
                }
                self.observation_count = 1;

                if measurement[0] <= 0.5 {
                    let mut min_var = [f64::MAX; NUM_DIMS];
                    for (d, mv) in min_var.iter_mut().enumerate() {
                        for model_r in &MODEL_R {
                            *mv = mv.min(model_r[d]);
                        }
                    }
                    return CertaintyFilterResult {
                        estimates: measurement,
                        variances: min_var,
                        model_probs: self.model_probs,
                        observation_count: self.observation_count,
                    };
                }
            }
        }

        // Step 1: Mixing
        let mut mixed_est = [[0.0f64; NUM_DIMS]; NUM_MODELS];
        let mut mixed_var = [[0.0f64; NUM_DIMS]; NUM_MODELS];
        let mut c_bar = [0.0f64; NUM_MODELS];

        for (j, cb) in c_bar.iter_mut().enumerate() {
            for (i, &prob) in self.model_probs.iter().enumerate() {
                *cb += TRANSITION_MATRIX[i][j] * prob;
            }
            *cb = cb.max(LIKELIHOOD_FLOOR);
        }

        for j in 0..NUM_MODELS {
            let mut mix_weights = [0.0f64; NUM_MODELS];
            for (i, mw) in mix_weights.iter_mut().enumerate() {
                *mw = TRANSITION_MATRIX[i][j] * self.model_probs[i] / c_bar[j];
            }

            for (i, &w) in mix_weights.iter().enumerate() {
                for (d, me) in mixed_est[j].iter_mut().enumerate() {
                    *me += w * self.models[i].estimates[d];
                }
            }

            for (i, &w) in mix_weights.iter().enumerate() {
                for (d, mv) in mixed_var[j].iter_mut().enumerate() {
                    let diff = self.models[i].estimates[d] - mixed_est[j][d];
                    *mv += w * (self.models[i].variances[d] + diff * diff);
                }
            }
        }

        // Step 2 & 3: Kalman update + likelihood per model (NEON on aarch64)
        let mut updated_est = [[0.0f64; NUM_DIMS]; NUM_MODELS];
        let mut updated_var = [[0.0f64; NUM_DIMS]; NUM_MODELS];
        let mut log_likelihoods = [0.0f64; NUM_MODELS];

        for j in 0..NUM_MODELS {
            let (est, var, innov_sq_over_s) =
                imm_kalman_update(mixed_est[j], mixed_var[j], measurement, MODEL_Q[j], MODEL_R[j]);
            updated_est[j] = est;
            updated_var[j] = var;

            let mut ll = 0.0f64;
            for d in 0..NUM_DIMS {
                let s = mixed_var[j][d] + MODEL_Q[j][d] + MODEL_R[j][d];
                ll += s.ln() + innov_sq_over_s[d];
            }
            log_likelihoods[j] = -0.5 * ll;
        }

        // Step 4: Model probability update (Bayes rule in log space for stability)
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

        // Store updated state
        for j in 0..NUM_MODELS {
            self.models[j].estimates = updated_est[j];
            self.models[j].variances = updated_var[j];
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

        let mut blended_var = [0.0f64; NUM_DIMS];
        for (j, &prob) in new_probs.iter().enumerate() {
            for (d, bv) in blended_var.iter_mut().enumerate() {
                let diff = updated_est[j][d] - blended_est[d];
                *bv += prob * (updated_var[j][d] + diff * diff);
            }
        }

        for be in blended_est.iter_mut() {
            *be = be.clamp(0.0, 1.0);
        }

        CertaintyFilterResult {
            estimates: blended_est,
            variances: blended_var,
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
    fn first_observation_trusts_measurement() {
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
    fn content_hash_change_resets_state() {
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
    fn converges_to_stable_estimate() {
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
    fn resists_single_outlier_after_convergence() {
        let mut state = CertaintyFilterState::new();
        for _ in 0..10 {
            obs(&mut state, 0.95, 0.82, 0.70, 0.50, 42);
        }
        let result = obs(&mut state, 0.95, 0.60, 0.70, 0.50, 42);
        assert!(result.semantic() > 0.72);
    }

    #[test]
    fn model_probs_shift_toward_stable_on_consistent_input() {
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
    fn noisy_observations_increase_noisy_model_prob() {
        let mut state = CertaintyFilterState::new();
        // All 4 dims noisy to give noisy model a clear signal
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
    fn variance_includes_model_spread() {
        let mut state = CertaintyFilterState::new();
        obs(&mut state, 0.95, 0.80, 0.70, 0.50, 42);
        let stable_result = obs(&mut state, 0.95, 0.80, 0.70, 0.50, 42);
        let mut state2 = CertaintyFilterState::new();
        obs(&mut state2, 0.95, 0.80, 0.70, 0.50, 42);
        let noisy_result = obs(&mut state2, 0.95, 0.60, 0.70, 0.50, 42);
        assert!(
            noisy_result.semantic_variance() > stable_result.semantic_variance(),
            "noisy input should produce larger blended variance"
        );
    }

    #[test]
    fn confidence_interval_narrows_with_convergence() {
        let mut state = CertaintyFilterState::new();
        obs(&mut state, 0.95, 0.85, 0.70, 0.50, 42);
        let early = obs(&mut state, 0.95, 0.85, 0.70, 0.50, 42);
        for _ in 0..15 {
            obs(&mut state, 0.95, 0.85, 0.70, 0.50, 42);
        }
        let late = obs(&mut state, 0.95, 0.85, 0.70, 0.50, 42);
        let z = 1.645;
        let early_ci = (early.semantic() - z * early.semantic_variance().sqrt()).clamp(0.0, 1.0);
        let late_ci = (late.semantic() - z * late.semantic_variance().sqrt()).clamp(0.0, 1.0);
        assert!(
            late_ci > early_ci,
            "CI should tighten: early_ci={}, late_ci={}",
            early_ci,
            late_ci
        );
    }

    #[test]
    fn all_five_dimensions_tracked_independently() {
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
    fn five_dim_first_observation_trusts_measurement() {
        let mut state = CertaintyFilterState::new();
        let result = obs5(&mut state, 0.95, 0.82, 0.70, 0.50, 0.90, 123);
        assert!((result.edit_success() - 0.90).abs() < 1e-9);
    }

    #[test]
    fn edit_success_dimension_converges() {
        let mut state = CertaintyFilterState::new();
        let mut result = obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.95, 42);
        for _ in 0..20 {
            result = obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.95, 42);
        }
        assert!((result.edit_success() - 0.95).abs() < 0.03);
    }

    #[test]
    fn edit_success_resists_single_failure() {
        let mut state = CertaintyFilterState::new();
        for _ in 0..10 {
            obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.95, 42);
        }
        let result = obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.10, 42);
        assert!(result.edit_success() > 0.60, "edit_success should resist single failure: {}", result.edit_success());
    }

    #[test]
    fn edit_success_variance_narrows_over_time() {
        let mut state = CertaintyFilterState::new();
        obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.90, 42);
        let early = obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.90, 42);
        for _ in 0..15 {
            obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.90, 42);
        }
        let late = obs5(&mut state, 0.95, 0.80, 0.70, 0.50, 0.90, 42);
        assert!(
            late.edit_success_variance() < early.edit_success_variance(),
            "edit_success variance should narrow: early={}, late={}",
            early.edit_success_variance(),
            late.edit_success_variance()
        );
    }
}
