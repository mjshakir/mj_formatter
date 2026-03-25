use crate::engine::adaptive_rules::{AdaptiveRuleBases, AdaptiveTSRuleBase};
use crate::engine::catalog::PolicyCertainty;
use crate::parser::manager::SemanticCompdbContextKind;

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct GaussianMF {
    pub center: f64,
    pub sigma: f64,
}

impl GaussianMF {
    pub const fn new(center: f64, sigma: f64) -> Self {
        Self { center, sigma }
    }

    #[inline(always)]
    pub fn membership(&self, x: f64) -> f64 {
        let z = (x - self.center) / self.sigma;
        (-0.5 * z * z).exp()
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct FuzzyVariable {
    pub low: GaussianMF,
    pub medium: GaussianMF,
    pub high: GaussianMF,
}

impl FuzzyVariable {
    pub const fn new(low: GaussianMF, medium: GaussianMF, high: GaussianMF) -> Self {
        Self { low, medium, high }
    }

    #[cfg(target_arch = "aarch64")]
    pub fn memberships(&self, x: f64) -> [f64; 3] {
        unsafe {
            use core::arch::aarch64::*;
            let x_vec   = vdupq_n_f64(x);
            let centers = vld1q_f64([self.low.center, self.medium.center].as_ptr());
            let sigmas  = vld1q_f64([self.low.sigma,  self.medium.sigma].as_ptr());
            let z       = vdivq_f64(vsubq_f64(x_vec, centers), sigmas);
            let exp_arg = vmulq_f64(vdupq_n_f64(-0.5), vmulq_f64(z, z));
            let z2c = (x - self.high.center) / self.high.sigma;
            [
                vgetq_lane_f64(exp_arg, 0).exp(),
                vgetq_lane_f64(exp_arg, 1).exp(),
                (-0.5 * z2c * z2c).exp(),
            ]
        }
    }

    #[cfg(target_arch = "x86_64")]
    pub fn memberships(&self, x: f64) -> [f64; 3] {
        unsafe {
            use core::arch::x86_64::*;
            let x_vec   = _mm_set1_pd(x);
            let centers = _mm_set_pd(self.medium.center, self.low.center);
            let sigmas  = _mm_set_pd(self.medium.sigma, self.low.sigma);
            let z       = _mm_div_pd(_mm_sub_pd(x_vec, centers), sigmas);
            let neg_half = _mm_set1_pd(-0.5);
            let exp_arg = _mm_mul_pd(neg_half, _mm_mul_pd(z, z));
            let z2c = (x - self.high.center) / self.high.sigma;
            [
                _mm_cvtsd_f64(exp_arg).exp(),
                _mm_cvtsd_f64(_mm_unpackhi_pd(exp_arg, exp_arg)).exp(),
                (-0.5 * z2c * z2c).exp(),
            ]
        }
    }

    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    pub fn memberships(&self, x: f64) -> [f64; 3] {
        [
            self.low.membership(x),
            self.medium.membership(x),
            self.high.membership(x),
        ]
    }
}

pub const fn ci_variable() -> FuzzyVariable {
    FuzzyVariable::new(
        GaussianMF::new(0.0, 0.22),
        GaussianMF::new(0.50, 0.18),
        GaussianMF::new(1.0, 0.22),
    )
}

pub const fn stability_variable() -> FuzzyVariable {
    FuzzyVariable::new(
        GaussianMF::new(0.0, 0.19),
        GaussianMF::new(0.50, 0.15),
        GaussianMF::new(1.0, 0.19),
    )
}

struct TsRule2 {
    term0: usize,
    term1: usize,
    consequent: f64,
}

struct TsRule1 {
    term: usize,
    consequent: f64,
}

fn evaluate_ts2(m0: &[f64; 3], m1: &[f64; 3], rules: &[TsRule2]) -> f64 {
    if rules.len() == 9 {
        let mut cons = [0.0f64; 9];
        for r in rules {
            cons[r.term0 * 3 + r.term1] = r.consequent;
        }
        return evaluate_ts2_f64(m0, m1, &cons);
    }
    let mut num = 0.0;
    let mut den = 0.0;
    for r in rules {
        let firing = m0[r.term0] * m1[r.term1];
        num += firing * r.consequent;
        den += firing;
    }
    if den < 1e-12 { 0.0 } else { (num / den).clamp(0.0, 1.0) }
}

pub struct AdaptiveFiringRecord {
    pub severity_firing: [f64; 9],
    pub acceptance_firing: [f64; 9],
    pub outcome_firing: [f64; 9],
    pub severity_value: f64,
    pub acceptance_value: f64,
}

#[cfg(target_arch = "aarch64")]
fn compute_firing_weights(m0: &[f64; 3], m1: &[f64; 3]) -> [f64; 9] {
    unsafe {
        use core::arch::aarch64::*;
        let mut firing = [0.0f64; 9];
        let m1_01 = vld1q_f64(m1.as_ptr());
        for i in 0..3 {
            let mi = vdupq_n_f64(m0[i]);
            vst1q_f64(firing.as_mut_ptr().add(i * 3), vmulq_f64(mi, m1_01));
            firing[i * 3 + 2] = m0[i] * m1[2];
        }
        firing
    }
}

#[cfg(target_arch = "x86_64")]
fn compute_firing_weights(m0: &[f64; 3], m1: &[f64; 3]) -> [f64; 9] {
    unsafe {
        use core::arch::x86_64::*;
        let mut firing = [0.0f64; 9];
        let m1_01 = _mm_set_pd(m1[1], m1[0]);
        for i in 0..3 {
            let mi = _mm_set1_pd(m0[i]);
            _mm_storeu_pd(firing.as_mut_ptr().add(i * 3), _mm_mul_pd(mi, m1_01));
            firing[i * 3 + 2] = m0[i] * m1[2];
        }
        firing
    }
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
fn compute_firing_weights(m0: &[f64; 3], m1: &[f64; 3]) -> [f64; 9] {
    let mut firing = [0.0f64; 9];
    for i in 0..3 {
        for j in 0..3 {
            firing[i * 3 + j] = m0[i] * m1[j];
        }
    }
    firing
}

fn evaluate_adaptive(firing: &[f64; 9], base: &AdaptiveTSRuleBase) -> f64 {
    base.evaluate(firing)
}

fn evaluate_ts1(m: &[f64; 3], rules: &[TsRule1]) -> f64 {
    let mut num = 0.0f64;
    let mut den = 0.0f64;
    for r in rules {
        let firing = m[r.term];
        num += firing * r.consequent;
        den += firing;
    }
    if den < 1e-12 { 1.0 } else { num / den }
}

// ---------------------------------------------------------------------------
// NEON f64 SIMD: 2-wide T-S evaluation (float64x2_t)
// ---------------------------------------------------------------------------

#[inline(always)]
fn evaluate_ts2_f64(m0: &[f64; 3], m1: &[f64; 3], consequents: &[f64; 9]) -> f64 {
    #[cfg(target_arch = "aarch64")]
    {
        unsafe { evaluate_ts2_f64_neon(m0, m1, consequents) }
    }
    #[cfg(target_arch = "x86_64")]
    {
        unsafe { evaluate_ts2_f64_x86(m0, m1, consequents) }
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    {
        evaluate_ts2_f64_scalar(m0, m1, consequents)
    }
}

#[cfg(any(not(any(target_arch = "aarch64", target_arch = "x86_64")), test))]
fn evaluate_ts2_f64_scalar(m0: &[f64; 3], m1: &[f64; 3], consequents: &[f64; 9]) -> f64 {
    let mut num = 0.0f64;
    let mut den = 0.0f64;
    for i0 in 0..3 {
        for i1 in 0..3 {
            let f = m0[i0] * m1[i1];
            num += f * consequents[i0 * 3 + i1];
            den += f;
        }
    }
    if den < 1e-15 { 0.0 } else { (num / den).clamp(0.0, 1.0) }
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
unsafe fn evaluate_ts2_f64_neon(m0: &[f64; 3], m1: &[f64; 3], consequents: &[f64; 9]) -> f64 {
    use core::arch::aarch64::*;
    let m1_01 = vld1q_f64([m1[0], m1[1]].as_ptr());

    let mut sum_01 = vdupq_n_f64(0.0);
    let mut sum_2 = 0.0f64;
    let mut wsum_01 = vdupq_n_f64(0.0);
    let mut wsum_2 = 0.0f64;

    for i in 0..3 {
        let mi = vdupq_n_f64(m0[i]);
        let w01 = vmulq_f64(mi, m1_01);
        let w2 = m0[i] * m1[2];

        let c01 = vld1q_f64(consequents[i * 3..].as_ptr());
        let c2 = consequents[i * 3 + 2];

        sum_01 = vfmaq_f64(sum_01, w01, c01);
        sum_2 += w2 * c2;
        wsum_01 = vaddq_f64(wsum_01, w01);
        wsum_2 += w2;
    }

    let num = vaddvq_f64(sum_01) + sum_2;
    let den = vaddvq_f64(wsum_01) + wsum_2;
    if den < 1e-15 { 0.0 } else { (num / den).clamp(0.0, 1.0) }
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn evaluate_ts2_f64_x86(m0: &[f64; 3], m1: &[f64; 3], consequents: &[f64; 9]) -> f64 {
    use core::arch::x86_64::*;
    let m1_01 = _mm_set_pd(m1[1], m1[0]);

    let mut sum_01 = _mm_setzero_pd();
    let mut sum_2 = 0.0f64;
    let mut wsum_01 = _mm_setzero_pd();
    let mut wsum_2 = 0.0f64;

    for i in 0..3 {
        let mi = _mm_set1_pd(m0[i]);
        let w01 = _mm_mul_pd(mi, m1_01);
        let w2 = m0[i] * m1[2];

        let c01 = _mm_loadu_pd(consequents[i * 3..].as_ptr());
        let c2 = consequents[i * 3 + 2];

        sum_01 = _mm_add_pd(sum_01, _mm_mul_pd(w01, c01));
        sum_2 += w2 * c2;
        wsum_01 = _mm_add_pd(wsum_01, w01);
        wsum_2 += w2;
    }

    let num = _mm_cvtsd_f64(_mm_add_sd(sum_01, _mm_unpackhi_pd(sum_01, sum_01))) + sum_2;
    let den = _mm_cvtsd_f64(_mm_add_sd(wsum_01, _mm_unpackhi_pd(wsum_01, wsum_01))) + wsum_2;
    if den < 1e-15 { 0.0 } else { (num / den).clamp(0.0, 1.0) }
}

/// Piecewise-linear interpolation: Kalman-direct replacement for T-S1 evaluations.
/// At x=0 → c0, x=0.5 → c1, x=1.0 → c2. Smooth two-segment linear.
#[inline(always)]
pub fn kalman_interp(x: f64, c0: f64, c1: f64, c2: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.5 {
        c0 + (c1 - c0) * (x * 2.0)
    } else {
        c1 + (c2 - c1) * ((x - 0.5) * 2.0)
    }
}

const VARIANCE_DAMP_BASE: [f64; 5] = [0.60, 0.50, 0.30, 0.30, 0.65];

#[inline(always)]
fn compute_regime_mod(model_probs: &[f64; 3]) -> f64 {
    let base = model_probs[0] * 0.7 + model_probs[1] * 1.0 + model_probs[2] * 1.3;

    let entropy: f64 = model_probs.iter()
        .filter(|&&p| p > 1e-10)
        .map(|&p| -p * p.ln())
        .sum();
    let max_entropy = (3.0f64).ln();
    let normalized_entropy = entropy / max_entropy;

    (base * (1.0 + 0.2 * normalized_entropy)).clamp(0.5, 1.8)
}

#[inline(always)]
fn adaptive_variance_damp(
    estimate: f64,
    variance: f64,
    dim: usize,
    model_probs: [f64; 3],
    observation_count: u32,
) -> f64 {
    let regime_mod = compute_regime_mod(&model_probs);
    let maturity = (observation_count as f64 / 5.0).clamp(0.0, 1.0);
    let coeff = (VARIANCE_DAMP_BASE[dim] * regime_mod * maturity).clamp(0.0, 0.85);
    let sigma = variance.sqrt().min(1.0);
    (estimate * (1.0 - coeff * sigma)).clamp(0.0, 1.0)
}

pub fn fuzzy_observation_converged(
    stable_model_avg: f64,
    obs_median: u32,
    file_count: usize,
) -> bool {
    if obs_median < 3 {
        return false;
    }
    // Fewer files need stricter convergence (stable model must dominate more clearly)
    // Many files (30+) → 0.42, medium (15) → 0.47, few (1-5) → 0.55
    let fc = (file_count as f64).clamp(1.0, 50.0) / 50.0;
    let threshold = kalman_interp(fc, 0.55, 0.47, 0.42);
    stable_model_avg >= threshold
}

/// Kalman-direct modulation: uses stability, edit_success, and richness estimates
/// directly from IMM filter instead of T-S evaluation.
fn modulation(certainty: &PolicyCertainty) -> f64 {
    let stab_mod = kalman_interp(certainty.stable_model_prob, 0.82, 1.00, 1.15);
    let es_mod = kalman_interp(certainty.edit_success_lower_ci(), 0.85, 1.00, 1.12);
    let rich_mod = kalman_interp(certainty.richness_lower_ci(), 0.90, 1.00, 1.08);
    stab_mod * es_mod * rich_mod
}

/// Kalman-direct trust: weighted average of semantic + coverage estimates,
/// damped by variance and modulated by stability/edit_success/richness.
pub fn fuzzy_trust_rewrite(certainty: &PolicyCertainty) -> f64 {
    let mp = certainty.model_probs();
    let oc = certainty.observation_count;
    let sem = adaptive_variance_damp(certainty.semantic, certainty.semantic_variance, 1, mp, oc);
    let cov = adaptive_variance_damp(certainty.coverage, certainty.coverage_variance, 2, mp, oc);
    let base = (0.65 * sem + 0.35 * cov).clamp(0.0, 1.0);
    (base * modulation(certainty)).clamp(0.0, 1.0)
}

/// Kalman-direct trust: structural estimate damped by variance.
pub fn fuzzy_trust_structural(certainty: &PolicyCertainty) -> f64 {
    let mp = certainty.model_probs();
    let oc = certainty.observation_count;
    let base = adaptive_variance_damp(certainty.structural, certainty.structural_variance, 0, mp, oc);
    (base * modulation(certainty)).clamp(0.0, 1.0)
}

/// Kalman-direct region batch size for clang_format: uses the file's actual
/// tree-sitter error node count to drive region splitting, modulated by Kalman
/// structural trust. Files with zero errors use whole-file formatting. Files with
/// errors get smaller regions to contain checkpoint blast radius.
pub fn fuzzy_batch_lines(certainty: &PolicyCertainty, file_error_nodes: usize) -> usize {
    if file_error_nodes == 0 {
        return usize::MAX;
    }
    let error_pressure = (file_error_nodes as f64 / 5.0).clamp(0.0, 1.0);
    let trust = if certainty.observation_count >= 3 {
        fuzzy_trust_structural(certainty)
    } else {
        0.5
    };
    let combined = (1.0 - error_pressure) * trust;
    let raw = kalman_interp(combined, 30.0, 200.0, 10_000.0);
    (raw as usize).max(10)
}

/// Kalman-direct trust: average of semantic + structural, damped by variance.
pub fn fuzzy_trust_general(certainty: &PolicyCertainty) -> f64 {
    let mp = certainty.model_probs();
    let oc = certainty.observation_count;
    let sem = adaptive_variance_damp(certainty.semantic, certainty.semantic_variance, 1, mp, oc);
    let str_val = adaptive_variance_damp(certainty.structural, certainty.structural_variance, 0, mp, oc);
    let base = (sem * 0.5 + str_val * 0.5).clamp(0.0, 1.0);
    (base * modulation(certainty)).clamp(0.0, 1.0)
}

// 2D T-S rule base: (plan_confidence × file_trust) → acceptance [0,1].
// Replaces the previous 1D sigmoid gate for rename plan filtering.
// High-confidence plans pass even at low file trust (0.55 > 0.5 threshold).
// Low-confidence plans need high file trust to pass.
const RENAME_GATE_BASE: [TsRule2; 9] = [
    TsRule2 { term0: 0, term1: 0, consequent: 0.10 },
    TsRule2 { term0: 0, term1: 1, consequent: 0.30 },
    TsRule2 { term0: 0, term1: 2, consequent: 0.55 },
    TsRule2 { term0: 1, term1: 0, consequent: 0.35 },
    TsRule2 { term0: 1, term1: 1, consequent: 0.60 },
    TsRule2 { term0: 1, term1: 2, consequent: 0.85 },
    TsRule2 { term0: 2, term1: 0, consequent: 0.55 },
    TsRule2 { term0: 2, term1: 1, consequent: 0.85 },
    TsRule2 { term0: 2, term1: 2, consequent: 1.00 },
];

pub fn fuzzy_rename_acceptance(plan_confidence: f64, file_trust: f64) -> f64 {
    let conf_var = FuzzyVariable::new(
        GaussianMF::new(0.0, 0.22),
        GaussianMF::new(0.5, 0.22),
        GaussianMF::new(1.0, 0.22),
    );
    let trust_var = ci_variable();
    let conf_m = conf_var.memberships(plan_confidence);
    let trust_m = trust_var.memberships(file_trust);
    evaluate_ts2(&conf_m, &trust_m, &RENAME_GATE_BASE).clamp(0.0, 1.0)
}

pub fn fuzzy_style_weight(uncertainty: f64) -> f64 {
    (0.45 + 0.20 * (1.0 - uncertainty)).clamp(0.35, 0.65)
}

pub fn fuzzy_confidence_weight(uncertainty: f64) -> f64 {
    (4.0 + 2.0 * (1.0 - uncertainty)).clamp(3.0, 7.0)
}

pub fn fuzzy_risk_stabilizer(risk_tier_index: usize, uncertainty: f64) -> f64 {
    let tier_base: [f64; 3] = [1.40, 0.55, -0.60];
    let base = tier_base[risk_tier_index.min(2)];
    base * uncertainty
}

pub fn fuzzy_scope_bonus(risk_tier_index: usize, scope_narrowness: f64) -> f64 {
    let bonus_table: [[f64; 3]; 3] = [
        [0.0, 0.0, 0.0],
        [0.20, 0.0, 0.0],
        [0.35, 0.10, -0.15],
    ];
    let scope_idx = if scope_narrowness < 0.3 { 0 } else if scope_narrowness < 0.7 { 1 } else { 2 };
    bonus_table[scope_idx][risk_tier_index.min(2)]
}

pub fn fuzzy_conflict_neighborhood(trust: f64, is_semantic: bool) -> usize {
    let base = if is_semantic { 8.0 } else { 4.0 };
    let adjusted = base * (0.6 + 0.8 * (1.0 - trust));
    adjusted.round().clamp(2.0, 16.0) as usize
}

pub const DEFAULT_TRUST: f64 = 0.5;

pub fn fuzzy_error_cap(trust: f64) -> usize {
    let cap = 4.0 + 12.0 * trust;
    cap.round().clamp(4.0, 16.0) as usize
}

pub fn fuzzy_ref_radius(ref_count: usize, trust: f64) -> usize {
    let normalized = (ref_count as f64 / 1024.0).clamp(0.0, 1.0);
    let raw = normalized * (1.2 - 0.4 * trust);
    kalman_interp(raw, 0.0, 1.0, 2.0).round().clamp(0.0, 2.0) as usize
}

pub fn fuzzy_edit_radius(edit_count: usize, trust: f64) -> usize {
    let normalized = (edit_count as f64 / 48.0).clamp(0.0, 1.0);
    let raw = normalized * (1.2 - 0.4 * trust);
    kalman_interp(raw, 0.0, 1.0, 2.0).round().clamp(0.0, 2.0) as usize
}

pub fn fuzzy_guard_relax(certainty: Option<&PolicyCertainty>) -> bool {
    let cert = match certainty {
        Some(c) => c,
        None => return false,
    };
    let structural = cert.structural;
    let edit_success = cert.edit_success_lower_ci();
    let stability = cert.stable_model_prob;
    let score = (structural * 0.5 + edit_success * 0.5).clamp(0.0, 1.0);
    let threshold = kalman_interp(stability, 0.85, 0.65, 0.50);
    score >= threshold
}

pub fn fuzzy_zone_relax(certainty: Option<&PolicyCertainty>) -> bool {
    let cert = match certainty {
        Some(c) => c,
        None => return false,
    };
    let semantic = cert.semantic_lower_ci();
    let structural = cert.structural;
    let stability = cert.stable_model_prob;
    let score = (semantic * 0.4 + structural * 0.6).clamp(0.0, 1.0);
    let threshold = kalman_interp(stability, 0.85, 0.65, 0.50);
    score >= threshold
}

pub fn fuzzy_constraint_override(
    certainty: Option<&PolicyCertainty>,
    clause: crate::engine::semantic_contract::SemanticInvariantClause,
) -> bool {
    use crate::engine::semantic_contract::SemanticInvariantClause;
    match clause {
        SemanticInvariantClause::ParserAvailability
        | SemanticInvariantClause::ParseQuality
        | SemanticInvariantClause::SymbolIdentity
        | SemanticInvariantClause::ScopeIntegrity
        | SemanticInvariantClause::DeclarationReferenceIntegrity
        | SemanticInvariantClause::UsageRoleConsistency => false,
        SemanticInvariantClause::EditSafety
        | SemanticInvariantClause::TouchContract
        | SemanticInvariantClause::MacroRegionSafety => {
            let cert = match certainty {
                Some(c) => c,
                None => return false,
            };
            let trust = cert.trust_for_general();
            let stability = cert.stable_model_prob;
            let trust_threshold = kalman_interp(stability, 0.90, 0.70, 0.55);
            trust >= trust_threshold
        }
    }
}

pub fn fuzzy_semantic_readiness(
    tree_error_ratio: Option<f64>,
    clang_error_count: Option<usize>,
    clang_fatal_count: Option<usize>,
    certainty: Option<&PolicyCertainty>,
) -> f64 {
    let tree_quality = tree_error_ratio
        .map(|r| (1.0 - r * 20.0).clamp(0.0, 1.0))
        .unwrap_or(0.3);
    let clang_quality = match (clang_error_count, clang_fatal_count) {
        (_, Some(f)) if f > 0 => (1.0 - f as f64 * 0.5).clamp(0.0, 0.3),
        (Some(e), _) => (1.0 - e as f64 / 16.0).clamp(0.0, 1.0),
        _ => 0.5,
    };
    let parse_quality = (tree_quality * 0.5 + clang_quality * 0.5).clamp(0.0, 1.0);

    let structural = certainty.map(|c| c.structural).unwrap_or(0.5);
    let base = (parse_quality * kalman_interp(structural, 0.20, 0.65, 0.95)).sqrt().clamp(0.0, 1.0);

    let edit_success = certainty.map(|c| c.edit_success).unwrap_or(0.5);
    let es_mod = kalman_interp(edit_success, 0.85, 1.00, 1.08);

    let stable_prob = certainty.map(|c| c.stable_model_prob).unwrap_or(0.5);
    let stable_boost = 1.0 + 0.05 * (stable_prob - 0.5).max(0.0);

    (base * es_mod * stable_boost).clamp(0.0, 1.0)
}

pub fn fuzzy_semantic_fidelity(
    context_kind: SemanticCompdbContextKind,
    certainty: Option<&PolicyCertainty>,
) -> f64 {
    let context_base = match context_kind {
        SemanticCompdbContextKind::Exact => 1.0,
        SemanticCompdbContextKind::PairedSourceHeuristic => 0.70,
        SemanticCompdbContextKind::HeaderConsensus => 0.45,
        SemanticCompdbContextKind::SourceConsensus => 0.40,
        SemanticCompdbContextKind::None => 0.10,
    };

    let certainty = match certainty {
        Some(c) => c,
        None => return context_base * 0.5,
    };

    let mp = certainty.model_probs();
    let oc = certainty.observation_count;
    let sem = adaptive_variance_damp(certainty.semantic, certainty.semantic_variance, 1, mp, oc);
    let cov = adaptive_variance_damp(certainty.coverage, certainty.coverage_variance, 2, mp, oc);
    let kalman_fidelity = (0.65 * sem + 0.35 * cov).clamp(0.0, 1.0);

    let maturity = (oc as f64 / 10.0).clamp(0.0, 1.0);
    let context_weight = 0.4 * (1.0 - 0.5 * maturity);
    let kalman_weight = 1.0 - context_weight;
    (context_base * context_weight + kalman_fidelity * kalman_weight).clamp(0.0, 1.0)
}

pub fn fuzzy_fidelity_deduction(fidelity_score: f64, trust: f64) -> u16 {
    let inv_fidelity = (1.0 - fidelity_score).clamp(0.0, 1.0);
    let base = kalman_interp(inv_fidelity, 0.0, 180.0, 380.0);
    let trust_mod = kalman_interp(trust, 0.7, 1.0, 1.2);
    (base * trust_mod).round().max(0.0) as u16
}

pub fn fuzzy_relaxation_limits(
    context_kind_index: u8,
    certainty: Option<&PolicyCertainty>,
) -> (usize, u32) {
    let (base_severe, base_weighted): (f64, f64) = match context_kind_index {
        0 => (8.0, 48.0),
        1 => (1.0, 10.0),
        _ => (0.0, 0.0),
    };
    if base_severe == 0.0 {
        return (0, 0);
    }
    let certainty = match certainty {
        Some(c) => c,
        None => return (base_severe as usize, base_weighted as u32),
    };
    let tightness = kalman_interp(certainty.semantic, 1.4, 1.0, 0.6);
    let noisy_factor = 1.0 + 0.4 * certainty.noisy_model_prob.clamp(0.0, 1.0);

    let adj_severe = (base_severe * tightness * noisy_factor).round().max(1.0) as usize;
    let adj_weighted = (base_weighted * tightness * noisy_factor).round().max(1.0) as u32;
    (adj_severe, adj_weighted)
}

pub fn fuzzy_error_tolerance(
    base: f64,
    certainty: Option<&PolicyCertainty>,
) -> f64 {
    let certainty = match certainty {
        Some(c) => c,
        None => return base,
    };
    let tightness = kalman_interp(certainty.structural, 1.6, 1.0, 0.65);

    let variance_factor = (1.0 + certainty.structural_variance.sqrt().min(0.5) * 1.5).min(2.0);
    let noisy_factor = 1.0 + 0.5 * certainty.noisy_model_prob.clamp(0.0, 1.0);

    (base * tightness * variance_factor * noisy_factor).clamp(0.001, 0.20)
}

pub fn fuzzy_transition_tols(
    context_kind: SemanticCompdbContextKind,
    base_ref: usize,
    base_scope: usize,
    certainty: Option<&PolicyCertainty>,
    edited_lines: Option<&std::collections::BTreeSet<usize>>,
) -> (usize, usize) {
    let context_extra_ref = match context_kind {
        SemanticCompdbContextKind::Exact => 0,
        SemanticCompdbContextKind::PairedSourceHeuristic => 24,
        SemanticCompdbContextKind::HeaderConsensus => 6,
        SemanticCompdbContextKind::SourceConsensus => 4,
        SemanticCompdbContextKind::None => 2,
    };
    let context_extra_scope = match context_kind {
        SemanticCompdbContextKind::Exact => 0,
        SemanticCompdbContextKind::PairedSourceHeuristic
        | SemanticCompdbContextKind::HeaderConsensus => 4,
        SemanticCompdbContextKind::SourceConsensus => 2,
        SemanticCompdbContextKind::None => 1,
    };

    let Some(lines) = edited_lines else {
        return (base_ref, base_scope);
    };
    let edit_count = lines.len();
    if edit_count < 2 {
        return (base_ref.saturating_add(context_extra_ref), base_scope.saturating_add(context_extra_scope));
    }

    let certainty = match certainty {
        Some(c) => c,
        None => return (base_ref.saturating_add(context_extra_ref), base_scope.saturating_add(context_extra_scope)),
    };

    let ref_tightness = kalman_interp(certainty.semantic, 1.5, 1.0, 0.6);
    let scope_tightness = kalman_interp(certainty.coverage, 1.4, 1.0, 0.7);

    let richness_compensation = (certainty.richness * 2.0).min(3.0) as usize;

    let adj_ref = ((base_ref.saturating_add(context_extra_ref) as f64 * ref_tightness).round() as usize)
        .saturating_add(richness_compensation);
    let adj_scope = (base_scope.saturating_add(context_extra_scope) as f64 * scope_tightness).round() as usize;

    (adj_ref, adj_scope)
}

#[derive(Clone, Copy, Debug)]
pub struct PostEditObservation {
    pub clang_diagnostics_increased: bool,
    pub tree_error_regressed: bool,
    pub culprit_locality_ratio: f64,
    pub has_rewrite_edits: bool,
    pub all_whitespace_safe: bool,
    pub exact_compdb: bool,
    pub identity_severity: f64,
    pub reference_severity: f64,
    pub scope_severity: f64,
}

fn failure_severity_inputs(obs: &PostEditObservation) -> (f64, f64) {
    let parse_base: f64 = match (obs.tree_error_regressed, obs.clang_diagnostics_increased) {
        (true, true) => 1.0,
        (true, false) => 0.70,
        (false, true) => 0.75,
        (false, false) => 0.0,
    };
    let edit_risk: f64 = if obs.has_rewrite_edits {
        1.0
    } else if !obs.all_whitespace_safe {
        0.6
    } else {
        0.2
    };
    let parse_damage = (parse_base * (0.5 + 0.5 * edit_risk)).clamp(0.0, 1.0);
    let semantic_damage = {
        let id = 0.65 * obs.identity_severity;
        let rf = 0.45 * obs.reference_severity;
        let sc = 0.25 * obs.scope_severity;
        let base = (id + rf + sc).clamp(0.0, 1.0);
        if obs.has_rewrite_edits && base > 0.0 {
            (base * 1.35).clamp(0.0, 1.0)
        } else if obs.all_whitespace_safe {
            base * 0.35
        } else {
            base
        }
    };
    (parse_damage, semantic_damage)
}

const FAILURE_SEVERITY_BASE: [TsRule2; 9] = [
    // parse_damage (term0) × semantic_damage (term1)
    TsRule2 { term0: 0, term1: 0, consequent: 0.00 },
    TsRule2 { term0: 0, term1: 1, consequent: 0.50 },
    TsRule2 { term0: 0, term1: 2, consequent: 0.85 },
    TsRule2 { term0: 1, term1: 0, consequent: 0.55 },
    TsRule2 { term0: 1, term1: 1, consequent: 0.75 },
    TsRule2 { term0: 1, term1: 2, consequent: 0.95 },
    TsRule2 { term0: 2, term1: 0, consequent: 0.90 },
    TsRule2 { term0: 2, term1: 1, consequent: 0.95 },
    TsRule2 { term0: 2, term1: 2, consequent: 1.00 },
];

fn failure_severity_adaptive(
    obs: &PostEditObservation,
    adaptive: Option<&AdaptiveTSRuleBase>,
) -> (f64, [f64; 9]) {
    let (parse_damage, semantic_damage) = failure_severity_inputs(obs);
    const DAMAGE_VAR: FuzzyVariable = FuzzyVariable::new(GaussianMF::new(0.0, 0.20), GaussianMF::new(0.45, 0.18), GaussianMF::new(1.0, 0.24));
    let damage_var = DAMAGE_VAR;
    let parse_m = damage_var.memberships(parse_damage);
    let semantic_m = damage_var.memberships(semantic_damage);
    let firing = compute_firing_weights(&parse_m, &semantic_m);
    let value = if let Some(base) = adaptive {
        evaluate_adaptive(&firing, base)
    } else {
        evaluate_ts2(&parse_m, &semantic_m, &FAILURE_SEVERITY_BASE)
    };
    (value.clamp(0.0, 1.0), firing)
}

fn fuzzy_context_reliability(context_kind: SemanticCompdbContextKind) -> f64 {
    match context_kind {
        SemanticCompdbContextKind::Exact => 1.0,
        SemanticCompdbContextKind::PairedSourceHeuristic => 0.7,
        SemanticCompdbContextKind::HeaderConsensus | SemanticCompdbContextKind::SourceConsensus => {
            0.4
        }
        SemanticCompdbContextKind::None => 0.1,
    }
}

const EDIT_ACCEPTANCE_BASE: [TsRule2; 9] = [
    TsRule2 { term0: 0, term1: 0, consequent: 0.60 },
    TsRule2 { term0: 0, term1: 1, consequent: 0.80 },
    TsRule2 { term0: 0, term1: 2, consequent: 0.95 },
    TsRule2 { term0: 1, term1: 0, consequent: 0.15 },
    TsRule2 { term0: 1, term1: 1, consequent: 0.45 },
    TsRule2 { term0: 1, term1: 2, consequent: 0.70 },
    TsRule2 { term0: 2, term1: 0, consequent: 0.02 },
    TsRule2 { term0: 2, term1: 1, consequent: 0.10 },
    TsRule2 { term0: 2, term1: 2, consequent: 0.25 },
];

const LOCALITY_MOD: [TsRule1; 3] = [
    TsRule1 { term: 0, consequent: 0.50 },
    TsRule1 { term: 1, consequent: 1.00 },
    TsRule1 { term: 2, consequent: 1.15 },
];

const CONTEXT_MOD: [TsRule1; 3] = [
    TsRule1 { term: 0, consequent: 0.70 },
    TsRule1 { term: 1, consequent: 1.00 },
    TsRule1 { term: 2, consequent: 1.10 },
];

pub fn fuzzy_edit_acceptance(
    observation: &PostEditObservation,
    certainty: Option<&PolicyCertainty>,
    context_kind: SemanticCompdbContextKind,
    adaptive: Option<&AdaptiveRuleBases>,
) -> (f64, Option<AdaptiveFiringRecord>) {
    if observation.tree_error_regressed && observation.exact_compdb && observation.has_rewrite_edits {
        let trust = certainty.map(|c| c.trust_for_general()).unwrap_or(DEFAULT_TRUST);
        return ((0.001 + 0.009 * trust).clamp(0.001, 0.01), None);
    }
    let (severity, sev_firing) = failure_severity_adaptive(
        observation,
        adaptive.map(|a| &a.failure_severity),
    );
    if severity < 0.05 {
        return (1.0, None);
    }

    let trust = if let Some(c) = certainty {
        if observation.has_rewrite_edits {
            fuzzy_trust_rewrite(c)
        } else {
            fuzzy_trust_structural(c)
        }
    } else {
        0.5
    };

    const SEVERITY_VAR: FuzzyVariable = FuzzyVariable::new(GaussianMF::new(0.0, 0.24), GaussianMF::new(0.55, 0.21), GaussianMF::new(1.0, 0.21));
    let severity_var = SEVERITY_VAR;
    let trust_var = ci_variable();
    const LOCALITY_VAR: FuzzyVariable = FuzzyVariable::new(GaussianMF::new(0.0, 0.28), GaussianMF::new(0.60, 0.19), GaussianMF::new(1.0, 0.19));
    let locality_var = LOCALITY_VAR;
    const CONTEXT_VAR: FuzzyVariable = FuzzyVariable::new(GaussianMF::new(0.0, 0.28), GaussianMF::new(0.60, 0.19), GaussianMF::new(1.0, 0.19));
    let context_var = CONTEXT_VAR;

    let sev_m = severity_var.memberships(severity);
    let trust_m = trust_var.memberships(trust);
    let loc_m = locality_var.memberships(observation.culprit_locality_ratio);
    let ctx_m = context_var.memberships(fuzzy_context_reliability(context_kind));

    let acc_firing = compute_firing_weights(&sev_m, &trust_m);
    let base = if let Some(a) = adaptive {
        evaluate_adaptive(&acc_firing, &a.edit_acceptance)
    } else {
        evaluate_ts2(&sev_m, &trust_m, &EDIT_ACCEPTANCE_BASE)
    };
    let loc_mod = evaluate_ts1(&loc_m, &LOCALITY_MOD);
    let ctx_mod = evaluate_ts1(&ctx_m, &CONTEXT_MOD);

    let combined_mod = (loc_mod * ctx_mod).sqrt();
    let value = (base * combined_mod).clamp(0.0, 1.0);
    let record = adaptive.map(|_| AdaptiveFiringRecord {
        severity_firing: sev_firing,
        acceptance_firing: acc_firing,
        outcome_firing: [0.0; 9],
        severity_value: severity,
        acceptance_value: value,
    });
    (value, record)
}

// Rule base: stability (term0) × uncertainty (term1) → normalized [0,1] consequent
// 1.0 = uncapped, 0.0 = tightest cap
const IMPACT_RADIUS_BASE: [TsRule2; 9] = [
    TsRule2 { term0: 0, term1: 0, consequent: 0.33 },
    TsRule2 { term0: 0, term1: 1, consequent: 0.17 },
    TsRule2 { term0: 0, term1: 2, consequent: 0.00 },
    TsRule2 { term0: 1, term1: 0, consequent: 0.67 },
    TsRule2 { term0: 1, term1: 1, consequent: 0.33 },
    TsRule2 { term0: 1, term1: 2, consequent: 0.17 },
    TsRule2 { term0: 2, term1: 0, consequent: 1.00 },
    TsRule2 { term0: 2, term1: 1, consequent: 0.67 },
    TsRule2 { term0: 2, term1: 2, consequent: 0.33 },
];

const RELIABILITY_MOD: [TsRule1; 3] = [
    TsRule1 { term: 0, consequent: 0.70 },
    TsRule1 { term: 1, consequent: 1.00 },
    TsRule1 { term: 2, consequent: 1.15 },
];

pub fn fuzzy_radius_cap(
    stability_score: f64,
    uncertainty: f64,
    reliability_lower: f64,
) -> Option<usize> {
    let stab_var = stability_variable();
    const UNCERT_VAR: FuzzyVariable = FuzzyVariable::new(GaussianMF::new(0.0, 0.19), GaussianMF::new(0.425, 0.17), GaussianMF::new(1.0, 0.25));
    let uncert_var = UNCERT_VAR;
    let rel_var = ci_variable();

    let stab_m = stab_var.memberships(stability_score);
    let uncert_m = uncert_var.memberships(uncertainty);
    let rel_m = rel_var.memberships(reliability_lower);

    let base = evaluate_ts2(&stab_m, &uncert_m, &IMPACT_RADIUS_BASE);
    let rel_mod = evaluate_ts1(&rel_m, &RELIABILITY_MOD);
    let raw = (base * rel_mod).clamp(0.0, 1.0);

    const CAP_VAR: FuzzyVariable = FuzzyVariable::new(GaussianMF::new(0.0, 0.21), GaussianMF::new(0.475, 0.18), GaussianMF::new(1.0, 0.23));
    let cap_var = CAP_VAR;
    let cap_m = cap_var.memberships(raw);
    let cap_rules: [TsRule1; 3] = [
        TsRule1 { term: 0, consequent: 0.0 },
        TsRule1 { term: 1, consequent: 0.5 },
        TsRule1 { term: 2, consequent: 1.0 },
    ];
    let cap_score = evaluate_ts1(&cap_m, &cap_rules).clamp(0.0, 1.0);
    const CAP_VAR2: FuzzyVariable = FuzzyVariable::new(GaussianMF::new(0.0, 0.18), GaussianMF::new(0.40, 0.16), GaussianMF::new(1.0, 0.26));
    let cap_var2 = CAP_VAR2;
    let cap_m2 = cap_var2.memberships(cap_score);
    let cap_rules2 = [
        TsRule1 { term: 0, consequent: 1.0 },
        TsRule1 { term: 1, consequent: 2.0 },
        TsRule1 { term: 2, consequent: 3.0 },
    ];
    let result = evaluate_ts1(&cap_m2, &cap_rules2).round().clamp(1.0, 3.0) as usize;
    if result >= 3 { None } else { Some(result) }
}

/// Computes the line-shift tolerance for identity migration using T-S1.
///
/// When policies insert/remove lines, symbols shift to different line numbers,
/// changing their location-based stable IDs. This tolerance allows
/// `identity_migrated_locally()` to search nearby lines (±tolerance) when
/// checking whether a "missing" stable ID has actually migrated.
///
/// Inputs:
/// - `edit_count`: number of edits applied (proxy for line-shift magnitude)
/// - `certainty`: optional Kalman filter state for trust modulation
///
/// Returns: tolerance in lines (0 = exact match only, 16 = very permissive)
pub fn fuzzy_migration_tol(
    edit_count: usize,
    certainty: Option<&PolicyCertainty>,
) -> usize {
    let trust = certainty
        .map(|c| c.trust_for_general())
        .unwrap_or(0.5);
    let edit_volume = (edit_count.min(200) as f64 / 200.0).clamp(0.0, 1.0);

    const VOL_VAR: FuzzyVariable = FuzzyVariable::new(GaussianMF::new(0.0, 0.20), GaussianMF::new(0.45, 0.18), GaussianMF::new(1.0, 0.24));
    let vol_var = VOL_VAR;
    let trust_var = ci_variable();

    let vol_m = vol_var.memberships(edit_volume);
    let trust_m = trust_var.memberships(trust);

    // T-S2: (edit_volume × trust) → tolerance [0, 1]
    // Low edits + high trust → low tolerance (few shifts expected, trust the contract)
    // High edits + low trust → high tolerance (many shifts, be permissive)
    #[rustfmt::skip]
    const TOLERANCE_BASE: [TsRule2; 9] = [
        // vol=Low × trust=Low/Med/High
        TsRule2 { term0: 0, term1: 0, consequent: 0.30 },
        TsRule2 { term0: 0, term1: 1, consequent: 0.20 },
        TsRule2 { term0: 0, term1: 2, consequent: 0.10 },
        // vol=Med × trust=Low/Med/High
        TsRule2 { term0: 1, term1: 0, consequent: 0.55 },
        TsRule2 { term0: 1, term1: 1, consequent: 0.40 },
        TsRule2 { term0: 1, term1: 2, consequent: 0.25 },
        // vol=High × trust=Low/Med/High
        TsRule2 { term0: 2, term1: 0, consequent: 0.85 },
        TsRule2 { term0: 2, term1: 1, consequent: 0.65 },
        TsRule2 { term0: 2, term1: 2, consequent: 0.40 },
    ];

    let raw = evaluate_ts2(&vol_m, &trust_m, &TOLERANCE_BASE).clamp(0.0, 1.0);
    // Map [0, 1] → [2, 16]
    let tolerance = 2.0 + raw * 14.0;
    tolerance.round() as usize
}

pub fn fuzzy_deficit_penalty(trust: f64) -> f64 {
    let low_trust = GaussianMF::new(0.0, 0.07);
    low_trust.membership(trust) * 0.075
}

// --- Part 2: Raw semantic observation ---

// Rule base: usr_ratio (term0) × error_severity (term1) → base semantic quality [0,1]
const RAW_SEMANTIC_BASE: [TsRule2; 9] = [
    TsRule2 { term0: 0, term1: 0, consequent: 0.30 },   // low USR, low errors
    TsRule2 { term0: 0, term1: 1, consequent: 0.18 },
    TsRule2 { term0: 0, term1: 2, consequent: 0.05 },   // low USR, high errors
    TsRule2 { term0: 1, term1: 0, consequent: 0.65 },
    TsRule2 { term0: 1, term1: 1, consequent: 0.50 },
    TsRule2 { term0: 1, term1: 2, consequent: 0.30 },
    TsRule2 { term0: 2, term1: 0, consequent: 0.95 },   // high USR, low errors
    TsRule2 { term0: 2, term1: 1, consequent: 0.80 },
    TsRule2 { term0: 2, term1: 2, consequent: 0.55 },   // high USR, high errors
];

// Parser availability modulation: higher tiers → higher multiplier
const PARSER_TIER_MOD: [TsRule1; 3] = [
    TsRule1 { term: 0, consequent: 0.35 },   // tree-only → strong damping
    TsRule1 { term: 1, consequent: 0.80 },   // clang without compdb
    TsRule1 { term: 2, consequent: 1.00 },   // full compdb → no damping
];

fn parser_availability_score(has_compdb: bool, clang_success: bool, tree_available: bool) -> f64 {
    if has_compdb && clang_success {
        1.0
    } else if clang_success {
        0.70
    } else if tree_available {
        0.20
    } else {
        0.0
    }
}

pub fn fuzzy_semantic_obs(
    has_compdb: bool,
    clang_success: bool,
    usr_ratio: f64,
    tree_available: bool,
    error_count: usize,
) -> f64 {
    if !tree_available && !clang_success {
        return 0.0;
    }

    let usr_var = ci_variable();
    const ERROR_VAR: FuzzyVariable = FuzzyVariable::new(GaussianMF::new(0.0, 0.20), GaussianMF::new(0.45, 0.18), GaussianMF::new(1.0, 0.24));
    let error_var = ERROR_VAR;
    const PARSER_VAR: FuzzyVariable = FuzzyVariable::new(GaussianMF::new(0.0, 0.27), GaussianMF::new(0.625, 0.17), GaussianMF::new(1.0, 0.17));
    let parser_var = PARSER_VAR;

    let error_normalized = (error_count.min(8) as f64 / 8.0).clamp(0.0, 1.0);
    let parser_score = parser_availability_score(has_compdb, clang_success, tree_available);

    let usr_m = usr_var.memberships(usr_ratio);
    let err_m = error_var.memberships(error_normalized);
    let parser_m = parser_var.memberships(parser_score);

    let base = evaluate_ts2(&usr_m, &err_m, &RAW_SEMANTIC_BASE);
    let parser_mod = evaluate_ts1(&parser_m, &PARSER_TIER_MOD);

    (base * parser_mod).clamp(0.0, 1.0)
}

pub fn fuzzy_coverage_weight(coverage: f64) -> f64 {
    // Smooth sigmoid mapping [0,1] → [0.2, 0.8]
    let sigmoid = 1.0 / (1.0 + (-10.0 * (coverage - 0.5)).exp());
    0.2 + sigmoid * 0.6
}

// --- Part 2b: Parser cross-validation ---

// Rule base: scope_agreement (term0) × error_agreement (term1) → agreement quality [0,1]
const PARSER_AGREEMENT_BASE: [TsRule2; 9] = [
    TsRule2 { term0: 0, term1: 0, consequent: 0.15 },
    TsRule2 { term0: 0, term1: 1, consequent: 0.30 },
    TsRule2 { term0: 0, term1: 2, consequent: 0.40 },
    TsRule2 { term0: 1, term1: 0, consequent: 0.35 },
    TsRule2 { term0: 1, term1: 1, consequent: 0.60 },
    TsRule2 { term0: 1, term1: 2, consequent: 0.75 },
    TsRule2 { term0: 2, term1: 0, consequent: 0.55 },
    TsRule2 { term0: 2, term1: 1, consequent: 0.80 },
    TsRule2 { term0: 2, term1: 2, consequent: 1.00 },
];

pub fn fuzzy_cross_validation(
    tree_scope_count: usize,
    clang_declaration_count: usize,
    tree_error_ratio: f64,
    clang_error_count: usize,
    staleness: usize,
) -> f64 {
    let scope_ratio = if tree_scope_count == 0 && clang_declaration_count == 0 {
        1.0
    } else if tree_scope_count == 0 || clang_declaration_count == 0 {
        0.2
    } else {
        let min = tree_scope_count.min(clang_declaration_count) as f64;
        let max = tree_scope_count.max(clang_declaration_count) as f64;
        min / max
    };

    let error_agreement: f64 = match (tree_error_ratio < 0.01, clang_error_count == 0) {
        (true, true) => 1.0,
        (false, false) => 0.7,
        _ => 0.4,
    };

    let staleness_factor = 1.0 / (1.0 + 0.15 * staleness as f64);

    let agreement_var = ci_variable();
    let scope_m = agreement_var.memberships(scope_ratio);
    let error_m = agreement_var.memberships(error_agreement);
    let base = evaluate_ts2(&scope_m, &error_m, &PARSER_AGREEMENT_BASE);
    (base * staleness_factor).clamp(0.0, 1.0)
}

// --- Part 3: Edit outcome recording ---

// Rule base: acceptance_quality (term0) × trust (term1) → outcome [0,1]
const EDIT_OUTCOME_BASE: [TsRule2; 9] = [
    TsRule2 { term0: 0, term1: 0, consequent: 0.35 },   // low quality, low trust
    TsRule2 { term0: 0, term1: 1, consequent: 0.45 },
    TsRule2 { term0: 0, term1: 2, consequent: 0.55 },   // low quality, high trust
    TsRule2 { term0: 1, term1: 0, consequent: 0.55 },
    TsRule2 { term0: 1, term1: 1, consequent: 0.70 },
    TsRule2 { term0: 1, term1: 2, consequent: 0.80 },
    TsRule2 { term0: 2, term1: 0, consequent: 0.75 },   // high quality, low trust
    TsRule2 { term0: 2, term1: 1, consequent: 0.85 },
    TsRule2 { term0: 2, term1: 2, consequent: 0.95 },   // high quality, high trust
];

const PASS_MOD: [TsRule1; 3] = [
    TsRule1 { term: 0, consequent: 0.75 },   // later pass → lower outcome
    TsRule1 { term: 1, consequent: 0.90 },
    TsRule1 { term: 2, consequent: 1.00 },   // first pass → full outcome
];

pub fn fuzzy_edit_outcome(
    pass_number: usize,
    accepted: bool,
    acceptance_score: f64,
    trust: f64,
    adaptive: Option<&AdaptiveTSRuleBase>,
) -> (f64, [f64; 9]) {
    if !accepted {
        let rejection_severity = (1.0 - acceptance_score.clamp(0.0, 1.0)) * 0.20 + 0.05;
        return (rejection_severity.clamp(0.05, 0.25), [0.0; 9]);
    }

    let quality_var = ci_variable();
    let trust_var = ci_variable();
    const PASS_VAR: FuzzyVariable = FuzzyVariable::new(GaussianMF::new(0.0, 0.26), GaussianMF::new(0.60, 0.18), GaussianMF::new(1.0, 0.18));
    let pass_var = PASS_VAR;

    let quality_m = quality_var.memberships(acceptance_score.clamp(0.0, 1.0));
    let trust_m = trust_var.memberships(trust.clamp(0.0, 1.0));
    let pass_signal = match pass_number {
        0 => 1.0,
        1 => 0.5,
        _ => 0.0,
    };
    let pass_m = pass_var.memberships(pass_signal);

    let firing = compute_firing_weights(&quality_m, &trust_m);
    let base = if let Some(ab) = adaptive {
        evaluate_adaptive(&firing, ab)
    } else {
        evaluate_ts2(&quality_m, &trust_m, &EDIT_OUTCOME_BASE)
    };
    let pass_mod = evaluate_ts1(&pass_m, &PASS_MOD);

    ((base * pass_mod).clamp(0.05, 0.95), firing)
}

// --- Part 5: Constraint penalty ---

pub fn fuzzy_constraint_penalty(constraint_count: usize, trust: f64) -> f64 {
    let severity = (constraint_count as f64 / 7.0).clamp(0.0, 1.0);
    let trust = trust.clamp(0.0, 1.0);
    // Kalman-direct: higher severity + lower trust → higher penalty
    (severity * 0.20 * (1.2 - 0.4 * trust)).clamp(0.0, 0.20)
}

// ---------------------------------------------------------------------------
// Adaptive threshold functions — replace hardcoded constants with T-S inference
// Design principle: at trust=0.5 (default), output ≈ old hardcoded value (±5%)
// ---------------------------------------------------------------------------

pub fn fuzzy_impact_cap(trust: f64) -> usize {
    kalman_interp(trust, 128.0, 256.0, 512.0).round().max(64.0) as usize
}

pub fn fuzzy_scope_cap(trust: f64) -> usize {
    kalman_interp(trust, 96.0, 192.0, 384.0).round().max(48.0) as usize
}

pub fn fuzzy_symbol_cap(trust: f64) -> usize {
    kalman_interp(trust, 64.0, 128.0, 256.0).round().max(32.0) as usize
}

pub fn fuzzy_damping_factor(trust: f64) -> f64 {
    kalman_interp(trust, 0.50, 0.30, 0.15).clamp(0.10, 0.60)
}

pub fn fuzzy_batch_dropped_cap(trust: f64) -> usize {
    kalman_interp(trust, 4.0, 8.0, 16.0).round().max(2.0) as usize
}

pub fn fuzzy_reobs_interval(trust: f64) -> usize {
    kalman_interp(trust, 2.0, 3.0, 5.0).round().max(1.0) as usize
}

/// Risk penalty by tier: replaces 0.15 (Medium) / 0.35 (High)
/// T-S2 on (tier, certainty): high certainty → lower penalty (more confident)
pub fn fuzzy_risk_penalty(tier: usize, certainty: f64) -> f64 {
    let base = match tier {
        0 => 0.00,
        1 => 0.15,
        _ => 0.35,
    };
    // Kalman-direct: higher certainty → lower penalty (more confident)
    (base * (1.3 - 0.5 * certainty.clamp(0.0, 1.0))).clamp(0.0, 0.50)
}

/// Footprint weights: replaces 0.02 (range) / 0.01 (symbol)
/// High certainty → lower weights (less penalty for large edits)
pub fn fuzzy_footprint_weights(certainty: f64) -> (f64, f64) {
    let c = certainty.clamp(0.0, 1.0);
    (
        kalman_interp(c, 0.03, 0.02, 0.01).clamp(0.005, 0.05),
        kalman_interp(c, 0.015, 0.01, 0.005).clamp(0.002, 0.025),
    )
}

pub fn fuzzy_severity_weight(severity_level: u8, trust: f64) -> u32 {
    let base = match severity_level {
        0 => 1.0,
        1 => 3.0,
        2 => 8.0,
        _ => 12.0,
    };
    // Kalman-direct: higher trust → stricter weights
    (base * kalman_interp(trust, 0.7, 1.0, 1.3)).round().max(1.0) as u32
}

pub fn fuzzy_transition_penalty(kind: u8, certainty: f64) -> u32 {
    let base = match kind {
        0 => 90.0,
        1 => 70.0,
        2 => 55.0,
        3 => 65.0,
        4 => 80.0,
        _ => 70.0,
    };
    // Kalman-direct: higher certainty → stricter penalty
    (base * kalman_interp(certainty, 0.65, 1.0, 1.30)).round().max(20.0) as u32
}

pub fn fuzzy_locality_radius(certainty: Option<&PolicyCertainty>) -> usize {
    let trust = certainty.map(|c| c.trust_for_general()).unwrap_or(DEFAULT_TRUST);
    kalman_interp(trust, 2.0, 4.0, 7.0).round().max(1.0) as usize
}

pub fn fuzzy_richness_mult(certainty: f64) -> f64 {
    kalman_interp(certainty, 2.5, 4.0, 5.5).clamp(2.0, 6.0)
}

pub fn fuzzy_style_weights(certainty: f64) -> (f64, f64, f64) {
    (
        kalman_interp(certainty, 0.50, 0.60, 0.70).clamp(0.3, 0.8),
        kalman_interp(certainty, 0.50, 0.40, 0.30).clamp(0.2, 0.6),
        kalman_interp(certainty, 0.25, 0.20, 0.15).clamp(0.1, 0.3),
    )
}

pub fn fuzzy_context_tol(kind: u8, trust: f64) -> (usize, usize) {
    let (base_ref, base_scope) = match kind {
        0 => (0, 0),
        1 => (24, 4),
        2 => (6, 4),
        3 => (4, 2),
        _ => (8, 5),
    };
    let scale = kalman_interp(trust, 1.4, 1.0, 0.7);
    (
        (base_ref as f64 * scale).round().max(0.0) as usize,
        (base_scope as f64 * scale).round().max(0.0) as usize,
    )
}

/// Semantic confidence bonus: replaces hardcoded bp additions (300, 250, 150, 180, 80)
/// clause: 0=base(300), 1=clang_success(250), 2=tree_clean(150), 3=diagnostics_clean(180), 4=usr_backed(80)
/// At trust=0.5: returns original hardcoded value.
pub fn fuzzy_confidence_bonus(clause: u8, trust: f64) -> u16 {
    let base = match clause {
        0 => 300.0,
        1 => 250.0,
        2 => 150.0,
        3 => 180.0,
        _ => 80.0,
    };
    (base * kalman_interp(trust, 0.7, 1.0, 1.25)).round().max(0.0) as u16
}

pub fn fuzzy_failure_deduction(clause: u8, trust: f64) -> u16 {
    let base = match clause {
        0 => 120.0,
        1 => 260.0,
        2 => 140.0,
        3 => 100.0,
        _ => 80.0,
    };
    (base * kalman_interp(trust, 0.6, 1.0, 1.3)).round().max(0.0) as u16
}

/// Kalman-direct readiness override: uses structural × edit_success directly,
/// modulated by stability probability from IMM filter.
#[cfg(test)]
mod tests {
    use super::*;

    const TEST_RULE_BASE: [TsRule2; 9] = [
        TsRule2 { term0: 0, term1: 0, consequent: 0.01 },
        TsRule2 { term0: 0, term1: 1, consequent: 0.03 },
        TsRule2 { term0: 0, term1: 2, consequent: 0.05 },
        TsRule2 { term0: 1, term1: 0, consequent: 0.05 },
        TsRule2 { term0: 1, term1: 1, consequent: 0.35 },
        TsRule2 { term0: 1, term1: 2, consequent: 0.50 },
        TsRule2 { term0: 2, term1: 0, consequent: 0.08 },
        TsRule2 { term0: 2, term1: 1, consequent: 0.55 },
        TsRule2 { term0: 2, term1: 2, consequent: 0.82 },
    ];

    #[test]
    fn gaussian_peaks_center() {
        let mf = GaussianMF::new(0.5, 0.20);
        assert!((mf.membership(0.5) - 1.0).abs() < 1e-9);
        assert!(mf.membership(0.3) > 0.3);
        assert!(mf.membership(0.7) > 0.3);
    }

    #[test]
    fn gaussian_near_zero() {
        let mf = GaussianMF::new(0.5, 0.15);
        assert!(mf.membership(0.0) < 0.01);
        assert!(mf.membership(1.0) < 0.01);
    }

    #[test]
    fn gaussian_symmetric_decay() {
        let mf = GaussianMF::new(0.5, 0.20);
        let left = mf.membership(0.3);
        let right = mf.membership(0.7);
        assert!((left - right).abs() < 1e-9);
        assert!(left < 1.0);
        assert!(left > 0.0);
    }

    #[test]
    fn ci_low_coverage() {
        let var = ci_variable();
        let m = var.memberships(0.05);
        assert!(m[0] > 0.9, "low membership should be high at 0.05, got {}", m[0]);
        assert!(m[1] < 0.10, "medium should be small at 0.05, got {}", m[1]);
        assert!(m[2] < 0.01, "high should be ~0 at 0.05, got {}", m[2]);
    }

    #[test]
    fn ci_high_coverage() {
        let var = ci_variable();
        let m = var.memberships(0.90);
        assert!(m[0] < 0.01, "low should be ~0 at 0.90, got {}", m[0]);
        assert!(m[1] < 0.15, "medium should be small at 0.90, got {}", m[1]);
        assert!(m[2] > 0.5, "high should be dominant at 0.90, got {}", m[2]);
    }

    #[test]
    fn ci_medium_coverage() {
        let var = ci_variable();
        let m = var.memberships(0.50);
        assert!(m[0] < 0.10, "low should be small at 0.50, got {}", m[0]);
        assert!(m[1] > 0.9, "medium should be high at 0.50, got {}", m[1]);
        assert!(m[2] < 0.10, "high should be small at 0.50, got {}", m[2]);
    }

    #[test]
    fn ts2_weighted_average() {
        let m0 = [0.0, 1.0, 0.0];
        let m1 = [0.0, 0.0, 1.0];
        let result = evaluate_ts2(&m0, &m1, &TEST_RULE_BASE);
        assert!((result - 0.50).abs() < 1e-5, "Med×High should be 0.50, got {result}");
    }

    #[test]
    fn ts2_zero_returns() {
        let m0 = [0.0, 0.0, 0.0];
        let m1 = [0.0, 0.0, 0.0];
        let result = evaluate_ts2(&m0, &m1, &TEST_RULE_BASE);
        assert!(result.abs() < 1e-5);
    }

    #[test]
    fn ts2_interpolates_rules() {
        let m0 = [0.0, 0.5, 0.5];
        let m1 = [0.0, 0.0, 1.0];
        let result = evaluate_ts2(&m0, &m1, &TEST_RULE_BASE);
        let expected = (0.5 * 0.50 + 0.5 * 0.82) / (0.5 + 0.5);
        assert!((result - expected).abs() < 1e-5, "got {result}, expected {expected}");
    }

    fn default_certainty() -> PolicyCertainty {
        PolicyCertainty {
            overall: 0.50,
            structural: 0.50,
            semantic: 0.50,
            coverage: 0.50,
            richness: 0.50,
            semantic_variance: 0.005,
            structural_variance: 0.005,
            coverage_variance: 0.005,
            richness_variance: 0.005,
            edit_success: 0.50,
            edit_success_variance: 0.005,
            stable_model_prob: 0.50,
            transitional_model_prob: 0.30,
            noisy_model_prob: 0.20,
            ..Default::default()
        }
    }

    #[test]
    fn trust_zero_coverage() {
        let c = PolicyCertainty {
            semantic: 0.90,
            coverage: 0.0,
            semantic_variance: 0.002,
            coverage_variance: 0.002,
            stable_model_prob: 0.70,
            edit_success: 0.80,
            edit_success_variance: 0.002,
            ..default_certainty()
        };
        let trust = fuzzy_trust_rewrite(&c);
        assert!(trust > 0.25 && trust < 0.65,
            "zero coverage should reduce trust but not kill it, got {}",
            trust);
    }

    #[test]
    fn trust_high_yields() {
        let c = PolicyCertainty {
            overall: 0.85,
            semantic: 0.90,
            coverage: 0.85,
            structural: 0.92,
            richness: 0.60,
            semantic_variance: 0.002,
            coverage_variance: 0.002,
            structural_variance: 0.002,
            richness_variance: 0.002,
            stable_model_prob: 0.80,
            transitional_model_prob: 0.10,
            noisy_model_prob: 0.10,
            edit_success: 0.90,
            edit_success_variance: 0.002,
            ..Default::default()
        };
        assert!(fuzzy_trust_rewrite(&c) > 0.60,
            "high everything should yield high trust, got {}",
            fuzzy_trust_rewrite(&c));
        assert!(fuzzy_trust_structural(&c) > 0.60,
            "structural trust should be high, got {}",
            fuzzy_trust_structural(&c));
    }

    #[test]
    fn trust_stable_boosts() {
        let base = PolicyCertainty {
            semantic: 0.70,
            coverage: 0.70,
            semantic_variance: 0.01,
            coverage_variance: 0.01,
            stable_model_prob: 0.30,
            edit_success: 0.50,
            edit_success_variance: 0.01,
            ..default_certainty()
        };
        let boosted = PolicyCertainty {
            stable_model_prob: 0.90,
            ..base
        };
        assert!(fuzzy_trust_rewrite(&boosted) > fuzzy_trust_rewrite(&base));
    }

    #[test]
    fn trust_edit_boosts() {
        let base = PolicyCertainty {
            semantic: 0.70,
            coverage: 0.70,
            semantic_variance: 0.01,
            coverage_variance: 0.01,
            stable_model_prob: 0.50,
            edit_success: 0.30,
            edit_success_variance: 0.01,
            ..default_certainty()
        };
        let boosted = PolicyCertainty {
            edit_success: 0.95,
            edit_success_variance: 0.001,
            ..base
        };
        assert!(fuzzy_trust_general(&boosted) > fuzzy_trust_general(&base));
    }

    #[test]
    fn trust_monotonic_semantic() {
        let low = PolicyCertainty {
            semantic: 0.30,
            semantic_variance: 0.01,
            ..default_certainty()
        };
        let high = PolicyCertainty {
            semantic: 0.90,
            semantic_variance: 0.002,
            ..default_certainty()
        };
        assert!(fuzzy_trust_rewrite(&high) > fuzzy_trust_rewrite(&low));
    }

    fn default_observation() -> PostEditObservation {
        PostEditObservation {
            clang_diagnostics_increased: false,
            tree_error_regressed: false,
            culprit_locality_ratio: 0.8,
            has_rewrite_edits: false,
            all_whitespace_safe: false,
            exact_compdb: false,
            identity_severity: 0.0,
            reference_severity: 0.0,
            scope_severity: 0.0,
        }
    }

    #[test]
    fn acceptance_no_failures() {
        let obs = default_observation();
        let score = fuzzy_edit_acceptance(&obs, None, SemanticCompdbContextKind::Exact, None).0;
        assert!((score - 1.0).abs() < 1e-9, "no failures should yield 1.0, got {score}");
    }

    #[test]
    fn acceptance_blocks_semantic() {
        let obs = PostEditObservation {
            tree_error_regressed: true,
            exact_compdb: true,
            has_rewrite_edits: true,
            ..default_observation()
        };
        let score = fuzzy_edit_acceptance(&obs, None, SemanticCompdbContextKind::Exact, None).0;
        assert!(score < 0.02, "tree error + exact compdb + semantic rewrite should yield near-zero, got {score}");
    }

    #[test]
    fn acceptance_allows_nonsemantic() {
        let obs = PostEditObservation {
            tree_error_regressed: true,
            exact_compdb: true,
            has_rewrite_edits: false,
            ..default_observation()
        };
        let score = fuzzy_edit_acceptance(&obs, None, SemanticCompdbContextKind::Exact, None).0;
        assert!(score > 0.0, "tree error + exact compdb without semantic rewrites should not hard-block, got {score}");
    }

    #[test]
    fn acceptance_identity_clang() {
        let c = PolicyCertainty {
            semantic: 0.90,
            coverage: 0.90,
            structural: 0.90,
            semantic_variance: 0.002,
            coverage_variance: 0.002,
            structural_variance: 0.002,
            richness: 0.80,
            richness_variance: 0.002,
            stable_model_prob: 0.80,
            edit_success: 0.90,
            edit_success_variance: 0.002,
            ..default_certainty()
        };
        let obs = PostEditObservation {
            identity_severity: 0.9,
            clang_diagnostics_increased: true,
            has_rewrite_edits: true,
            culprit_locality_ratio: 0.95,
            ..default_observation()
        };
        let score = fuzzy_edit_acceptance(&obs, Some(&c), SemanticCompdbContextKind::Exact, None).0;
        assert!(score < 1.0, "high identity severity + clang_diag should reduce score, got {score}");
    }

    #[test]
    fn acceptance_severity_rejects() {
        let c = PolicyCertainty {
            semantic: 0.90,
            coverage: 0.90,
            structural: 0.90,
            semantic_variance: 0.002,
            coverage_variance: 0.002,
            structural_variance: 0.002,
            richness: 0.80,
            richness_variance: 0.002,
            stable_model_prob: 0.80,
            edit_success: 0.90,
            edit_success_variance: 0.002,
            ..default_certainty()
        };
        let obs = PostEditObservation {
            identity_severity: 1.0,
            has_rewrite_edits: true,
            culprit_locality_ratio: 0.8,
            ..default_observation()
        };
        let score = fuzzy_edit_acceptance(&obs, Some(&c), SemanticCompdbContextKind::Exact, None).0;
        assert!(score < 0.80, "high severity should reduce score even with high trust, got {score}");
    }

    #[test]
    fn acceptance_scope_accepts() {
        let c = default_certainty();
        let obs = PostEditObservation {
            scope_severity: 0.5,
            culprit_locality_ratio: 0.7,
            ..default_observation()
        };
        let score = fuzzy_edit_acceptance(&obs, Some(&c), SemanticCompdbContextKind::PairedSourceHeuristic, None).0;
        assert!(score >= 0.50, "moderate scope severity with medium trust should accept, got {score}");
    }

    #[test]
    fn acceptance_whitespace_accepts() {
        let obs = PostEditObservation {
            scope_severity: 0.5,
            reference_severity: 0.5,
            all_whitespace_safe: true,
            culprit_locality_ratio: 0.5,
            ..default_observation()
        };
        let score = fuzzy_edit_acceptance(&obs, None, SemanticCompdbContextKind::Exact, None).0;
        assert!(score >= 0.50, "whitespace-safe edits should accept, got {score}");
    }

    #[test]
    fn acceptance_low_locality() {
        let c = default_certainty();
        let high_loc = PostEditObservation {
            reference_severity: 1.0,
            culprit_locality_ratio: 0.95,
            ..default_observation()
        };
        let low_loc = PostEditObservation {
            reference_severity: 1.0,
            culprit_locality_ratio: 0.1,
            ..default_observation()
        };
        let high_score = fuzzy_edit_acceptance(&high_loc, Some(&c), SemanticCompdbContextKind::Exact, None).0;
        let low_score = fuzzy_edit_acceptance(&low_loc, Some(&c), SemanticCompdbContextKind::Exact, None).0;
        assert!(high_score > low_score,
            "higher locality should yield higher acceptance ({high_score} vs {low_score})");
    }

    #[test]
    fn richness_modulates_trust() {
        let low_rich = PolicyCertainty {
            richness: 0.05,
            richness_variance: 0.001,
            semantic: 0.70,
            coverage: 0.70,
            semantic_variance: 0.01,
            coverage_variance: 0.01,
            stable_model_prob: 0.50,
            edit_success: 0.50,
            edit_success_variance: 0.01,
            ..default_certainty()
        };
        let high_rich = PolicyCertainty {
            richness: 0.90,
            richness_variance: 0.001,
            ..low_rich
        };
        let low_trust = fuzzy_trust_rewrite(&low_rich);
        let high_trust = fuzzy_trust_rewrite(&high_rich);
        assert!(
            high_trust > low_trust,
            "high richness should yield higher trust ({high_trust}) than low richness ({low_trust})"
        );
    }

    #[test]
    fn radius_high_stability() {
        let cap = fuzzy_radius_cap(0.90, 0.10, 0.80);
        assert_eq!(cap, None, "stable + low uncertainty + high reliability should be uncapped");
    }

    #[test]
    fn radius_low_stability() {
        let cap = fuzzy_radius_cap(0.30, 0.60, 0.30);
        assert_eq!(cap, Some(1), "unstable + high uncertainty + low reliability should cap to 1");
    }

    #[test]
    fn radius_medium_values() {
        let cap = fuzzy_radius_cap(0.55, 0.40, 0.50);
        assert!(cap == Some(1) || cap == Some(2), "medium stability/uncertainty should cap to 1 or 2, got {cap:?}");
    }

    #[test]
    fn deficit_zero_max() {
        let p = fuzzy_deficit_penalty(0.0);
        assert!((p - 0.075).abs() < 1e-9, "zero trust should give max penalty 0.075, got {p}");
    }

    #[test]
    fn deficit_high_none() {
        let p = fuzzy_deficit_penalty(0.50);
        assert!(p < 1e-6, "high trust should give negligible penalty, got {p}");
    }

    #[test]
    fn deficit_boundary_smooth() {
        let p_at_015 = fuzzy_deficit_penalty(0.15);
        let p_at_020 = fuzzy_deficit_penalty(0.20);
        assert!(p_at_015 > 0.0, "trust=0.15 should still have some penalty, got {p_at_015}");
        assert!(p_at_020 < 0.005, "trust=0.20 should have near-zero penalty, got {p_at_020}");
        assert!(p_at_015 > p_at_020, "penalty should decrease as trust increases");
    }

    // --- Part 2 tests: raw semantic observation ---

    #[test]
    fn raw_full_compdb() {
        let result = fuzzy_semantic_obs(true, true, 0.9, true, 0);
        assert!(result > 0.85, "full compdb + high USR + 0 errors → high quality, got {result}");
    }

    #[test]
    fn raw_no_parser() {
        let result = fuzzy_semantic_obs(false, false, 0.0, false, 0);
        assert!(result.abs() < 1e-9, "no parser → 0.0, got {result}");
    }

    #[test]
    fn raw_tree_low() {
        let result = fuzzy_semantic_obs(false, false, 0.0, true, 0);
        assert!(result < 0.25, "tree-only with no USR → low quality, got {result}");
    }

    #[test]
    fn raw_errors_reduce() {
        let no_errors = fuzzy_semantic_obs(true, true, 0.5, true, 0);
        let many_errors = fuzzy_semantic_obs(true, true, 0.5, true, 8);
        assert!(no_errors > many_errors, "errors should reduce quality: {no_errors} vs {many_errors}");
    }

    #[test]
    fn raw_monotonic_usr() {
        let low = fuzzy_semantic_obs(true, true, 0.1, true, 0);
        let high = fuzzy_semantic_obs(true, true, 0.9, true, 0);
        assert!(high > low, "higher USR ratio → higher quality: {high} vs {low}");
    }

    #[test]
    fn coverage_smooth_range() {
        let low = fuzzy_coverage_weight(0.0);
        let mid = fuzzy_coverage_weight(0.5);
        let high = fuzzy_coverage_weight(1.0);
        assert!(low >= 0.2 && low < 0.25, "low coverage weight near 0.2, got {low}");
        assert!((mid - 0.5).abs() < 0.05, "mid coverage weight near 0.5, got {mid}");
        assert!(high > 0.75 && high <= 0.8, "high coverage weight near 0.8, got {high}");
    }

    // --- Part 3 tests: edit outcome ---

    #[test]
    fn outcome_pass0_high() {
        let outcome = fuzzy_edit_outcome(0, true, 0.95, 0.9, None).0;
        assert!(outcome > 0.85, "accepted pass 0 high trust → high outcome, got {outcome}");
    }

    #[test]
    fn outcome_pass2_lower() {
        let pass0 = fuzzy_edit_outcome(0, true, 0.8, 0.7, None).0;
        let pass2 = fuzzy_edit_outcome(2, true, 0.8, 0.7, None).0;
        assert!(pass0 > pass2, "pass 0 outcome > pass 2: {pass0} vs {pass2}");
    }

    #[test]
    fn outcome_rejected_low() {
        let outcome = fuzzy_edit_outcome(0, false, 0.3, 0.5, None).0;
        assert!(outcome >= 0.05 && outcome <= 0.25, "rejected → [0.05,0.25], got {outcome}");
    }

    #[test]
    fn outcome_marginal_lower() {
        let clean = fuzzy_edit_outcome(0, true, 0.95, 0.8, None).0;
        let marginal = fuzzy_edit_outcome(0, true, 0.51, 0.8, None).0;
        assert!(clean > marginal, "clean acceptance > marginal: {clean} vs {marginal}");
    }

    // --- Part 5 tests: constraint penalty ---

    #[test]
    fn penalty_zero_constraints() {
        let p = fuzzy_constraint_penalty(0, 0.8);
        assert!(p < 0.02, "0 constraints → near-zero penalty, got {p}");
    }

    #[test]
    fn penalty_many_lowtrust() {
        let p = fuzzy_constraint_penalty(7, 0.0);
        assert!(p > 0.15, "7 constraints + low trust → high penalty, got {p}");
    }

    #[test]
    fn trust_reduces_penalty() {
        let low_trust = fuzzy_constraint_penalty(3, 0.1);
        let high_trust = fuzzy_constraint_penalty(3, 0.9);
        assert!(low_trust > high_trust, "low trust → higher penalty: {low_trust} vs {high_trust}");
    }

    #[test]
    fn severity_no_semantic() {
        let obs = PostEditObservation {
            tree_error_regressed: true,
            has_rewrite_edits: false,
            clang_diagnostics_increased: false,
            ..default_observation()
        };
        let severity = failure_severity_adaptive(&obs, None).0;
        assert!(severity >= 0.30 && severity <= 0.65,
            "tree error without semantic edits should have moderate severity ~0.4-0.6, got {severity}");
    }

    #[test]
    fn severity_semantic_full() {
        let obs = PostEditObservation {
            tree_error_regressed: true,
            has_rewrite_edits: true,
            ..default_observation()
        };
        let severity = failure_severity_adaptive(&obs, None).0;
        assert!(severity >= 0.70,
            "tree error with semantic edits should have high severity >=0.70, got {severity}");
    }

    #[test]
    fn severity_clang_full() {
        let obs = PostEditObservation {
            tree_error_regressed: true,
            has_rewrite_edits: false,
            clang_diagnostics_increased: true,
            ..default_observation()
        };
        let severity = failure_severity_adaptive(&obs, None).0;
        assert!(severity >= 0.80,
            "tree error with clang diagnostics should have high severity >=0.80, got {severity}");
    }

    #[test]
    fn migration_low_edits() {
        let certainty = PolicyCertainty::default();
        let tolerance = fuzzy_migration_tol(5, Some(&certainty));
        assert!(tolerance >= 2 && tolerance <= 6,
            "low edits + default trust should give small tolerance, got {tolerance}");
    }

    #[test]
    fn migration_high_edits() {
        let certainty = PolicyCertainty {
            overall: 0.1,
            structural: 0.1,
            semantic: 0.1,
            coverage: 0.1,
            richness: 0.1,
            edit_success: 0.1,
            ..PolicyCertainty::default()
        };
        let tolerance = fuzzy_migration_tol(180, Some(&certainty));
        assert!(tolerance >= 10,
            "high edits + low trust should give large tolerance, got {tolerance}");
    }

    #[test]
    fn migration_no_certainty() {
        let tolerance = fuzzy_migration_tol(50, None);
        assert!(tolerance >= 2 && tolerance <= 16,
            "no certainty should use default trust (0.5), got {tolerance}");
    }

    #[test]
    fn crossval_high_agreement() {
        let agreement = fuzzy_cross_validation(20, 18, 0.0, 0, 0);
        assert!(agreement > 0.8, "matching scope/decl counts should yield high agreement, got {agreement}");
    }

    #[test]
    fn crossval_low_agreement() {
        let agreement = fuzzy_cross_validation(20, 2, 0.0, 0, 0);
        assert!(agreement < 0.5, "mismatched scope/decl counts should yield low agreement, got {agreement}");
    }

    #[test]
    fn crossval_staleness_degrades() {
        let fresh = fuzzy_cross_validation(15, 15, 0.0, 0, 0);
        let stale = fuzzy_cross_validation(15, 15, 0.0, 0, 5);
        assert!(stale < fresh, "staleness should degrade agreement: fresh={fresh}, stale={stale}");
    }

    #[test]
    fn crossval_zero_staleness() {
        let agreement = fuzzy_cross_validation(10, 10, 0.0, 0, 0);
        assert!(agreement > 0.9, "zero staleness + matching counts should yield near-1.0 agreement, got {agreement}");
    }

    fn eval_severity_raw(parse_damage: f64, semantic_damage: f64) -> f64 {
        let var = FuzzyVariable::new(GaussianMF::new(0.0, 0.20), GaussianMF::new(0.45, 0.18), GaussianMF::new(1.0, 0.24));
        let m0 = var.memberships(parse_damage);
        let m1 = var.memberships(semantic_damage);
        evaluate_ts2(&m0, &m1, &FAILURE_SEVERITY_BASE)
    }

    fn eval_acceptance_raw(severity: f64, trust: f64) -> f64 {
        let sev_var = FuzzyVariable::new(GaussianMF::new(0.0, 0.24), GaussianMF::new(0.55, 0.21), GaussianMF::new(1.0, 0.21));
        let trust_var = ci_variable();
        let m0 = sev_var.memberships(severity);
        let m1 = trust_var.memberships(trust);
        evaluate_ts2(&m0, &m1, &EDIT_ACCEPTANCE_BASE)
    }

    fn eval_outcome_raw(quality: f64, trust: f64) -> f64 {
        let var = ci_variable();
        let m0 = var.memberships(quality);
        let m1 = var.memberships(trust);
        evaluate_ts2(&m0, &m1, &EDIT_OUTCOME_BASE)
    }

    #[test]
    fn severity_parse_monotonic() {
        for sem in [0.0, 0.3, 0.5, 0.7, 1.0] {
            let mut prev = -0.01;
            for pd in (0..=20).map(|i| i as f64 / 20.0) {
                let sev = eval_severity_raw(pd, sem);
                assert!(sev >= prev - 0.02,
                    "severity not monotonic: parse_damage={pd}, semantic={sem}, sev={sev} < prev={prev}");
                prev = sev;
            }
        }
    }

    #[test]
    fn severity_semantic_monotonic() {
        for pd in [0.0, 0.3, 0.5, 0.7, 1.0] {
            let mut prev = -0.01;
            for sem in (0..=20).map(|i| i as f64 / 20.0) {
                let sev = eval_severity_raw(pd, sem);
                assert!(sev >= prev - 0.02,
                    "severity not monotonic: parse_damage={pd}, semantic={sem}, sev={sev} < prev={prev}");
                prev = sev;
            }
        }
    }

    #[test]
    fn acceptance_decreases_severity() {
        for trust in [0.2, 0.5, 0.8] {
            let mut prev = 1.1;
            for sev in (0..=20).map(|i| i as f64 / 20.0) {
                let acc = eval_acceptance_raw(sev, trust);
                assert!(acc <= prev + 0.02,
                    "acceptance not decreasing: sev={sev}, trust={trust}, acc={acc} > prev={prev}");
                prev = acc;
            }
        }
    }

    #[test]
    fn acceptance_increases_trust() {
        for sev in [0.2, 0.5, 0.8] {
            let mut prev = -0.01;
            for trust in (0..=20).map(|i| i as f64 / 20.0) {
                let acc = eval_acceptance_raw(sev, trust);
                assert!(acc >= prev - 0.02,
                    "acceptance not increasing: sev={sev}, trust={trust}, acc={acc} < prev={prev}");
                prev = acc;
            }
        }
    }

    #[test]
    fn outcome_increases_quality() {
        for trust in [0.2, 0.5, 0.8] {
            let mut prev = -0.01;
            for q in (0..=20).map(|i| i as f64 / 20.0) {
                let out = eval_outcome_raw(q, trust);
                assert!(out >= prev - 0.02,
                    "outcome not increasing with quality: q={q}, trust={trust}, out={out} < prev={prev}");
                prev = out;
            }
        }
    }

    #[test]
    fn outcome_increases_trust() {
        for q in [0.2, 0.5, 0.8] {
            let mut prev = -0.01;
            for trust in (0..=20).map(|i| i as f64 / 20.0) {
                let out = eval_outcome_raw(q, trust);
                assert!(out >= prev - 0.02,
                    "outcome not increasing with trust: q={q}, trust={trust}, out={out} < prev={prev}");
                prev = out;
            }
        }
    }

    #[test]
    fn entropy_uniform_higher() {
        let dominant = compute_regime_mod(&[0.90, 0.05, 0.05]);
        let uniform = compute_regime_mod(&[0.34, 0.33, 0.33]);
        assert!(uniform > dominant,
            "uniform model probs should yield higher regime mod (more damping): uniform={uniform}, dominant={dominant}");
    }

    #[test]
    fn entropy_stable_low() {
        let stable = compute_regime_mod(&[0.95, 0.03, 0.02]);
        assert!(stable < 0.85,
            "stable-dominant should have low regime mod, got {stable}");
    }

    #[test]
    fn geometric_mean_lenient() {
        let loc_mod: f64 = 0.50;
        let ctx_mod: f64 = 0.70;
        let multiplicative = loc_mod * ctx_mod;
        let geometric = (loc_mod * ctx_mod).sqrt();
        assert!(geometric > multiplicative,
            "geometric mean should be gentler: geo={geometric}, mult={multiplicative}");
        assert!(geometric < 1.0, "geometric mean should still penalize");
    }

    #[test]
    fn neon_f64_scalar() {
        let m0 = [0.3, 0.5, 0.2];
        let m1 = [0.1, 0.6, 0.3];
        let cons = [0.1, 0.3, 0.5, 0.2, 0.4, 0.6, 0.3, 0.5, 0.8];

        let scalar = evaluate_ts2_f64_scalar(&m0, &m1, &cons);
        let neon_result = evaluate_ts2_f64(&m0, &m1, &cons);
        assert!((scalar - neon_result).abs() < 1e-12,
            "f64 NEON should match scalar: scalar={scalar}, neon={neon_result}");
    }

    #[test]
    fn fidelity_increases_maturity() {
        use crate::engine::catalog::PolicyCertainty;
        use crate::parser::manager::SemanticCompdbContextKind;

        let base = PolicyCertainty {
            semantic: 0.90,
            coverage: 0.80,
            semantic_variance: 0.005,
            coverage_variance: 0.005,
            stable_model_prob: 0.60,
            transitional_model_prob: 0.20,
            noisy_model_prob: 0.20,
            edit_success: 0.50,
            edit_success_variance: 0.01,
            ..Default::default()
        };

        let young = PolicyCertainty { observation_count: 1, ..base };
        let mature = PolicyCertainty { observation_count: 10, ..base };

        let fidelity_young = fuzzy_semantic_fidelity(
            SemanticCompdbContextKind::PairedSourceHeuristic,
            Some(&young),
        );
        let fidelity_mature = fuzzy_semantic_fidelity(
            SemanticCompdbContextKind::PairedSourceHeuristic,
            Some(&mature),
        );
        assert!(
            fidelity_mature > fidelity_young,
            "mature fidelity ({fidelity_mature:.4}) should exceed young ({fidelity_young:.4})"
        );
    }

    #[test]
    fn coverage_zero_reduces() {
        let c = PolicyCertainty {
            semantic: 0.85,
            coverage: 0.0,
            semantic_variance: 0.005,
            coverage_variance: 0.005,
            stable_model_prob: 0.60,
            edit_success: 0.70,
            edit_success_variance: 0.005,
            observation_count: 5,
            ..default_certainty()
        };
        let trust = fuzzy_trust_rewrite(&c);
        assert!(trust > 0.25,
            "zero coverage should not kill trust, got {trust}");
    }

    #[test]
    fn high_prior_damping() {
        let low_obs = PolicyCertainty {
            semantic: 0.80,
            coverage: 0.70,
            semantic_variance: 0.04,
            coverage_variance: 0.04,
            stable_model_prob: 0.50,
            edit_success: 0.50,
            edit_success_variance: 0.01,
            observation_count: 1,
            ..default_certainty()
        };
        let high_obs = PolicyCertainty {
            observation_count: 5,
            ..low_obs
        };
        let trust_low = fuzzy_trust_rewrite(&low_obs);
        let trust_high = fuzzy_trust_rewrite(&high_obs);
        assert!(trust_high < trust_low,
            "higher obs_count with high variance should damp more: obs1={trust_low:.4}, obs5={trust_high:.4}");
    }
}
