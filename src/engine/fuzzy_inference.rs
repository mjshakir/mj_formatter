use crate::engine::catalog::PolicyCertainty;
use crate::parser::manager::SemanticCompdbContextKind;

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct TrapezoidalMF {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
}

impl TrapezoidalMF {
    pub const fn new(a: f64, b: f64, c: f64, d: f64) -> Self {
        Self { a, b, c, d }
    }

    pub fn membership(&self, x: f64) -> f64 {
        if x < self.a || x > self.d {
            0.0
        } else if x >= self.b && x <= self.c {
            1.0
        } else if x < self.b {
            (x - self.a) / (self.b - self.a)
        } else {
            (self.d - x) / (self.d - self.c)
        }
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct FuzzyVariable {
    pub low: TrapezoidalMF,
    pub medium: TrapezoidalMF,
    pub high: TrapezoidalMF,
}

impl FuzzyVariable {
    pub const fn new(low: TrapezoidalMF, medium: TrapezoidalMF, high: TrapezoidalMF) -> Self {
        Self { low, medium, high }
    }

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
        TrapezoidalMF::new(0.0, 0.0, 0.15, 0.45),
        TrapezoidalMF::new(0.15, 0.35, 0.65, 0.85),
        TrapezoidalMF::new(0.55, 0.75, 1.0, 1.0),
    )
}

pub const fn stability_variable() -> FuzzyVariable {
    FuzzyVariable::new(
        TrapezoidalMF::new(0.0, 0.0, 0.20, 0.40),
        TrapezoidalMF::new(0.30, 0.45, 0.55, 0.70),
        TrapezoidalMF::new(0.60, 0.75, 1.0, 1.0),
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
        let m0f = [m0[0] as f32, m0[1] as f32, m0[2] as f32];
        let m1f = [m1[0] as f32, m1[1] as f32, m1[2] as f32];
        let mut cons = [0.0f32; 9];
        for r in rules {
            cons[r.term0 * 3 + r.term1] = r.consequent as f32;
        }
        return evaluate_ts2_f32(&m0f, &m1f, &cons) as f64;
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

fn evaluate_ts1(m: &[f64; 3], rules: &[TsRule1]) -> f64 {
    let mf = [m[0] as f32, m[1] as f32, m[2] as f32];
    if rules.len() == 3
        && rules[0].term == 0
        && rules[1].term == 1
        && rules[2].term == 2
    {
        let cons = [
            rules[0].consequent as f32,
            rules[1].consequent as f32,
            rules[2].consequent as f32,
        ];
        return evaluate_ts1_f32(&mf, &cons) as f64;
    }
    let mut num = 0.0;
    let mut den = 0.0;
    for r in rules {
        let firing = m[r.term];
        num += firing * r.consequent;
        den += firing;
    }
    if den < 1e-12 { 1.0 } else { num / den }
}

// ---------------------------------------------------------------------------
// NEON f32 SIMD: 4-wide membership + T-S evaluation
// ---------------------------------------------------------------------------

#[repr(C, align(16))]
struct Aligned16<const N: usize>([f32; N]);

#[inline(always)]
fn evaluate_ts2_f32(m0: &[f32; 3], m1: &[f32; 3], consequents: &[f32; 9]) -> f32 {
    #[cfg(target_arch = "aarch64")]
    {
        unsafe { evaluate_ts2_f32_neon(m0, m1, consequents) }
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        evaluate_ts2_f32_scalar(m0, m1, consequents)
    }
}

#[cfg(not(target_arch = "aarch64"))]
fn evaluate_ts2_f32_scalar(m0: &[f32; 3], m1: &[f32; 3], consequents: &[f32; 9]) -> f32 {
    let mut num = 0.0f32;
    let mut den = 0.0f32;
    for i0 in 0..3 {
        for i1 in 0..3 {
            let f = m0[i0] * m1[i1];
            num += f * consequents[i0 * 3 + i1];
            den += f;
        }
    }
    if den < 1e-7 { 0.0 } else { (num / den).clamp(0.0, 1.0) }
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
unsafe fn evaluate_ts2_f32_neon(m0: &[f32; 3], m1: &[f32; 3], consequents: &[f32; 9]) -> f32 {
    use core::arch::aarch64::*;
    // Build 9 firing strengths: firing[i0*3+i1] = m0[i0] * m1[i1]
    // Layout: [m0[0]*m1[0], m0[0]*m1[1], m0[0]*m1[2],
    //          m0[1]*m1[0], m0[1]*m1[1], m0[1]*m1[2],
    //          m0[2]*m1[0], m0[2]*m1[1], m0[2]*m1[2]]
    let m1v = vld1q_f32([m1[0], m1[1], m1[2], 0.0].as_ptr());
    // Row 0: m0[0] * m1[*]
    let r0 = vmulq_f32(vdupq_n_f32(m0[0]), m1v);
    // Row 1: m0[1] * m1[*]
    let r1 = vmulq_f32(vdupq_n_f32(m0[1]), m1v);
    // Row 2: m0[2] * m1[*]
    let r2 = vmulq_f32(vdupq_n_f32(m0[2]), m1v);

    // Process in two 4-wide chunks + 1 scalar
    // Chunk 1: rules 0-3 (firing[0..4], consequents[0..4])
    let f03 = Aligned16([
        vgetq_lane_f32(r0, 0), vgetq_lane_f32(r0, 1),
        vgetq_lane_f32(r0, 2), vgetq_lane_f32(r1, 0),
    ]);
    let fv0 = vld1q_f32(f03.0.as_ptr());
    let cv0 = vld1q_f32(consequents.as_ptr());
    let num0 = vmulq_f32(fv0, cv0);

    // Chunk 2: rules 4-7
    let f47 = Aligned16([
        vgetq_lane_f32(r1, 1), vgetq_lane_f32(r1, 2),
        vgetq_lane_f32(r2, 0), vgetq_lane_f32(r2, 1),
    ]);
    let fv1 = vld1q_f32(f47.0.as_ptr());
    let cv1 = vld1q_f32(consequents.as_ptr().add(4));
    let num1 = vmulq_f32(fv1, cv1);

    // Rule 8 (scalar)
    let f8 = vgetq_lane_f32(r2, 2);
    let c8 = consequents[8];

    // Sum numerator: horizontal add of num0 + num1 + f8*c8
    let sum01 = vaddq_f32(num0, num1);
    let num_total = vaddvq_f32(sum01) + f8 * c8;

    // Sum denominator: sum all firings
    let den_sum01 = vaddq_f32(fv0, fv1);
    let den_total = vaddvq_f32(den_sum01) + f8;

    if den_total < 1e-7 { 0.0 } else { (num_total / den_total).clamp(0.0, 1.0) }
}

#[inline(always)]
fn evaluate_ts1_f32(m: &[f32; 3], consequents: &[f32; 3]) -> f32 {
    let mut num = 0.0f32;
    let mut den = 0.0f32;
    for i in 0..3 {
        num += m[i] * consequents[i];
        den += m[i];
    }
    if den < 1e-7 { 1.0 } else { num / den }
}

/// Piecewise-linear interpolation: Kalman-direct replacement for T-S1 evaluations.
/// At x=0 → c0, x=0.5 → c1, x=1.0 → c2. Smooth two-segment linear.
#[inline(always)]
fn kalman_interp(x: f64, c0: f64, c1: f64, c2: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.5 {
        c0 + (c1 - c0) * (x * 2.0)
    } else {
        c1 + (c2 - c1) * ((x - 0.5) * 2.0)
    }
}

/// Variance-damped Kalman estimate: reduces trust when variance is high.
#[inline(always)]
fn variance_damp(estimate: f64, variance: f64) -> f64 {
    let sigma = variance.sqrt().min(1.0);
    (estimate * (1.0 - 0.5 * sigma)).clamp(0.0, 1.0)
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
pub fn fuzzy_trust_semantic_rewrite(certainty: &PolicyCertainty) -> f64 {
    let sem = variance_damp(certainty.semantic, certainty.semantic_variance);
    let cov = variance_damp(certainty.coverage, certainty.coverage_variance);
    let base = (sem * cov).sqrt().clamp(0.0, 1.0);
    (base * modulation(certainty)).clamp(0.0, 1.0)
}

/// Kalman-direct trust: structural estimate damped by variance.
pub fn fuzzy_trust_structural(certainty: &PolicyCertainty) -> f64 {
    let base = variance_damp(certainty.structural, certainty.structural_variance);
    (base * modulation(certainty)).clamp(0.0, 1.0)
}

/// Kalman-direct trust: average of semantic + structural, damped by variance.
pub fn fuzzy_trust_general(certainty: &PolicyCertainty) -> f64 {
    let sem = variance_damp(certainty.semantic, certainty.semantic_variance);
    let str_val = variance_damp(certainty.structural, certainty.structural_variance);
    let base = (sem * 0.5 + str_val * 0.5).clamp(0.0, 1.0);
    (base * modulation(certainty)).clamp(0.0, 1.0)
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

pub fn fuzzy_diagnostic_error_cap(trust: f64) -> usize {
    let cap = 4.0 + 12.0 * trust;
    cap.round().clamp(4.0, 16.0) as usize
}

pub fn fuzzy_reference_count_radius(ref_count: usize, trust: f64) -> usize {
    let normalized = (ref_count as f64 / 1024.0).clamp(0.0, 1.0);
    let raw = normalized * (1.2 - 0.4 * trust);
    kalman_interp(raw, 0.0, 1.0, 2.0).round().clamp(0.0, 2.0) as usize
}

pub fn fuzzy_edit_count_radius(edit_count: usize, trust: f64) -> usize {
    let normalized = (edit_count as f64 / 48.0).clamp(0.0, 1.0);
    let raw = normalized * (1.2 - 0.4 * trust);
    kalman_interp(raw, 0.0, 1.0, 2.0).round().clamp(0.0, 2.0) as usize
}

pub fn fuzzy_edit_guard_relax(certainty: Option<&PolicyCertainty>) -> bool {
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

pub fn fuzzy_guardian_zone_relax(certainty: Option<&PolicyCertainty>) -> bool {
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

pub fn fuzzy_hard_constraint_override(
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

    let sem = variance_damp(certainty.semantic, certainty.semantic_variance);
    let cov = variance_damp(certainty.coverage, certainty.coverage_variance);
    let kalman_fidelity = (sem * cov).sqrt().clamp(0.0, 1.0);

    (context_base * 0.4 + kalman_fidelity * 0.6).clamp(0.0, 1.0)
}

pub fn fuzzy_fidelity_deduction(fidelity_score: f64, trust: f64) -> u16 {
    let inv_fidelity = (1.0 - fidelity_score).clamp(0.0, 1.0);
    let base = kalman_interp(inv_fidelity, 0.0, 180.0, 380.0);
    let trust_mod = kalman_interp(trust, 0.7, 1.0, 1.2);
    (base * trust_mod).round().max(0.0) as u16
}

pub fn fuzzy_diagnostic_relaxation_limits(
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

pub fn fuzzy_tree_error_ratio_tolerance(
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

pub fn fuzzy_semantic_transition_tolerances(
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
    pub has_semantic_rewrite_edits: bool,
    pub all_edits_whitespace_safe: bool,
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
    let edit_risk: f64 = if obs.has_semantic_rewrite_edits {
        1.0
    } else if !obs.all_edits_whitespace_safe {
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
        if obs.has_semantic_rewrite_edits && base > 0.0 {
            (base * 1.35).clamp(0.0, 1.0)
        } else if obs.all_edits_whitespace_safe {
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

fn failure_severity(obs: &PostEditObservation) -> f64 {
    let (parse_damage, semantic_damage) = failure_severity_inputs(obs);
    const DAMAGE_VAR: FuzzyVariable = FuzzyVariable::new(TrapezoidalMF::new(0.0, 0.0, 0.15, 0.35), TrapezoidalMF::new(0.20, 0.35, 0.55, 0.70), TrapezoidalMF::new(0.55, 0.65, 1.0, 1.0));
    let damage_var = DAMAGE_VAR;
    let parse_m = damage_var.memberships(parse_damage);
    let semantic_m = damage_var.memberships(semantic_damage);
    evaluate_ts2(&parse_m, &semantic_m, &FAILURE_SEVERITY_BASE).clamp(0.0, 1.0)
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
) -> f64 {
    if observation.tree_error_regressed && observation.exact_compdb && observation.has_semantic_rewrite_edits {
        let trust = certainty.map(|c| c.trust_for_general()).unwrap_or(DEFAULT_TRUST);
        return (0.001 + 0.009 * trust).clamp(0.001, 0.01);
    }
    let severity = failure_severity(observation);
    if severity < 1e-9 {
        return 1.0;
    }

    let trust = if let Some(c) = certainty {
        if observation.has_semantic_rewrite_edits {
            fuzzy_trust_semantic_rewrite(c)
        } else {
            fuzzy_trust_structural(c)
        }
    } else {
        0.5
    };

    const SEVERITY_VAR: FuzzyVariable = FuzzyVariable::new(TrapezoidalMF::new(0.0, 0.0, 0.25, 0.45), TrapezoidalMF::new(0.25, 0.45, 0.65, 0.85), TrapezoidalMF::new(0.65, 0.80, 1.0, 1.0));
    let severity_var = SEVERITY_VAR;
    let trust_var = ci_variable();
    const LOCALITY_VAR: FuzzyVariable = FuzzyVariable::new(TrapezoidalMF::new(0.0, 0.0, 0.30, 0.50), TrapezoidalMF::new(0.30, 0.50, 0.70, 0.90), TrapezoidalMF::new(0.70, 0.90, 1.0, 1.0));
    let locality_var = LOCALITY_VAR;
    const CONTEXT_VAR: FuzzyVariable = FuzzyVariable::new(TrapezoidalMF::new(0.0, 0.0, 0.20, 0.40), TrapezoidalMF::new(0.20, 0.50, 0.70, 0.90), TrapezoidalMF::new(0.70, 0.90, 1.0, 1.0));
    let context_var = CONTEXT_VAR;

    let sev_m = severity_var.memberships(severity);
    let trust_m = trust_var.memberships(trust);
    let loc_m = locality_var.memberships(observation.culprit_locality_ratio);
    let ctx_m = context_var.memberships(fuzzy_context_reliability(context_kind));

    let base = evaluate_ts2(&sev_m, &trust_m, &EDIT_ACCEPTANCE_BASE);
    let loc_mod = evaluate_ts1(&loc_m, &LOCALITY_MOD);
    let ctx_mod = evaluate_ts1(&ctx_m, &CONTEXT_MOD);

    (base * loc_mod * ctx_mod).clamp(0.0, 1.0)
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

pub fn fuzzy_impact_radius_cap(
    stability_score: f64,
    uncertainty: f64,
    reliability_lower: f64,
) -> Option<usize> {
    let stab_var = stability_variable();
    const UNCERT_VAR: FuzzyVariable = FuzzyVariable::new(TrapezoidalMF::new(0.0, 0.0, 0.15, 0.35), TrapezoidalMF::new(0.20, 0.35, 0.50, 0.65), TrapezoidalMF::new(0.40, 0.55, 1.0, 1.0));
    let uncert_var = UNCERT_VAR;
    let rel_var = ci_variable();

    let stab_m = stab_var.memberships(stability_score);
    let uncert_m = uncert_var.memberships(uncertainty);
    let rel_m = rel_var.memberships(reliability_lower);

    let base = evaluate_ts2(&stab_m, &uncert_m, &IMPACT_RADIUS_BASE);
    let rel_mod = evaluate_ts1(&rel_m, &RELIABILITY_MOD);
    let raw = (base * rel_mod).clamp(0.0, 1.0);

    const CAP_VAR: FuzzyVariable = FuzzyVariable::new(TrapezoidalMF::new(0.0, 0.0, 0.20, 0.35), TrapezoidalMF::new(0.25, 0.40, 0.55, 0.65), TrapezoidalMF::new(0.50, 0.65, 1.0, 1.0));
    let cap_var = CAP_VAR;
    let cap_m = cap_var.memberships(raw);
    let cap_rules: [TsRule1; 3] = [
        TsRule1 { term: 0, consequent: 0.0 },
        TsRule1 { term: 1, consequent: 0.5 },
        TsRule1 { term: 2, consequent: 1.0 },
    ];
    let cap_score = evaluate_ts1(&cap_m, &cap_rules).clamp(0.0, 1.0);
    const CAP_VAR2: FuzzyVariable = FuzzyVariable::new(TrapezoidalMF::new(0.0, 0.0, 0.15, 0.30), TrapezoidalMF::new(0.20, 0.30, 0.50, 0.65), TrapezoidalMF::new(0.55, 0.65, 1.0, 1.0));
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
pub fn fuzzy_identity_migration_tolerance(
    edit_count: usize,
    certainty: Option<&PolicyCertainty>,
) -> usize {
    let trust = certainty
        .map(|c| c.trust_for_general())
        .unwrap_or(0.5);
    let edit_volume = (edit_count.min(200) as f64 / 200.0).clamp(0.0, 1.0);

    const VOL_VAR: FuzzyVariable = FuzzyVariable::new(TrapezoidalMF::new(0.0, 0.0, 0.10, 0.30), TrapezoidalMF::new(0.15, 0.35, 0.55, 0.75), TrapezoidalMF::new(0.50, 0.70, 1.0, 1.0));
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

pub fn fuzzy_trust_deficit_penalty(trust: f64) -> f64 {
    let low_trust = TrapezoidalMF::new(0.0, 0.0, 0.08, 0.20);
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

pub fn fuzzy_raw_semantic_observation(
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
    const ERROR_VAR: FuzzyVariable = FuzzyVariable::new(TrapezoidalMF::new(0.0, 0.0, 0.10, 0.30), TrapezoidalMF::new(0.15, 0.35, 0.55, 0.75), TrapezoidalMF::new(0.50, 0.70, 1.0, 1.0));
    let error_var = ERROR_VAR;
    const PARSER_VAR: FuzzyVariable = FuzzyVariable::new(TrapezoidalMF::new(0.0, 0.0, 0.25, 0.45), TrapezoidalMF::new(0.30, 0.50, 0.75, 0.90), TrapezoidalMF::new(0.70, 0.85, 1.0, 1.0));
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

pub fn fuzzy_parser_cross_validation(
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
) -> f64 {
    if !accepted {
        // Rejected: map acceptance_score to [0.05, 0.25]
        let rejection_severity = (1.0 - acceptance_score.clamp(0.0, 1.0)) * 0.20 + 0.05;
        return rejection_severity.clamp(0.05, 0.25);
    }

    let quality_var = ci_variable();
    let trust_var = ci_variable();
    const PASS_VAR: FuzzyVariable = FuzzyVariable::new(TrapezoidalMF::new(0.0, 0.0, 0.30, 0.50), TrapezoidalMF::new(0.30, 0.50, 0.70, 0.90), TrapezoidalMF::new(0.60, 0.80, 1.0, 1.0));
    let pass_var = PASS_VAR;

    let quality_m = quality_var.memberships(acceptance_score.clamp(0.0, 1.0));
    let trust_m = trust_var.memberships(trust.clamp(0.0, 1.0));
    // Map pass_number: 0 → 1.0 (best), 1 → 0.5, 2+ → 0.0
    let pass_signal = match pass_number {
        0 => 1.0,
        1 => 0.5,
        _ => 0.0,
    };
    let pass_m = pass_var.memberships(pass_signal);

    let base = evaluate_ts2(&quality_m, &trust_m, &EDIT_OUTCOME_BASE);
    let pass_mod = evaluate_ts1(&pass_m, &PASS_MOD);

    (base * pass_mod).clamp(0.05, 0.95)
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

pub fn fuzzy_convergence_impact_cap(trust: f64) -> usize {
    kalman_interp(trust, 128.0, 256.0, 512.0).round().max(64.0) as usize
}

pub fn fuzzy_convergence_scope_cap(trust: f64) -> usize {
    kalman_interp(trust, 96.0, 192.0, 384.0).round().max(48.0) as usize
}

pub fn fuzzy_convergence_symbol_cap(trust: f64) -> usize {
    kalman_interp(trust, 64.0, 128.0, 256.0).round().max(32.0) as usize
}

pub fn fuzzy_damping_factor(trust: f64) -> f64 {
    kalman_interp(trust, 0.50, 0.30, 0.15).clamp(0.10, 0.60)
}

pub fn fuzzy_batch_dropped_cap(trust: f64) -> usize {
    kalman_interp(trust, 4.0, 8.0, 16.0).round().max(2.0) as usize
}

pub fn fuzzy_kalman_reobservation_interval(trust: f64) -> usize {
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

pub fn fuzzy_diagnostic_severity_weight(severity_level: u8, trust: f64) -> u32 {
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

pub fn fuzzy_richness_radius_multiplier(certainty: f64) -> f64 {
    kalman_interp(certainty, 2.5, 4.0, 5.5).clamp(2.0, 6.0)
}

pub fn fuzzy_style_gain_weights(certainty: f64) -> (f64, f64, f64) {
    (
        kalman_interp(certainty, 0.50, 0.60, 0.70).clamp(0.3, 0.8),
        kalman_interp(certainty, 0.50, 0.40, 0.30).clamp(0.2, 0.6),
        kalman_interp(certainty, 0.25, 0.20, 0.15).clamp(0.1, 0.3),
    )
}

pub fn fuzzy_context_tolerance_adjustment(kind: u8, trust: f64) -> (usize, usize) {
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
pub fn fuzzy_semantic_confidence_bonus(clause: u8, trust: f64) -> u16 {
    let base = match clause {
        0 => 300.0,
        1 => 250.0,
        2 => 150.0,
        3 => 180.0,
        _ => 80.0,
    };
    (base * kalman_interp(trust, 0.7, 1.0, 1.25)).round().max(0.0) as u16
}

pub fn fuzzy_contract_failure_deduction(clause: u8, trust: f64) -> u16 {
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
    fn trapezoid_full_membership_between_b_and_c() {
        let mf = TrapezoidalMF::new(0.0, 0.2, 0.8, 1.0);
        assert!((mf.membership(0.5) - 1.0).abs() < 1e-9);
        assert!((mf.membership(0.2) - 1.0).abs() < 1e-9);
        assert!((mf.membership(0.8) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn trapezoid_zero_outside_bounds() {
        let mf = TrapezoidalMF::new(0.2, 0.4, 0.6, 0.8);
        assert!((mf.membership(0.0)).abs() < 1e-9);
        assert!((mf.membership(0.1)).abs() < 1e-9);
        assert!((mf.membership(0.9)).abs() < 1e-9);
        assert!((mf.membership(1.0)).abs() < 1e-9);
    }

    #[test]
    fn trapezoid_linear_ramp() {
        let mf = TrapezoidalMF::new(0.0, 1.0, 1.0, 2.0);
        assert!((mf.membership(0.5) - 0.5).abs() < 1e-9);
        assert!((mf.membership(0.25) - 0.25).abs() < 1e-9);
        assert!((mf.membership(1.5) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn ci_variable_low_coverage() {
        let var = ci_variable();
        let m = var.memberships(0.05);
        assert!(m[0] > 0.9, "low membership should be high at 0.05");
        assert!(m[1] < 0.01, "medium should be ~0 at 0.05");
        assert!(m[2] < 0.01, "high should be ~0 at 0.05");
    }

    #[test]
    fn ci_variable_high_coverage() {
        let var = ci_variable();
        let m = var.memberships(0.90);
        assert!(m[0] < 0.01);
        assert!(m[1] < 0.01);
        assert!(m[2] > 0.5);
    }

    #[test]
    fn ci_variable_medium_coverage() {
        let var = ci_variable();
        let m = var.memberships(0.50);
        assert!(m[0] < 0.01);
        assert!(m[1] > 0.9);
        assert!(m[2] < 0.01);
    }

    #[test]
    fn ts2_weighted_average() {
        let m0 = [0.0, 1.0, 0.0];
        let m1 = [0.0, 0.0, 1.0];
        let result = evaluate_ts2(&m0, &m1, &TEST_RULE_BASE);
        assert!((result - 0.50).abs() < 1e-5, "Med×High should be 0.50, got {result}");
    }

    #[test]
    fn ts2_zero_firing_returns_zero() {
        let m0 = [0.0, 0.0, 0.0];
        let m1 = [0.0, 0.0, 0.0];
        let result = evaluate_ts2(&m0, &m1, &TEST_RULE_BASE);
        assert!(result.abs() < 1e-5);
    }

    #[test]
    fn ts2_interpolation_between_rules() {
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
    fn fuzzy_trust_zero_coverage_near_zero() {
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
        assert!(fuzzy_trust_semantic_rewrite(&c) < 0.15,
            "zero coverage should yield near-zero trust, got {}",
            fuzzy_trust_semantic_rewrite(&c));
    }

    #[test]
    fn fuzzy_trust_high_everything_yields_high() {
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
        assert!(fuzzy_trust_semantic_rewrite(&c) > 0.60,
            "high everything should yield high trust, got {}",
            fuzzy_trust_semantic_rewrite(&c));
        assert!(fuzzy_trust_structural(&c) > 0.60,
            "structural trust should be high, got {}",
            fuzzy_trust_structural(&c));
    }

    #[test]
    fn fuzzy_trust_stable_boosts() {
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
        assert!(fuzzy_trust_semantic_rewrite(&boosted) > fuzzy_trust_semantic_rewrite(&base));
    }

    #[test]
    fn fuzzy_trust_edit_success_boosts() {
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
    fn fuzzy_trust_monotonic_semantic() {
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
        assert!(fuzzy_trust_semantic_rewrite(&high) > fuzzy_trust_semantic_rewrite(&low));
    }

    fn default_observation() -> PostEditObservation {
        PostEditObservation {
            clang_diagnostics_increased: false,
            tree_error_regressed: false,
            culprit_locality_ratio: 0.8,
            has_semantic_rewrite_edits: false,
            all_edits_whitespace_safe: false,
            exact_compdb: false,
            identity_severity: 0.0,
            reference_severity: 0.0,
            scope_severity: 0.0,
        }
    }

    #[test]
    fn fuzzy_edit_acceptance_no_failures_returns_one() {
        let obs = default_observation();
        let score = fuzzy_edit_acceptance(&obs, None, SemanticCompdbContextKind::Exact);
        assert!((score - 1.0).abs() < 1e-9, "no failures should yield 1.0, got {score}");
    }

    #[test]
    fn fuzzy_edit_acceptance_tree_error_exact_compdb_blocks_semantic_rewrites() {
        let obs = PostEditObservation {
            tree_error_regressed: true,
            exact_compdb: true,
            has_semantic_rewrite_edits: true,
            ..default_observation()
        };
        let score = fuzzy_edit_acceptance(&obs, None, SemanticCompdbContextKind::Exact);
        assert!(score < 0.02, "tree error + exact compdb + semantic rewrite should yield near-zero, got {score}");
    }

    #[test]
    fn fuzzy_edit_acceptance_tree_error_exact_compdb_allows_non_semantic() {
        let obs = PostEditObservation {
            tree_error_regressed: true,
            exact_compdb: true,
            has_semantic_rewrite_edits: false,
            ..default_observation()
        };
        let score = fuzzy_edit_acceptance(&obs, None, SemanticCompdbContextKind::Exact);
        assert!(score > 0.0, "tree error + exact compdb without semantic rewrites should not hard-block, got {score}");
    }

    #[test]
    fn fuzzy_edit_acceptance_high_identity_severity_with_clang_diag() {
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
            has_semantic_rewrite_edits: true,
            culprit_locality_ratio: 0.95,
            ..default_observation()
        };
        let score = fuzzy_edit_acceptance(&obs, Some(&c), SemanticCompdbContextKind::Exact);
        assert!(score < 1.0, "high identity severity + clang_diag should reduce score, got {score}");
    }

    #[test]
    fn fuzzy_edit_acceptance_high_severity_high_trust_still_rejects() {
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
            has_semantic_rewrite_edits: true,
            culprit_locality_ratio: 0.8,
            ..default_observation()
        };
        let score = fuzzy_edit_acceptance(&obs, Some(&c), SemanticCompdbContextKind::Exact);
        assert!(score < 0.80, "high severity should reduce score even with high trust, got {score}");
    }

    #[test]
    fn fuzzy_edit_acceptance_scope_severity_medium_trust_accepts() {
        let c = default_certainty();
        let obs = PostEditObservation {
            scope_severity: 0.5,
            culprit_locality_ratio: 0.7,
            ..default_observation()
        };
        let score = fuzzy_edit_acceptance(&obs, Some(&c), SemanticCompdbContextKind::PairedSourceHeuristic);
        assert!(score >= 0.50, "moderate scope severity with medium trust should accept, got {score}");
    }

    #[test]
    fn fuzzy_edit_acceptance_whitespace_safe_accepts() {
        let obs = PostEditObservation {
            scope_severity: 0.5,
            reference_severity: 0.5,
            all_edits_whitespace_safe: true,
            culprit_locality_ratio: 0.5,
            ..default_observation()
        };
        let score = fuzzy_edit_acceptance(&obs, None, SemanticCompdbContextKind::Exact);
        assert!(score >= 0.50, "whitespace-safe edits should accept, got {score}");
    }

    #[test]
    fn fuzzy_edit_acceptance_low_locality_penalizes() {
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
        let high_score = fuzzy_edit_acceptance(&high_loc, Some(&c), SemanticCompdbContextKind::Exact);
        let low_score = fuzzy_edit_acceptance(&low_loc, Some(&c), SemanticCompdbContextKind::Exact);
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
        let low_trust = fuzzy_trust_semantic_rewrite(&low_rich);
        let high_trust = fuzzy_trust_semantic_rewrite(&high_rich);
        assert!(
            high_trust > low_trust,
            "high richness should yield higher trust ({high_trust}) than low richness ({low_trust})"
        );
    }

    #[test]
    fn impact_radius_cap_high_stability_low_uncertainty_uncapped() {
        let cap = fuzzy_impact_radius_cap(0.90, 0.10, 0.80);
        assert_eq!(cap, None, "stable + low uncertainty + high reliability should be uncapped");
    }

    #[test]
    fn impact_radius_cap_low_stability_high_uncertainty_tight() {
        let cap = fuzzy_impact_radius_cap(0.30, 0.60, 0.30);
        assert_eq!(cap, Some(1), "unstable + high uncertainty + low reliability should cap to 1");
    }

    #[test]
    fn impact_radius_cap_medium_values_intermediate() {
        let cap = fuzzy_impact_radius_cap(0.55, 0.40, 0.50);
        assert!(cap == Some(1) || cap == Some(2), "medium stability/uncertainty should cap to 1 or 2, got {cap:?}");
    }

    #[test]
    fn trust_deficit_penalty_zero_trust_max_penalty() {
        let p = fuzzy_trust_deficit_penalty(0.0);
        assert!((p - 0.075).abs() < 1e-9, "zero trust should give max penalty 0.075, got {p}");
    }

    #[test]
    fn trust_deficit_penalty_high_trust_no_penalty() {
        let p = fuzzy_trust_deficit_penalty(0.50);
        assert!(p.abs() < 1e-9, "high trust should give zero penalty, got {p}");
    }

    #[test]
    fn trust_deficit_penalty_at_boundary_smooth() {
        let p_at_015 = fuzzy_trust_deficit_penalty(0.15);
        let p_at_020 = fuzzy_trust_deficit_penalty(0.20);
        assert!(p_at_015 > 0.0, "trust=0.15 should still have some penalty, got {p_at_015}");
        assert!(p_at_020.abs() < 1e-9, "trust=0.20 should have zero penalty, got {p_at_020}");
    }

    // --- Part 2 tests: raw semantic observation ---

    #[test]
    fn raw_semantic_full_compdb_high_usr_low_errors() {
        let result = fuzzy_raw_semantic_observation(true, true, 0.9, true, 0);
        assert!(result > 0.85, "full compdb + high USR + 0 errors → high quality, got {result}");
    }

    #[test]
    fn raw_semantic_no_parser_returns_zero() {
        let result = fuzzy_raw_semantic_observation(false, false, 0.0, false, 0);
        assert!(result.abs() < 1e-9, "no parser → 0.0, got {result}");
    }

    #[test]
    fn raw_semantic_tree_only_low() {
        let result = fuzzy_raw_semantic_observation(false, false, 0.0, true, 0);
        assert!(result < 0.25, "tree-only with no USR → low quality, got {result}");
    }

    #[test]
    fn raw_semantic_errors_reduce_quality() {
        let no_errors = fuzzy_raw_semantic_observation(true, true, 0.5, true, 0);
        let many_errors = fuzzy_raw_semantic_observation(true, true, 0.5, true, 8);
        assert!(no_errors > many_errors, "errors should reduce quality: {no_errors} vs {many_errors}");
    }

    #[test]
    fn raw_semantic_monotonic_in_usr() {
        let low = fuzzy_raw_semantic_observation(true, true, 0.1, true, 0);
        let high = fuzzy_raw_semantic_observation(true, true, 0.9, true, 0);
        assert!(high > low, "higher USR ratio → higher quality: {high} vs {low}");
    }

    #[test]
    fn coverage_weight_smooth_range() {
        let low = fuzzy_coverage_weight(0.0);
        let mid = fuzzy_coverage_weight(0.5);
        let high = fuzzy_coverage_weight(1.0);
        assert!(low >= 0.2 && low < 0.25, "low coverage weight near 0.2, got {low}");
        assert!((mid - 0.5).abs() < 0.05, "mid coverage weight near 0.5, got {mid}");
        assert!(high > 0.75 && high <= 0.8, "high coverage weight near 0.8, got {high}");
    }

    // --- Part 3 tests: edit outcome ---

    #[test]
    fn edit_outcome_accepted_pass0_high_trust() {
        let outcome = fuzzy_edit_outcome(0, true, 0.95, 0.9);
        assert!(outcome > 0.85, "accepted pass 0 high trust → high outcome, got {outcome}");
    }

    #[test]
    fn edit_outcome_accepted_pass2_lower() {
        let pass0 = fuzzy_edit_outcome(0, true, 0.8, 0.7);
        let pass2 = fuzzy_edit_outcome(2, true, 0.8, 0.7);
        assert!(pass0 > pass2, "pass 0 outcome > pass 2: {pass0} vs {pass2}");
    }

    #[test]
    fn edit_outcome_rejected_low_range() {
        let outcome = fuzzy_edit_outcome(0, false, 0.3, 0.5);
        assert!(outcome >= 0.05 && outcome <= 0.25, "rejected → [0.05,0.25], got {outcome}");
    }

    #[test]
    fn edit_outcome_marginal_acceptance_lower_than_clean() {
        let clean = fuzzy_edit_outcome(0, true, 0.95, 0.8);
        let marginal = fuzzy_edit_outcome(0, true, 0.51, 0.8);
        assert!(clean > marginal, "clean acceptance > marginal: {clean} vs {marginal}");
    }

    // --- Part 5 tests: constraint penalty ---

    #[test]
    fn constraint_penalty_zero_constraints() {
        let p = fuzzy_constraint_penalty(0, 0.8);
        assert!(p < 0.02, "0 constraints → near-zero penalty, got {p}");
    }

    #[test]
    fn constraint_penalty_many_constraints_low_trust() {
        let p = fuzzy_constraint_penalty(7, 0.0);
        assert!(p > 0.15, "7 constraints + low trust → high penalty, got {p}");
    }

    #[test]
    fn constraint_penalty_trust_reduces_penalty() {
        let low_trust = fuzzy_constraint_penalty(3, 0.1);
        let high_trust = fuzzy_constraint_penalty(3, 0.9);
        assert!(low_trust > high_trust, "low trust → higher penalty: {low_trust} vs {high_trust}");
    }

    #[test]
    fn failure_severity_tree_error_no_semantic_reduced() {
        let obs = PostEditObservation {
            tree_error_regressed: true,
            has_semantic_rewrite_edits: false,
            clang_diagnostics_increased: false,
            ..default_observation()
        };
        let severity = failure_severity(&obs);
        assert!(severity >= 0.30 && severity <= 0.65,
            "tree error without semantic edits should have moderate severity ~0.4-0.6, got {severity}");
    }

    #[test]
    fn failure_severity_tree_error_with_semantic_full() {
        let obs = PostEditObservation {
            tree_error_regressed: true,
            has_semantic_rewrite_edits: true,
            ..default_observation()
        };
        let severity = failure_severity(&obs);
        assert!(severity >= 0.85,
            "tree error with semantic edits should have high severity >=0.85, got {severity}");
    }

    #[test]
    fn failure_severity_tree_error_with_clang_diag_full() {
        let obs = PostEditObservation {
            tree_error_regressed: true,
            has_semantic_rewrite_edits: false,
            clang_diagnostics_increased: true,
            ..default_observation()
        };
        let severity = failure_severity(&obs);
        assert!(severity >= 0.85,
            "tree error with clang diagnostics should have high severity >=0.85, got {severity}");
    }

    #[test]
    fn identity_migration_tolerance_low_edits_high_trust() {
        let certainty = PolicyCertainty::default();
        let tolerance = fuzzy_identity_migration_tolerance(5, Some(&certainty));
        assert!(tolerance >= 2 && tolerance <= 6,
            "low edits + default trust should give small tolerance, got {tolerance}");
    }

    #[test]
    fn identity_migration_tolerance_high_edits_low_trust() {
        let certainty = PolicyCertainty {
            overall: 0.1,
            structural: 0.1,
            semantic: 0.1,
            coverage: 0.1,
            richness: 0.1,
            edit_success: 0.1,
            ..PolicyCertainty::default()
        };
        let tolerance = fuzzy_identity_migration_tolerance(180, Some(&certainty));
        assert!(tolerance >= 10,
            "high edits + low trust should give large tolerance, got {tolerance}");
    }

    #[test]
    fn identity_migration_tolerance_no_certainty() {
        let tolerance = fuzzy_identity_migration_tolerance(50, None);
        assert!(tolerance >= 2 && tolerance <= 16,
            "no certainty should use default trust (0.5), got {tolerance}");
    }

    #[test]
    fn parser_cross_validation_matching_counts_high_agreement() {
        let agreement = fuzzy_parser_cross_validation(20, 18, 0.0, 0, 0);
        assert!(agreement > 0.8, "matching scope/decl counts should yield high agreement, got {agreement}");
    }

    #[test]
    fn parser_cross_validation_mismatched_counts_low_agreement() {
        let agreement = fuzzy_parser_cross_validation(20, 2, 0.0, 0, 0);
        assert!(agreement < 0.5, "mismatched scope/decl counts should yield low agreement, got {agreement}");
    }

    #[test]
    fn parser_cross_validation_staleness_degrades_agreement() {
        let fresh = fuzzy_parser_cross_validation(15, 15, 0.0, 0, 0);
        let stale = fuzzy_parser_cross_validation(15, 15, 0.0, 0, 5);
        assert!(stale < fresh, "staleness should degrade agreement: fresh={fresh}, stale={stale}");
    }

    #[test]
    fn parser_cross_validation_zero_staleness_no_change() {
        let agreement = fuzzy_parser_cross_validation(10, 10, 0.0, 0, 0);
        assert!(agreement > 0.9, "zero staleness + matching counts should yield near-1.0 agreement, got {agreement}");
    }
}
