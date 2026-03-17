use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::engine::fuzzy_inference::{FuzzyVariable, GaussianMF};

const MIN_FILES_FOR_POPULATION: usize = 3;
const MIN_SPREAD: f64 = 0.10;
const MIN_P25: f64 = 0.05;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DimStats {
    pub mean: f64,
    pub variance: f64,
    pub p10: f64,
    pub p25: f64,
    pub p50: f64,
    pub p75: f64,
    pub p90: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PopulationContext {
    pub dim_stats: [DimStats; 5],
    pub coverage_variable: FuzzyVariable,
    pub prior_estimates: [f64; 5],
    pub prior_variances: [f64; 5],
    pub file_count: usize,
    coverage_sorted: Vec<f64>,
    richness_sorted: Vec<f64>,
}

impl Default for PopulationContext {
    fn default() -> Self {
        Self {
            dim_stats: [
                DimStats { mean: 0.85, variance: 0.05, p10: 0.60, p25: 0.75, p50: 0.90, p75: 0.95, p90: 0.98 },
                DimStats { mean: 0.50, variance: 0.10, p10: 0.10, p25: 0.25, p50: 0.50, p75: 0.75, p90: 0.90 },
                DimStats { mean: 0.50, variance: 0.10, p10: 0.10, p25: 0.25, p50: 0.50, p75: 0.75, p90: 0.90 },
                DimStats { mean: 0.50, variance: 0.10, p10: 0.10, p25: 0.25, p50: 0.50, p75: 0.75, p90: 0.90 },
                DimStats { mean: 0.85, variance: 0.05, p10: 0.60, p25: 0.70, p50: 0.85, p75: 0.92, p90: 0.98 },
            ],
            coverage_variable: crate::engine::fuzzy_inference::ci_variable(),
            prior_estimates: [0.85, 0.50, 0.50, 0.50, 0.85],
            prior_variances: [0.06, 0.11, 0.11, 0.11, 0.06],
            file_count: 0,
            coverage_sorted: Vec::new(),
            richness_sorted: Vec::new(),
        }
    }
}

impl PopulationContext {
    pub fn compute_from_measurements(measurements: &[[f64; 5]]) -> Self {
        if measurements.len() < MIN_FILES_FOR_POPULATION {
            return Self::default();
        }

        let n = measurements.len() as f64;
        let zero_stats = DimStats { mean: 0.0, variance: 0.0, p10: 0.0, p25: 0.0, p50: 0.0, p75: 0.0, p90: 0.0 };
        let mut dim_stats = [
            zero_stats.clone(),
            zero_stats.clone(),
            zero_stats.clone(),
            zero_stats.clone(),
            zero_stats,
        ];

        for d in 0..5 {
            let mut vals: Vec<f64> = measurements.iter().map(|m| m[d]).collect();
            vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let mean = vals.iter().sum::<f64>() / n;
            let variance = vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
            dim_stats[d] = DimStats {
                mean,
                variance,
                p10: percentile(&vals, 0.10),
                p25: percentile(&vals, 0.25),
                p50: percentile(&vals, 0.50),
                p75: percentile(&vals, 0.75),
                p90: percentile(&vals, 0.90),
            };
        }

        let coverage_variable = derive_adaptive_variable(&dim_stats[2]);

        let mut coverage_sorted: Vec<f64> = measurements.iter().map(|m| m[2]).collect();
        coverage_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mut richness_sorted: Vec<f64> = measurements.iter().map(|m| m[3]).collect();
        richness_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let prior_estimates = [
            dim_stats[0].mean,
            dim_stats[1].mean,
            dim_stats[2].mean,
            dim_stats[3].mean,
            dim_stats[4].mean,
        ];
        let prior_variances = [
            (dim_stats[0].variance + 0.01).min(1.0),
            (dim_stats[1].variance + 0.01).min(1.0),
            (dim_stats[2].variance + 0.01).min(1.0),
            (dim_stats[3].variance + 0.01).min(1.0),
            (dim_stats[4].variance + 0.01).min(1.0),
        ];

        Self {
            dim_stats,
            coverage_variable,
            prior_estimates,
            prior_variances,
            file_count: measurements.len(),
            coverage_sorted,
            richness_sorted,
        }
    }

    pub fn save_for_ipc(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let bytes = crate::files::codec::StateCodec::encode_binary(self)
            .context("failed encoding population context for IPC")?;
        crate::files::atomic_writer::AtomicWriter::write_bytes(path, &bytes)
            .with_context(|| format!("failed writing population context to {}", path.display()))
    }

    pub fn load_for_ipc(path: &std::path::Path) -> Option<Self> {
        if !path.exists() {
            return None;
        }
        crate::files::codec::StateCodec::read_decode_binary::<Self>(path).ok()
    }
}

fn derive_adaptive_variable(stats: &DimStats) -> FuzzyVariable {
    let spread = stats.p90 - stats.p10;
    if spread < MIN_SPREAD && stats.p90 < 0.30 {
        let low_d = stats.p25.max(MIN_P25);
        let med_b = low_d;
        let med_c = stats.p75.max(low_d + 0.02);
        let mid_center = (med_b + med_c) / 2.0;
        let sigma_low = mid_center.max(0.05) / 2.15;
        let sigma_med = (med_c - med_b).max(0.04) / 2.15;
        let sigma_high = (1.0 - mid_center).max(0.05) / 2.15;
        return FuzzyVariable {
            low: GaussianMF::new(0.0, sigma_low.max(0.03)),
            medium: GaussianMF::new(mid_center, sigma_med.max(0.03)),
            high: GaussianMF::new(1.0, sigma_high.max(0.03)),
        };
    }

    let p10 = stats.p10.max(0.01);
    let p25 = stats.p25.max(p10 + 0.01).max(MIN_P25);
    let p75 = stats.p75.max(p25 + 0.05);
    let p90 = stats.p90.max(p75 + 0.01);

    let mid_center = (p25 + p75) / 2.0;
    let sigma_low = mid_center.max(0.05) / 2.15;
    let sigma_med = ((p90 - p10) / 2.0).max(0.05) / 2.15;
    let sigma_high = (1.0 - mid_center).max(0.05) / 2.15;

    FuzzyVariable {
        low: GaussianMF::new(0.0, sigma_low.max(0.03)),
        medium: GaussianMF::new(mid_center, sigma_med.max(0.03)),
        high: GaussianMF::new(1.0, sigma_high.max(0.03)),
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let idx = p * (sorted.len() - 1) as f64;
    let lo = idx.floor() as usize;
    let hi = idx.ceil() as usize;
    let frac = idx - lo as f64;
    if hi >= sorted.len() {
        sorted[sorted.len() - 1]
    } else {
        sorted[lo] * (1.0 - frac) + sorted[hi] * frac
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_produces_valid_context() {
        let ctx = PopulationContext::default();
        assert_eq!(ctx.file_count, 0);
        assert!((ctx.prior_estimates[4] - 0.85).abs() < 1e-9);
    }

    #[test]
    fn too_few_files_returns_default() {
        let measurements = vec![[0.9, 0.5, 0.1, 0.3, 0.8], [0.8, 0.4, 0.2, 0.5, 0.7]];
        let ctx = PopulationContext::compute_from_measurements(&measurements);
        assert_eq!(ctx.file_count, 0);
    }

    #[test]
    fn population_with_low_coverage_produces_tight_boundaries() {
        let measurements: Vec<[f64; 5]> = (0..32)
            .map(|i| {
                let coverage = (i as f64) * 0.005;
                [0.95, 0.20, coverage, 0.40, 0.80]
            })
            .collect();
        let ctx = PopulationContext::compute_from_measurements(&measurements);
        assert_eq!(ctx.file_count, 32);

        let mems = ctx.coverage_variable.memberships(0.08);
        assert!(
            mems[1] > 0.3,
            "coverage=0.08 should have significant medium membership, got {:?}",
            mems
        );

        let mems_zero = ctx.coverage_variable.memberships(0.0);
        assert!(
            mems_zero[0] > 0.9,
            "coverage=0.00 should be fully low, got {:?}",
            mems_zero
        );

        assert!(ctx.prior_estimates[0] > 0.90, "structural prior should be high");
        assert!((ctx.prior_estimates[4] - 0.80).abs() < 1e-9, "edit_success prior should be from data");
    }

    #[test]
    fn population_with_high_coverage_produces_wide_boundaries() {
        let measurements: Vec<[f64; 5]> = (0..20)
            .map(|i| {
                let coverage = 0.40 + (i as f64) * 0.025;
                [0.98, 0.70, coverage, 0.60, 0.90]
            })
            .collect();
        let ctx = PopulationContext::compute_from_measurements(&measurements);

        let mems = ctx.coverage_variable.memberships(0.50);
        assert!(
            mems[1] > 0.1 || mems[0] > 0.1,
            "coverage=0.50 should have low or medium membership in high-coverage project, got {:?}",
            mems
        );

        let mems_low = ctx.coverage_variable.memberships(0.10);
        assert!(
            mems_low[0] > 0.5,
            "coverage=0.10 should be low in high-coverage project, got {:?}",
            mems_low
        );
    }

    #[test]
    fn five_dim_stats_computed() {
        let measurements: Vec<[f64; 5]> = (0..10)
            .map(|i| {
                let v = (i as f64) / 10.0;
                [0.90 + v * 0.01, 0.50 + v * 0.05, 0.30 + v * 0.04, 0.40 + v * 0.03, 0.70 + v * 0.02]
            })
            .collect();
        let ctx = PopulationContext::compute_from_measurements(&measurements);
        assert_eq!(ctx.dim_stats.len(), 5);
        assert!(ctx.dim_stats[4].mean > 0.70);
        assert!(ctx.dim_stats[4].variance > 0.0);
        assert!(ctx.dim_stats[4].p25 > 0.0);
    }

    #[test]
    fn percentile_computation_correct() {
        let sorted = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        assert!((percentile(&sorted, 0.0) - 1.0).abs() < 1e-9);
        assert!((percentile(&sorted, 0.50) - 3.0).abs() < 1e-9);
        assert!((percentile(&sorted, 1.0) - 5.0).abs() < 1e-9);
        assert!((percentile(&sorted, 0.25) - 2.0).abs() < 1e-9);
    }
}
