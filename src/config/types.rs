use std::path::PathBuf;

use crate::config::enums::Enforcement;
use crate::config::rollout_profile::AccuracyRolloutProfile;

#[derive(Clone, Debug)]
pub struct ConfidenceConfig {
    pub enabled: bool,
    pub default_enforcement: Enforcement,
}

impl Default for ConfidenceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_enforcement: Enforcement::Hard,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AccuracyBenchmarkConfig {
    pub enabled: bool,
    pub input_dir: PathBuf,
    pub expected_dir: PathBuf,
    pub min_precision: f64,
    pub min_recall: f64,
    pub min_match_ratio: f64,
    pub min_samples: usize,
    pub fail_closed: bool,
}

impl Default for AccuracyBenchmarkConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            input_dir: PathBuf::from("behavior_test/input"),
            expected_dir: PathBuf::from("behavior_test/expected"),
            min_precision: 0.90,
            min_recall: 0.90,
            min_match_ratio: 0.90,
            min_samples: 8,
            fail_closed: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AccuracyGateConfig {
    pub enabled: bool,
    pub semantic_required: bool,
    pub fail_closed: bool,
    pub profile: AccuracyRolloutProfile,
    pub rollout_defer_fail_closed_until_stable: bool,
    pub rollout_stable_passes_required: usize,
    pub rollout_state_path: PathBuf,
    pub ci_require_benchmark: bool,
    pub min_precision: f64,
    pub min_recall: f64,
    pub min_samples: usize,
}

impl Default for AccuracyGateConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            semantic_required: false,
            fail_closed: false,
            profile: AccuracyRolloutProfile::Balanced,
            rollout_defer_fail_closed_until_stable: true,
            rollout_stable_passes_required: 5,
            rollout_state_path: PathBuf::from(
                "var/cache/rollout_state.bin",
            ),
            ci_require_benchmark: false,
            min_precision: 0.85,
            min_recall: 0.35,
            min_samples: 4,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RetryConfig {
    pub post_edit_check_enabled: bool,
    pub post_edit_fail_on_parser_unavailable: bool,
    pub post_edit_tree_error_ratio_tolerance: f64,
    pub post_edit_retry_enabled: bool,
    pub post_edit_retry_max_attempts: usize,
    pub post_edit_retry_confidence_step: f64,
    pub post_edit_retry_confidence_max: f64,
    pub post_edit_retry_aggressive_step_multiplier: f64,
    pub post_edit_retry_no_improve_limit: usize,
    pub post_edit_retry_max_blocked_policies: usize,
    pub retry_snapshot_cache_size: usize,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            post_edit_check_enabled: true,
            post_edit_fail_on_parser_unavailable: true,
            post_edit_tree_error_ratio_tolerance: 0.01,
            post_edit_retry_enabled: true,
            post_edit_retry_max_attempts: 6,
            post_edit_retry_confidence_step: 0.05,
            post_edit_retry_confidence_max: 1.0,
            post_edit_retry_aggressive_step_multiplier: 1.8,
            post_edit_retry_no_improve_limit: 2,
            post_edit_retry_max_blocked_policies: 6,
            retry_snapshot_cache_size: 128,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProjectGraphConfig {
    pub enabled: bool,
    pub path: PathBuf,
    pub prune_enabled: bool,
    pub retention_days: u32,
    pub max_nodes: usize,
    pub max_edges: usize,
    pub tombstone_enabled: bool,
    pub tombstone_retention_days: u32,
    pub tombstone_decay_days: u32,
    pub convergence_decay_enabled: bool,
    pub convergence_decay_half_life_days: u32,
    pub convergence_decay_min_count: u64,
    pub incremental_neighborhood_enabled: bool,
    pub incremental_neighborhood_hops: usize,
    pub incremental_neighborhood_max_files: usize,
}

impl Default for ProjectGraphConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: PathBuf::from("var/cache/graph_state.bin"),
            prune_enabled: true,
            retention_days: 30,
            max_nodes: 250_000,
            max_edges: 1_000_000,
            tombstone_enabled: true,
            tombstone_retention_days: 90,
            tombstone_decay_days: 30,
            convergence_decay_enabled: true,
            convergence_decay_half_life_days: 30,
            convergence_decay_min_count: 1,
            incremental_neighborhood_enabled: true,
            incremental_neighborhood_hops: 1,
            incremental_neighborhood_max_files: 256,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RetryOptimizerConfig {
    pub enabled: bool,
    pub path: PathBuf,
    pub ema_alpha: f64,
    pub context_weight: f64,
    pub min_samples: u64,
    pub max_bonus: i32,
    pub persist_every: u64,
    pub canary_only: bool,
    pub auto_tune_enabled: bool,
    pub auto_tune_ema_alpha: f64,
    pub auto_tune_target_retry_success_rate: f64,
    pub auto_tune_deadband: f64,
    pub auto_tune_step: i32,
    pub auto_tune_adjust_every: u64,
    pub auto_tune_min_samples: u64,
    pub auto_tune_min_bonus: i32,
    pub auto_tune_max_bonus_cap: i32,
}

impl Default for RetryOptimizerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: PathBuf::from("var/cache/retry_optimizer.json"),
            ema_alpha: 0.20,
            context_weight: 0.70,
            min_samples: 10,
            max_bonus: 180,
            persist_every: 16,
            canary_only: false,
            auto_tune_enabled: true,
            auto_tune_ema_alpha: 0.15,
            auto_tune_target_retry_success_rate: 0.80,
            auto_tune_deadband: 0.05,
            auto_tune_step: 10,
            auto_tune_adjust_every: 4,
            auto_tune_min_samples: 16,
            auto_tune_min_bonus: 40,
            auto_tune_max_bonus_cap: 240,
        }
    }
}
