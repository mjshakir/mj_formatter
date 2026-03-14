use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RawConfig {
    #[serde(default)]
    pub formatter: RawFormatter,
    #[serde(default)]
    pub policies: RawPolicies,
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
    pub check_result_cache_enabled: Option<bool>,
    pub check_result_cache_path: Option<String>,
    pub check_result_cache_l1_size: Option<usize>,
    pub policy_context_tracker_path: Option<String>,
    pub cpp_standard: Option<String>,
    pub clang_binary: Option<String>,
    #[serde(default)]
    pub clang_args: Vec<String>,
    #[serde(alias = "clang_compdb")]
    pub clang_compdb_path: Option<String>,
    pub clang_args_mode: Option<String>,
    pub semantic_require_compdb: Option<bool>,
    pub semantic_disable_inferred_includes: Option<bool>,
    pub worker_process_timeout_seconds: Option<u64>,
    pub worker_process_kill_grace_seconds: Option<u64>,
    pub worker_max_restarts: Option<usize>,
    pub clang_format_binary: Option<String>,
    pub conflict_detection_enabled: Option<bool>,
    pub conflict_touch_threshold: Option<usize>,
    pub post_edit_check_enabled: Option<bool>,
    pub post_edit_fail_on_parser_unavailable: Option<bool>,
    pub post_edit_tree_error_ratio_tolerance: Option<f64>,
    pub post_edit_retry_enabled: Option<bool>,
    pub post_edit_retry_max_attempts: Option<usize>,
    pub post_edit_retry_confidence_step: Option<f64>,
    pub post_edit_retry_confidence_max: Option<f64>,
    pub post_edit_retry_aggressive_step_multiplier: Option<f64>,
    pub post_edit_retry_no_improve_limit: Option<usize>,
    pub post_edit_retry_max_blocked_policies: Option<usize>,
    pub retry_snapshot_cache_size: Option<usize>,
    pub project_graph_enabled: Option<bool>,
    pub project_graph_path: Option<String>,
    pub project_graph_prune_enabled: Option<bool>,
    pub project_graph_retention_days: Option<u32>,
    pub project_graph_max_nodes: Option<usize>,
    pub project_graph_max_edges: Option<usize>,
    pub project_graph_tombstone_enabled: Option<bool>,
    pub project_graph_tombstone_retention_days: Option<u32>,
    pub project_graph_tombstone_decay_days: Option<u32>,
    pub project_graph_convergence_decay_enabled: Option<bool>,
    pub project_graph_convergence_decay_half_life_days: Option<u32>,
    pub project_graph_convergence_decay_min_count: Option<u64>,
    pub project_graph_incremental_neighborhood_enabled: Option<bool>,
    pub project_graph_incremental_neighborhood_hops: Option<usize>,
    pub project_graph_incremental_neighborhood_max_files: Option<usize>,
    pub convergence_learn_on_check: Option<bool>,
    pub confidence_blocking_enabled: Option<bool>,
    pub confidence_default_enforcement: Option<String>,
    pub retry_strategy_optimizer_enabled: Option<bool>,
    pub retry_strategy_optimizer_path: Option<String>,
    pub retry_strategy_optimizer_ema_alpha: Option<f64>,
    pub retry_strategy_optimizer_context_weight: Option<f64>,
    pub retry_strategy_optimizer_min_samples: Option<u64>,
    pub retry_strategy_optimizer_max_bonus: Option<i32>,
    pub retry_strategy_optimizer_persist_every: Option<u64>,
    pub retry_strategy_optimizer_canary_only: Option<bool>,
    pub retry_strategy_optimizer_auto_tune_enabled: Option<bool>,
    pub retry_strategy_optimizer_auto_tune_ema_alpha: Option<f64>,
    pub retry_strategy_optimizer_auto_tune_target_retry_success_rate: Option<f64>,
    pub retry_strategy_optimizer_auto_tune_deadband: Option<f64>,
    pub retry_strategy_optimizer_auto_tune_step: Option<i32>,
    pub retry_strategy_optimizer_auto_tune_adjust_every: Option<u64>,
    pub retry_strategy_optimizer_auto_tune_min_samples: Option<u64>,
    pub retry_strategy_optimizer_auto_tune_min_bonus: Option<i32>,
    pub retry_strategy_optimizer_auto_tune_max_bonus_cap: Option<i32>,
    pub accuracy_gate_enabled: Option<bool>,
    pub accuracy_profile: Option<String>,
    pub semantic_required: Option<bool>,
    pub fail_closed: Option<bool>,
    pub accuracy_rollout_defer_fail_closed_until_stable: Option<bool>,
    pub accuracy_rollout_stable_passes_required: Option<usize>,
    pub accuracy_rollout_state_path: Option<String>,
    pub accuracy_ci_require_benchmark: Option<bool>,
    pub accuracy_gate_min_precision: Option<f64>,
    pub accuracy_gate_min_recall: Option<f64>,
    pub accuracy_gate_min_samples: Option<usize>,
    pub accuracy_benchmark_enabled: Option<bool>,
    pub accuracy_benchmark_input_dir: Option<String>,
    pub accuracy_benchmark_expected_dir: Option<String>,
    pub accuracy_benchmark_min_precision: Option<f64>,
    pub accuracy_benchmark_min_recall: Option<f64>,
    pub accuracy_benchmark_min_match_ratio: Option<f64>,
    pub accuracy_benchmark_min_samples: Option<usize>,
    pub accuracy_benchmark_fail_closed: Option<bool>,
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
