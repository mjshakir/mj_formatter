use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RawConfig {
    #[serde(default)]
    pub formatter: RawFormatter,
    #[serde(default)]
    pub policies: RawPolicies,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RawCacheConfig {
    #[serde(rename = "check_result_cache_enabled")]
    pub enabled: Option<bool>,
    #[serde(rename = "check_result_cache_path")]
    pub path: Option<String>,
    #[serde(rename = "check_result_cache_l1_size")]
    pub l1_size: Option<usize>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RawPostEditConfig {
    #[serde(rename = "post_edit_check_enabled")]
    pub check_enabled: Option<bool>,
    #[serde(rename = "post_edit_fail_on_parser_unavailable")]
    pub fail_no_parser: Option<bool>,
    #[serde(rename = "post_edit_tree_error_ratio_tolerance")]
    pub error_tolerance: Option<f64>,
    #[serde(rename = "post_edit_retry_enabled")]
    pub retry_enabled: Option<bool>,
    #[serde(rename = "post_edit_retry_max_attempts")]
    pub max_attempts: Option<usize>,
    #[serde(rename = "post_edit_retry_confidence_step")]
    pub confidence_step: Option<f64>,
    #[serde(rename = "post_edit_retry_confidence_max")]
    pub confidence_max: Option<f64>,
    #[serde(rename = "post_edit_retry_aggressive_step_multiplier")]
    pub aggressive_mult: Option<f64>,
    #[serde(rename = "post_edit_retry_no_improve_limit")]
    pub no_improve_limit: Option<usize>,
    #[serde(rename = "post_edit_retry_max_blocked_policies")]
    pub max_blocked: Option<usize>,
    #[serde(rename = "retry_snapshot_cache_size")]
    pub cache_size: Option<usize>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RawGraphConfig {
    #[serde(rename = "project_graph_enabled")]
    pub enabled: Option<bool>,
    #[serde(rename = "project_graph_path")]
    pub path: Option<String>,
    #[serde(rename = "project_graph_prune_enabled")]
    pub prune_enabled: Option<bool>,
    #[serde(rename = "project_graph_retention_days")]
    pub retention_days: Option<u32>,
    #[serde(rename = "project_graph_max_nodes")]
    pub max_nodes: Option<usize>,
    #[serde(rename = "project_graph_max_edges")]
    pub max_edges: Option<usize>,
    #[serde(rename = "project_graph_tombstone_enabled")]
    pub tombstone_enabled: Option<bool>,
    #[serde(rename = "project_graph_tombstone_retention_days")]
    pub tombstone_retention: Option<u32>,
    #[serde(rename = "project_graph_tombstone_decay_days")]
    pub tombstone_decay: Option<u32>,
    #[serde(rename = "project_graph_convergence_decay_enabled")]
    pub decay_enabled: Option<bool>,
    #[serde(rename = "project_graph_convergence_decay_half_life_days")]
    pub decay_half_life: Option<u32>,
    #[serde(rename = "project_graph_convergence_decay_min_count")]
    pub decay_min_count: Option<u64>,
    #[serde(rename = "project_graph_incremental_neighborhood_enabled")]
    pub neighborhood_enabled: Option<bool>,
    #[serde(rename = "project_graph_incremental_neighborhood_hops")]
    pub neighborhood_hops: Option<usize>,
    #[serde(rename = "project_graph_incremental_neighborhood_max_files")]
    pub neighborhood_max: Option<usize>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RawRetryOptimizerConfig {
    #[serde(rename = "retry_strategy_optimizer_enabled")]
    pub enabled: Option<bool>,
    #[serde(rename = "retry_strategy_optimizer_path")]
    pub path: Option<String>,
    #[serde(rename = "retry_strategy_optimizer_ema_alpha")]
    pub ema_alpha: Option<f64>,
    #[serde(rename = "retry_strategy_optimizer_context_weight")]
    pub context_weight: Option<f64>,
    #[serde(rename = "retry_strategy_optimizer_min_samples")]
    pub min_samples: Option<u64>,
    #[serde(rename = "retry_strategy_optimizer_max_bonus")]
    pub max_bonus: Option<i32>,
    #[serde(rename = "retry_strategy_optimizer_persist_every")]
    pub persist_every: Option<u64>,
    #[serde(rename = "retry_strategy_optimizer_canary_only")]
    pub canary_only: Option<bool>,
    #[serde(rename = "retry_strategy_optimizer_auto_tune_enabled")]
    pub tune_enabled: Option<bool>,
    #[serde(rename = "retry_strategy_optimizer_auto_tune_ema_alpha")]
    pub tune_ema_alpha: Option<f64>,
    #[serde(rename = "retry_strategy_optimizer_auto_tune_target_retry_success_rate")]
    pub tune_target_rate: Option<f64>,
    #[serde(rename = "retry_strategy_optimizer_auto_tune_deadband")]
    pub tune_deadband: Option<f64>,
    #[serde(rename = "retry_strategy_optimizer_auto_tune_step")]
    pub tune_step: Option<i32>,
    #[serde(rename = "retry_strategy_optimizer_auto_tune_adjust_every")]
    pub tune_adjust_every: Option<u64>,
    #[serde(rename = "retry_strategy_optimizer_auto_tune_min_samples")]
    pub tune_min_samples: Option<u64>,
    #[serde(rename = "retry_strategy_optimizer_auto_tune_min_bonus")]
    pub tune_min_bonus: Option<i32>,
    #[serde(rename = "retry_strategy_optimizer_auto_tune_max_bonus_cap")]
    pub tune_max_cap: Option<i32>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RawAccuracyConfig {
    #[serde(rename = "accuracy_gate_enabled")]
    pub gate_enabled: Option<bool>,
    #[serde(rename = "accuracy_profile")]
    pub profile: Option<String>,
    pub semantic_required: Option<bool>,
    pub fail_closed: Option<bool>,
    #[serde(rename = "accuracy_rollout_defer_fail_closed_until_stable")]
    pub defer_fail_closed: Option<bool>,
    #[serde(rename = "accuracy_rollout_stable_passes_required")]
    pub stable_passes: Option<usize>,
    #[serde(rename = "accuracy_rollout_state_path")]
    pub rollout_path: Option<String>,
    #[serde(rename = "accuracy_ci_require_benchmark")]
    pub ci_require_bench: Option<bool>,
    #[serde(rename = "accuracy_gate_min_precision")]
    pub gate_precision: Option<f64>,
    #[serde(rename = "accuracy_gate_min_recall")]
    pub gate_recall: Option<f64>,
    #[serde(rename = "accuracy_gate_min_samples")]
    pub gate_samples: Option<usize>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RawBenchmarkConfig {
    #[serde(rename = "accuracy_benchmark_enabled")]
    pub enabled: Option<bool>,
    #[serde(rename = "accuracy_benchmark_input_dir")]
    pub input_dir: Option<String>,
    #[serde(rename = "accuracy_benchmark_expected_dir")]
    pub expected_dir: Option<String>,
    #[serde(rename = "accuracy_benchmark_min_precision")]
    pub min_precision: Option<f64>,
    #[serde(rename = "accuracy_benchmark_min_recall")]
    pub min_recall: Option<f64>,
    #[serde(rename = "accuracy_benchmark_min_match_ratio")]
    pub min_match_ratio: Option<f64>,
    #[serde(rename = "accuracy_benchmark_min_samples")]
    pub min_samples: Option<usize>,
    #[serde(rename = "accuracy_benchmark_fail_closed")]
    pub fail_closed: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RawFormatter {
    pub root: Option<String>,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    pub jobs: Option<usize>,
    pub processes: Option<usize>,
    pub check: Option<bool>,
    pub backup: Option<bool>,
    pub backup_mode: Option<String>,
    pub backup_suffix: Option<String>,
    pub backup_dir: Option<String>,
    pub report_path: Option<String>,
    pub run_journal_dir: Option<String>,
    #[serde(alias = "policy_context_tracker_path")]
    pub tracker_path: Option<String>,
    pub cpp_standard: Option<String>,
    pub clang_binary: Option<String>,
    #[serde(default)]
    pub clang_args: Vec<String>,
    #[serde(alias = "clang_compdb")]
    pub clang_compdb_path: Option<String>,
    pub clang_args_mode: Option<String>,
    pub semantic_require_compdb: Option<bool>,
    #[serde(alias = "semantic_disable_inferred_includes")]
    pub semantic_no_inferred: Option<bool>,
    #[serde(alias = "worker_process_timeout_seconds")]
    pub worker_timeout_secs: Option<u64>,
    #[serde(alias = "worker_process_kill_grace_seconds")]
    pub worker_kill_secs: Option<u64>,
    pub worker_max_restarts: Option<usize>,
    pub clang_format_binary: Option<String>,
    #[serde(alias = "conflict_detection_enabled")]
    pub conflict_enabled: Option<bool>,
    pub conflict_touch_threshold: Option<usize>,
    #[serde(alias = "confidence_blocking_enabled")]
    pub confidence_enabled: Option<bool>,
    #[serde(alias = "confidence_default_enforcement")]
    pub confidence_enforcement: Option<String>,
    pub convergence_learn_on_check: Option<bool>,
    #[serde(flatten)]
    pub cache: RawCacheConfig,
    #[serde(flatten)]
    pub post_edit: RawPostEditConfig,
    #[serde(flatten)]
    pub graph: RawGraphConfig,
    #[serde(flatten)]
    pub optimizer: RawRetryOptimizerConfig,
    #[serde(flatten)]
    pub accuracy: RawAccuracyConfig,
    #[serde(flatten)]
    pub benchmark: RawBenchmarkConfig,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RawPolicies {
    pub style: Option<String>,
    #[serde(default)]
    pub enabled: Vec<String>,
    #[serde(default)]
    pub disabled: Vec<String>,
    #[serde(default)]
    pub order: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RawEnableFile {
    #[serde(default)]
    pub enable: RawEnable,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RawEnable {
    #[serde(default)]
    pub enabled: Vec<String>,
    #[serde(default)]
    pub disabled: Vec<String>,
}
