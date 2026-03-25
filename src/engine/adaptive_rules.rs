#![allow(clippy::needless_range_loop)]

const NUM_RULES: usize = 9;

#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn rls_matvec(p: &[[f64; NUM_RULES]; NUM_RULES], phi: &[f64; NUM_RULES]) -> [f64; NUM_RULES] {
    unsafe {
        use core::arch::aarch64::*;
        let phi01 = vld1q_f64(phi.as_ptr());
        let phi23 = vld1q_f64(phi.as_ptr().add(2));
        let phi45 = vld1q_f64(phi.as_ptr().add(4));
        let phi67 = vld1q_f64(phi.as_ptr().add(6));
        let mut result = [0.0f64; NUM_RULES];
        for i in 0..NUM_RULES {
            let row = &p[i];
            let mut acc = vmulq_f64(vld1q_f64(row.as_ptr()), phi01);
            acc = vfmaq_f64(acc, vld1q_f64(row.as_ptr().add(2)), phi23);
            acc = vfmaq_f64(acc, vld1q_f64(row.as_ptr().add(4)), phi45);
            acc = vfmaq_f64(acc, vld1q_f64(row.as_ptr().add(6)), phi67);
            result[i] = vaddvq_f64(acc) + row[8] * phi[8];
        }
        result
    }
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn rls_matvec(p: &[[f64; NUM_RULES]; NUM_RULES], phi: &[f64; NUM_RULES]) -> [f64; NUM_RULES] {
    unsafe {
        use core::arch::x86_64::*;
        let phi01 = _mm_loadu_pd(phi.as_ptr());
        let phi23 = _mm_loadu_pd(phi.as_ptr().add(2));
        let phi45 = _mm_loadu_pd(phi.as_ptr().add(4));
        let phi67 = _mm_loadu_pd(phi.as_ptr().add(6));
        let mut result = [0.0f64; NUM_RULES];
        for i in 0..NUM_RULES {
            let row = &p[i];
            let mut acc = _mm_mul_pd(_mm_loadu_pd(row.as_ptr()), phi01);
            acc = _mm_add_pd(acc, _mm_mul_pd(_mm_loadu_pd(row.as_ptr().add(2)), phi23));
            acc = _mm_add_pd(acc, _mm_mul_pd(_mm_loadu_pd(row.as_ptr().add(4)), phi45));
            acc = _mm_add_pd(acc, _mm_mul_pd(_mm_loadu_pd(row.as_ptr().add(6)), phi67));
            result[i] = _mm_cvtsd_f64(_mm_add_sd(acc, _mm_unpackhi_pd(acc, acc))) + row[8] * phi[8];
        }
        result
    }
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
#[inline(always)]
fn rls_matvec(p: &[[f64; NUM_RULES]; NUM_RULES], phi: &[f64; NUM_RULES]) -> [f64; NUM_RULES] {
    let mut result = [0.0f64; NUM_RULES];
    for i in 0..NUM_RULES {
        for j in 0..NUM_RULES {
            result[i] += p[i][j] * phi[j];
        }
    }
    result
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn rls_update_p(p: &mut [[f64; NUM_RULES]; NUM_RULES], p_phi: &[f64; NUM_RULES], denom: f64, lambda: f64) {
    unsafe {
        use core::arch::aarch64::*;
        let inv_denom = vdupq_n_f64(1.0 / denom);
        let inv_lambda = vdupq_n_f64(1.0 / lambda);
        let pphi01 = vld1q_f64(p_phi.as_ptr());
        let pphi23 = vld1q_f64(p_phi.as_ptr().add(2));
        let pphi45 = vld1q_f64(p_phi.as_ptr().add(4));
        let pphi67 = vld1q_f64(p_phi.as_ptr().add(6));
        for i in 0..NUM_RULES {
            let ki = vmulq_f64(vdupq_n_f64(p_phi[i]), inv_denom);
            let row = &mut p[i];
            // (p[i][j] - p_phi[i] * p_phi[j] / denom) / lambda
            vst1q_f64(row.as_mut_ptr(),       vmulq_f64(vsubq_f64(vld1q_f64(row.as_ptr()),       vmulq_f64(ki, pphi01)), inv_lambda));
            vst1q_f64(row.as_mut_ptr().add(2), vmulq_f64(vsubq_f64(vld1q_f64(row.as_ptr().add(2)), vmulq_f64(ki, pphi23)), inv_lambda));
            vst1q_f64(row.as_mut_ptr().add(4), vmulq_f64(vsubq_f64(vld1q_f64(row.as_ptr().add(4)), vmulq_f64(ki, pphi45)), inv_lambda));
            vst1q_f64(row.as_mut_ptr().add(6), vmulq_f64(vsubq_f64(vld1q_f64(row.as_ptr().add(6)), vmulq_f64(ki, pphi67)), inv_lambda));
            row[8] = (row[8] - p_phi[i] * p_phi[8] / denom) / lambda;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn rls_update_p(p: &mut [[f64; NUM_RULES]; NUM_RULES], p_phi: &[f64; NUM_RULES], denom: f64, lambda: f64) {
    unsafe {
        use core::arch::x86_64::*;
        let inv_denom = _mm_set1_pd(1.0 / denom);
        let inv_lambda = _mm_set1_pd(1.0 / lambda);
        let pphi01 = _mm_loadu_pd(p_phi.as_ptr());
        let pphi23 = _mm_loadu_pd(p_phi.as_ptr().add(2));
        let pphi45 = _mm_loadu_pd(p_phi.as_ptr().add(4));
        let pphi67 = _mm_loadu_pd(p_phi.as_ptr().add(6));
        for i in 0..NUM_RULES {
            let ki = _mm_mul_pd(_mm_set1_pd(p_phi[i]), inv_denom);
            let row = &mut p[i];
            _mm_storeu_pd(row.as_mut_ptr(),       _mm_mul_pd(_mm_sub_pd(_mm_loadu_pd(row.as_ptr()),       _mm_mul_pd(ki, pphi01)), inv_lambda));
            _mm_storeu_pd(row.as_mut_ptr().add(2), _mm_mul_pd(_mm_sub_pd(_mm_loadu_pd(row.as_ptr().add(2)), _mm_mul_pd(ki, pphi23)), inv_lambda));
            _mm_storeu_pd(row.as_mut_ptr().add(4), _mm_mul_pd(_mm_sub_pd(_mm_loadu_pd(row.as_ptr().add(4)), _mm_mul_pd(ki, pphi45)), inv_lambda));
            _mm_storeu_pd(row.as_mut_ptr().add(6), _mm_mul_pd(_mm_sub_pd(_mm_loadu_pd(row.as_ptr().add(6)), _mm_mul_pd(ki, pphi67)), inv_lambda));
            row[8] = (row[8] - p_phi[i] * p_phi[8] / denom) / lambda;
        }
    }
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
#[inline(always)]
fn rls_update_p(p: &mut [[f64; NUM_RULES]; NUM_RULES], p_phi: &[f64; NUM_RULES], denom: f64, lambda: f64) {
    for i in 0..NUM_RULES {
        for j in 0..NUM_RULES {
            p[i][j] = (p[i][j] - p_phi[i] * p_phi[j] / denom) / lambda;
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AdaptiveTSRuleBase {
    pub consequents: [f64; NUM_RULES],
    pub initial_consequents: [f64; NUM_RULES],
    rls_p: [[f64; NUM_RULES]; NUM_RULES],
    lambda: f64,
    update_count: u32,
}

impl AdaptiveTSRuleBase {
    pub fn new(initial: [f64; NUM_RULES], lambda: f64) -> Self {
        let mut p = [[0.0f64; NUM_RULES]; NUM_RULES];
        for i in 0..NUM_RULES {
            p[i][i] = 100.0;
        }
        Self {
            consequents: initial,
            initial_consequents: initial,
            rls_p: p,
            lambda,
            update_count: 0,
        }
    }

    pub fn evaluate(&self, firing: &[f64; NUM_RULES]) -> f64 {
        let sum: f64 = firing.iter().sum();
        if sum < 1e-10 {
            return 0.5;
        }
        let result: f64 = firing
            .iter()
            .zip(&self.consequents)
            .map(|(f, c)| f * c)
            .sum::<f64>()
            / sum;
        result.clamp(0.0, 1.0)
    }

    pub fn update(&mut self, firing: &[f64; NUM_RULES], actual_outcome: f64) {
        let sum: f64 = firing.iter().sum();
        if sum < 1e-10 {
            return;
        }
        let mut phi = [0.0; NUM_RULES];
        for i in 0..NUM_RULES {
            phi[i] = firing[i] / sum;
        }

        let prediction: f64 = phi
            .iter()
            .zip(&self.consequents)
            .map(|(p, c)| p * c)
            .sum();
        let error = actual_outcome - prediction;

        // RLS: K = P*phi / (lambda + phi^T*P*phi)
        let p_phi = rls_matvec(&self.rls_p, &phi);
        let denom = self.lambda
            + phi
                .iter()
                .zip(&p_phi)
                .map(|(p, pp)| p * pp)
                .sum::<f64>();

        if denom.abs() < 1e-15 {
            return;
        }

        for i in 0..NUM_RULES {
            self.consequents[i] += (p_phi[i] / denom) * error;
        }

        // Update P: P = (P - K*phi^T*P) / lambda
        rls_update_p(&mut self.rls_p, &p_phi, denom, self.lambda);

        self.project_constraints();
        self.update_count += 1;
    }

    fn project_constraints(&mut self) {
        for c in &mut self.consequents {
            *c = c.clamp(0.0, 1.0);
        }
        // Monotonicity: within each row (trust axis), higher trust → higher value
        for row in 0..3 {
            let base = row * 3;
            if self.consequents[base + 1] < self.consequents[base] {
                let avg = (self.consequents[base] + self.consequents[base + 1]) / 2.0;
                self.consequents[base] = avg;
                self.consequents[base + 1] = avg;
            }
            if self.consequents[base + 2] < self.consequents[base + 1] {
                let avg = (self.consequents[base + 1] + self.consequents[base + 2]) / 2.0;
                self.consequents[base + 1] = avg;
                self.consequents[base + 2] = avg;
            }
        }
    }

}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AdaptiveRuleBases {
    pub failure_severity: AdaptiveTSRuleBase,
    pub edit_acceptance: AdaptiveTSRuleBase,
    pub edit_outcome: AdaptiveTSRuleBase,
}

impl AdaptiveRuleBases {
    pub fn new() -> Self {
        Self {
            failure_severity: AdaptiveTSRuleBase::new(
                [0.00, 0.50, 0.85, 0.55, 0.75, 0.95, 0.90, 0.95, 1.00],
                0.97,
            ),
            edit_acceptance: AdaptiveTSRuleBase::new(
                [0.60, 0.80, 0.95, 0.15, 0.45, 0.70, 0.02, 0.10, 0.25],
                0.97,
            ),
            edit_outcome: AdaptiveTSRuleBase::new(
                [0.35, 0.45, 0.55, 0.55, 0.70, 0.80, 0.75, 0.85, 0.95],
                0.97,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rls_converges_true() {
        let initial = [0.5; NUM_RULES];
        let target = [0.1, 0.3, 0.5, 0.2, 0.4, 0.6, 0.3, 0.5, 0.8];
        let mut rb = AdaptiveTSRuleBase::new(initial, 0.97);

        for step in 0..200 {
            // Rotate through rules with dominant firing
            let dominant = step % NUM_RULES;
            let mut firing = [0.01; NUM_RULES];
            firing[dominant] = 1.0;
            let sum: f64 = firing.iter().sum();
            let outcome: f64 = firing
                .iter()
                .zip(&target)
                .map(|(f, t)| f * t)
                .sum::<f64>()
                / sum;
            rb.update(&firing, outcome);
        }

        for i in 0..NUM_RULES {
            assert!(
                (rb.consequents[i] - target[i]).abs() < 0.15,
                "rule {i}: expected ~{}, got {}",
                target[i],
                rb.consequents[i]
            );
        }
    }

    #[test]
    fn rls_monotonicity_enforced() {
        let initial = [0.8, 0.2, 0.5, 0.8, 0.2, 0.5, 0.8, 0.2, 0.5];
        let rb = AdaptiveTSRuleBase::new(initial, 0.97);
        // After construction, project_constraints hasn't been called yet
        // Call update with identity-like firing to trigger projection
        let mut rb2 = rb.clone();
        rb2.update(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], 0.5);
        for row in 0..3 {
            let base = row * 3;
            assert!(
                rb2.consequents[base + 1] >= rb2.consequents[base],
                "monotonicity violated at row {row}: {} < {}",
                rb2.consequents[base + 1],
                rb2.consequents[base]
            );
            assert!(
                rb2.consequents[base + 2] >= rb2.consequents[base + 1],
                "monotonicity violated at row {row}: {} < {}",
                rb2.consequents[base + 2],
                rb2.consequents[base + 1]
            );
        }
    }

    #[test]
    fn rls_weighted_average() {
        let consequents = [0.1, 0.3, 0.5, 0.2, 0.4, 0.6, 0.3, 0.5, 0.8];
        let rb = AdaptiveTSRuleBase::new(consequents, 0.97);
        let firing = [0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
        let result = rb.evaluate(&firing);
        assert!(
            (result - 0.4).abs() < 1e-9,
            "single rule firing should return that consequent: got {result}"
        );
    }

    #[test]
    fn rls_zero_default() {
        let rb = AdaptiveTSRuleBase::new([0.5; NUM_RULES], 0.97);
        let result = rb.evaluate(&[0.0; NUM_RULES]);
        assert!(
            (result - 0.5).abs() < 1e-9,
            "zero firing should return 0.5, got {result}"
        );
    }

    #[test]
    fn bases_init_static() {
        let bases = AdaptiveRuleBases::new();
        assert!((bases.failure_severity.consequents[0] - 0.00).abs() < 1e-12);
        assert!((bases.failure_severity.consequents[8] - 1.00).abs() < 1e-12);
        assert!((bases.edit_acceptance.consequents[0] - 0.60).abs() < 1e-12);
        assert!((bases.edit_acceptance.consequents[6] - 0.02).abs() < 1e-12);
        assert!((bases.edit_outcome.consequents[0] - 0.35).abs() < 1e-12);
        assert!((bases.edit_outcome.consequents[8] - 0.95).abs() < 1e-12);
    }

    #[test]
    fn rls_p_reasonable() {
        let mut rb = AdaptiveTSRuleBase::new([0.5; NUM_RULES], 0.97);
        for step in 0..500 {
            let dominant = step % NUM_RULES;
            let mut firing = [0.05; NUM_RULES];
            firing[dominant] = 1.0;
            rb.update(&firing, 0.5);
        }
        for i in 0..NUM_RULES {
            assert!(
                rb.rls_p[i][i] > 0.0 && rb.rls_p[i][i] < 1e6,
                "P diagonal [{i}] should be reasonable: {}",
                rb.rls_p[i][i]
            );
        }
    }
}
