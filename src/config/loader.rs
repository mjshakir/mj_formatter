use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use toml::Value;

use crate::cli::args::CliArgs;
use crate::config::rollout_profile::AccuracyRolloutProfile;
use crate::config::app_config::AppConfig;
use crate::config::enums::BackupMode;
use crate::config::enums::ClangArgsMode;
use crate::config::enums::Enforcement;
use crate::config::policy_config::PolicyConfig;
use crate::config::raw::{RawConfig, RawEnableFile};
use crate::config::types::AccuracyBenchmarkConfig;
use crate::config::types::AccuracyGateConfig;
use crate::config::types::ConfidenceConfig;
use crate::config::types::ProjectGraphConfig;
use crate::config::types::RetryConfig;
use crate::config::types::RetryOptimizerConfig;

/// Legacy: kept so worker_execution.rs can still pass the env var to subprocesses.
/// The adaptive calibrator has been removed; workers no longer read this env var.
pub const ADAPTIVE_CONFIDENCE_STATE_PATH_ENV: &str = "FMT_ADAPTIVE_STATE_PATH";
pub const RETRY_STRATEGY_OPTIMIZER_STATE_PATH_ENV: &str = "FMT_RETRY_OPTIMIZER_PATH";

#[derive(Clone, Copy)]
struct AccuracyProfileDefaults {
    gate_semantic_required: bool,
    gate_fail_closed: bool,
    gate_min_precision: f64,
    gate_min_recall: f64,
    gate_min_samples: usize,
    benchmark_min_precision: f64,
    benchmark_min_recall: f64,
    benchmark_min_match_ratio: f64,
    benchmark_min_samples: usize,
    rollout_stable_passes_required: usize,
    ci_require_benchmark: bool,
}

#[cfg(test)]
mod tests {
    use crate::config::rollout_profile::AccuracyRolloutProfile;
    use crate::config::loader::ConfigLoader;

    #[test]
    fn strict_requires_benchmark() {
        let defaults = ConfigLoader::accuracy_profile_defaults(AccuracyRolloutProfile::Strict);
        assert!(defaults.gate_semantic_required);
        assert!(defaults.gate_fail_closed);
        assert!(defaults.ci_require_benchmark);
        assert!(defaults.gate_min_precision >= 0.95);
        assert!(defaults.gate_min_recall >= 0.85);
    }

    #[test]
    fn adaptive_more_lenient() {
        let strict = ConfigLoader::accuracy_profile_defaults(AccuracyRolloutProfile::Strict);
        let adaptive = ConfigLoader::accuracy_profile_defaults(AccuracyRolloutProfile::Adaptive);
        assert!(strict.gate_min_precision > adaptive.gate_min_precision);
        assert!(strict.gate_min_recall > adaptive.gate_min_recall);
        assert!(strict.benchmark_min_precision > adaptive.benchmark_min_precision);
        assert!(strict.benchmark_min_recall > adaptive.benchmark_min_recall);
        assert!(strict.rollout_stable_passes_required > adaptive.rollout_stable_passes_required);
    }
}

#[derive(Default)]
pub struct ConfigLoader;

impl ConfigLoader {
    fn parse_processes_arg(s: &str) -> usize {
        if s.eq_ignore_ascii_case("max") { 0 } else { s.parse().unwrap_or(1) }
    }

    pub fn load(&self, args: &CliArgs) -> Result<AppConfig> {
        let config_path = self.resolve_config_path(args)?;
        let config_root = config_path
            .parent()
            .ok_or_else(|| anyhow!("config path has no parent: {}", config_path.display()))?
            .to_path_buf();
        let project_root = config_root
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        let content = fs::read_to_string(&config_path)
            .with_context(|| format!("failed to read config file {}", config_path.display()))?;
        let raw = toml::from_str::<RawConfig>(&content)
            .with_context(|| format!("failed to parse TOML {}", config_path.display()))?;

        let style_name = args
            .style
            .clone()
            .or(raw.policies.style.clone())
            .unwrap_or_else(|| "default".to_string());
        let style_root = project_root.join("styles").join(&style_name);
        let mut policy_settings = self.load_policy_settings(&style_root)?;

        let (enable_set, disable_set) = self.load_style_sets(&style_root)?;
        for policy in policy_settings.values_mut() {
            policy.enabled =
                enable_set.contains(&policy.name) && !disable_set.contains(&policy.name);
        }

        let config_enabled = raw.policies.enabled.clone();
        let config_disabled = raw.policies.disabled.clone();
        for name in config_enabled {
            if let Some(policy) = policy_settings.get_mut(&name) {
                policy.enabled = true;
            }
        }
        for name in config_disabled {
            if let Some(policy) = policy_settings.get_mut(&name) {
                policy.enabled = false;
            }
        }

        for name in Self::csv_or_repeat_list(&args.enable) {
            if let Some(policy) = policy_settings.get_mut(&name) {
                policy.enabled = true;
            }
        }
        for name in Self::csv_or_repeat_list(&args.disable) {
            if let Some(policy) = policy_settings.get_mut(&name) {
                policy.enabled = false;
            }
        }

        let mut include_patterns = raw.formatter.include.clone();
        if !args.include.is_empty() {
            include_patterns.extend(args.include.iter().cloned());
        }
        let mut exclude_patterns = raw.formatter.exclude.clone();
        if !args.exclude.is_empty() {
            exclude_patterns.extend(args.exclude.iter().cloned());
        }

        let root = args
            .root
            .clone()
            .or(raw.formatter.root.clone())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));

        let processes = args
            .processes
            .as_deref()
            .map(Self::parse_processes_arg)
            .or(raw.formatter.processes)
            .unwrap_or(1);
        let jobs = args.jobs
            .or_else(|| args.threads_per_process.map(|tpp| {
                let p = if processes == 0 {
                    std::thread::available_parallelism().map(usize::from).unwrap_or(1)
                } else {
                    processes
                };
                tpp * p
            }))
            .or_else(|| raw.formatter.jobs.filter(|&j| j != 0))
            .unwrap_or_else(|| {
                // I/O-aware default: actual I/O blocking fraction ~60%.
                // Optimal threads/core ≈ 1/(1-0.60) ≈ 2.5, use 4 for headroom.
                // Total = cores * 4. Distributes as 4 threads/process when p=cores,
                // and gives 16 threads for single-process mode on a 4-core machine.
                // jobs=0 in config means "auto" — use this formula instead of available_parallelism.
                let cores = std::thread::available_parallelism().map(usize::from).unwrap_or(1);
                cores.saturating_mul(4)
            });
        let check = args.check || raw.formatter.check.unwrap_or(false);
        let verbose = args.verbose;

        let backup = raw.formatter.backup.unwrap_or(true);
        let backup_mode = raw
            .formatter
            .backup_mode
            .as_deref()
            .map(BackupMode::from_value)
            .unwrap_or(BackupMode::Suffix);
        let backup_suffix = raw
            .formatter
            .backup_suffix
            .clone()
            .unwrap_or_else(|| ".bak".to_string());
        let backup_dir = raw
            .formatter
            .backup_dir
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("var/backups"));
        let report_path = raw
            .formatter
            .report_path
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("var/reports/run.ndjson"));
        let run_journal_dir = raw
            .formatter
            .run_journal_dir
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("var/runs"));
        let cache_enabled = raw.formatter.cache.enabled.unwrap_or(true);
        let cache_path = raw
            .formatter
            .cache
            .path
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("var/cache/check_results.bin"));
        let cache_l1_size = raw
            .formatter
            .cache
            .l1_size
            .unwrap_or(2048)
            .max(64);
        let tracker_path = raw
            .formatter
            .tracker_path
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("var/cache/context_tracker.bin"));

        let policy_order = raw.policies.order.clone();

        let clang_binary = raw
            .formatter
            .clang_binary
            .clone()
            .unwrap_or_else(|| "clang".to_string());
        let clang_args = raw.formatter.clang_args.clone();
        if !clang_args.is_empty() {
            return Err(anyhow!(
                "phase-1 parse fidelity lock: formatter.clang_args must be empty; compile_commands is authoritative"
            ));
        }
        if raw.formatter.semantic_require_compdb == Some(false) {
            return Err(anyhow!(
                "phase-1 parse fidelity lock: semantic_require_compdb=false is unsupported"
            ));
        }
        if raw.formatter.semantic_no_inferred == Some(false) {
            return Err(anyhow!(
                "phase-1 parse fidelity lock: semantic_disable_inferred_includes=false is unsupported"
            ));
        }
        let requested_args_mode = raw
            .formatter
            .clang_args_mode
            .as_deref()
            .map(ClangArgsMode::from_value)
            .unwrap_or(ClangArgsMode::CompdbOnly);
        if requested_args_mode != ClangArgsMode::CompdbOnly {
            return Err(anyhow!(
                "phase-1 parse fidelity lock: clang_args_mode must be 'compdb_only'"
            ));
        }
        let discovered_compdb_path = raw
            .formatter
            .clang_compdb_path
            .as_ref()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| Self::discover_compdb_path(root.as_path()));
        let clang_compdb_path =
            Self::resolve_compdb(root.as_path(), discovered_compdb_path)?;
        let cpp_standard = raw
            .formatter
            .cpp_standard
            .clone()
            .or_else(|| {
                clang_compdb_path
                    .as_ref()
                    .and_then(|path| Self::detect_cpp_std(path))
            })
            .unwrap_or_else(|| "c++17".to_string());

        let clang_args_mode = ClangArgsMode::CompdbOnly;
        let semantic_require_compdb = true;
        let semantic_no_inferred = true;
        let worker_timeout_secs = args
            .worker_timeout
            .or(raw.formatter.worker_timeout_secs)
            .unwrap_or(900)
            .max(10);
        let worker_kill_secs = raw
            .formatter
            .worker_kill_secs
            .unwrap_or(5)
            .max(1);
        let worker_max_restarts = raw.formatter.worker_max_restarts.unwrap_or(1).min(8);

        let clang_format_binary = raw
            .formatter
            .clang_format_binary
            .clone()
            .unwrap_or_else(|| "clang-format".to_string());
        let conflict_enabled = raw.formatter.conflict_enabled.unwrap_or(true);
        let conflict_touch_threshold = raw.formatter.conflict_touch_threshold.unwrap_or(3).max(2);

        let mut confidence = ConfidenceConfig::default();
        confidence.enabled = raw
            .formatter
            .confidence_enabled
            .unwrap_or(confidence.enabled);
        confidence.default_enforcement = raw
            .formatter
            .confidence_enforcement
            .as_deref()
            .map(Enforcement::from_value)
            .unwrap_or(confidence.default_enforcement);

        let mut retry_strategy_optimizer = RetryOptimizerConfig::default();
        retry_strategy_optimizer.enabled = raw
            .formatter
            .optimizer
            .enabled
            .unwrap_or(retry_strategy_optimizer.enabled);
        if let Some(path) = raw.formatter.optimizer.path.as_ref() {
            let trimmed = path.trim();
            if !trimmed.is_empty() {
                retry_strategy_optimizer.path = PathBuf::from(trimmed);
            }
        }
        if let Some(path) = std::env::var_os(RETRY_STRATEGY_OPTIMIZER_STATE_PATH_ENV) {
            let overridden = PathBuf::from(path);
            if !overridden.as_os_str().is_empty() {
                retry_strategy_optimizer.path = overridden;
            }
        }
        retry_strategy_optimizer.ema_alpha = raw
            .formatter
            .optimizer
            .ema_alpha
            .unwrap_or(retry_strategy_optimizer.ema_alpha)
            .clamp(0.01, 1.0);
        retry_strategy_optimizer.context_weight = raw
            .formatter
            .optimizer
            .context_weight
            .unwrap_or(retry_strategy_optimizer.context_weight)
            .clamp(0.0, 1.0);
        retry_strategy_optimizer.min_samples = raw
            .formatter
            .optimizer
            .min_samples
            .unwrap_or(retry_strategy_optimizer.min_samples)
            .max(1);
        retry_strategy_optimizer.max_bonus = raw
            .formatter
            .optimizer
            .max_bonus
            .unwrap_or(retry_strategy_optimizer.max_bonus)
            .clamp(0, 2_000);
        retry_strategy_optimizer.persist_every = raw
            .formatter
            .optimizer
            .persist_every
            .unwrap_or(retry_strategy_optimizer.persist_every)
            .max(1);
        retry_strategy_optimizer.canary_only = raw
            .formatter
            .optimizer
            .canary_only
            .unwrap_or(retry_strategy_optimizer.canary_only);
        retry_strategy_optimizer.auto_tune_enabled = raw
            .formatter
            .optimizer
            .tune_enabled
            .unwrap_or(retry_strategy_optimizer.auto_tune_enabled);
        retry_strategy_optimizer.auto_tune_ema_alpha = raw
            .formatter
            .optimizer
            .tune_ema_alpha
            .unwrap_or(retry_strategy_optimizer.auto_tune_ema_alpha)
            .clamp(0.01, 1.0);
        retry_strategy_optimizer.auto_tune_target_retry_success_rate = raw
            .formatter
            .optimizer
            .tune_target_rate
            .unwrap_or(retry_strategy_optimizer.auto_tune_target_retry_success_rate)
            .clamp(0.0, 1.0);
        retry_strategy_optimizer.auto_tune_deadband = raw
            .formatter
            .optimizer
            .tune_deadband
            .unwrap_or(retry_strategy_optimizer.auto_tune_deadband)
            .clamp(0.0, 0.50);
        retry_strategy_optimizer.auto_tune_step = raw
            .formatter
            .optimizer
            .tune_step
            .unwrap_or(retry_strategy_optimizer.auto_tune_step)
            .clamp(1, 1_000);
        retry_strategy_optimizer.auto_tune_adjust_every = raw
            .formatter
            .optimizer
            .tune_adjust_every
            .unwrap_or(retry_strategy_optimizer.auto_tune_adjust_every)
            .max(1);
        retry_strategy_optimizer.auto_tune_min_samples = raw
            .formatter
            .optimizer
            .tune_min_samples
            .unwrap_or(retry_strategy_optimizer.auto_tune_min_samples)
            .max(1);
        retry_strategy_optimizer.auto_tune_min_bonus = raw
            .formatter
            .optimizer
            .tune_min_bonus
            .unwrap_or(retry_strategy_optimizer.auto_tune_min_bonus)
            .clamp(0, 2_000);
        retry_strategy_optimizer.auto_tune_max_bonus_cap = raw
            .formatter
            .optimizer
            .tune_max_cap
            .unwrap_or(retry_strategy_optimizer.auto_tune_max_bonus_cap)
            .clamp(1, 4_000);
        retry_strategy_optimizer.auto_tune_max_bonus_cap = retry_strategy_optimizer
            .auto_tune_max_bonus_cap
            .max(retry_strategy_optimizer.auto_tune_min_bonus.max(1));
        retry_strategy_optimizer.max_bonus = retry_strategy_optimizer.max_bonus.clamp(
            retry_strategy_optimizer.auto_tune_min_bonus,
            retry_strategy_optimizer.auto_tune_max_bonus_cap,
        );

        let mut retry = RetryConfig::default();
        retry.post_edit_check_enabled = raw
            .formatter
            .post_edit
            .check_enabled
            .unwrap_or(retry.post_edit_check_enabled);
        retry.post_edit_fail_on_parser_unavailable = raw
            .formatter
            .post_edit
            .fail_no_parser
            .unwrap_or(retry.post_edit_fail_on_parser_unavailable);
        retry.post_edit_tree_error_ratio_tolerance = raw
            .formatter
            .post_edit
            .error_tolerance
            .unwrap_or(retry.post_edit_tree_error_ratio_tolerance)
            .clamp(0.0, 1.0);
        retry.post_edit_retry_enabled = raw
            .formatter
            .post_edit
            .retry_enabled
            .unwrap_or(retry.post_edit_retry_enabled);
        retry.post_edit_retry_max_attempts = raw
            .formatter
            .post_edit
            .max_attempts
            .unwrap_or(retry.post_edit_retry_max_attempts);
        retry.post_edit_retry_confidence_step = raw
            .formatter
            .post_edit
            .confidence_step
            .unwrap_or(retry.post_edit_retry_confidence_step)
            .clamp(0.0, 1.0);
        retry.post_edit_retry_confidence_max = raw
            .formatter
            .post_edit
            .confidence_max
            .unwrap_or(retry.post_edit_retry_confidence_max)
            .clamp(0.0, 1.0);
        retry.post_edit_retry_aggressive_step_multiplier = raw
            .formatter
            .post_edit
            .aggressive_mult
            .unwrap_or(retry.post_edit_retry_aggressive_step_multiplier)
            .clamp(1.0, 4.0);
        retry.post_edit_retry_no_improve_limit = raw
            .formatter
            .post_edit
            .no_improve_limit
            .unwrap_or(retry.post_edit_retry_no_improve_limit);
        retry.post_edit_retry_max_blocked_policies = raw
            .formatter
            .post_edit
            .max_blocked
            .unwrap_or(retry.post_edit_retry_max_blocked_policies);
        retry.retry_snapshot_cache_size = raw
            .formatter
            .post_edit
            .cache_size
            .unwrap_or(retry.retry_snapshot_cache_size);

        let accuracy_profile =
            AccuracyRolloutProfile::from_str(raw.formatter.accuracy.profile.as_deref());
        let profile_defaults = Self::accuracy_profile_defaults(accuracy_profile);
        let mut accuracy_gate = AccuracyGateConfig {
            profile: accuracy_profile,
            semantic_required: profile_defaults.gate_semantic_required,
            fail_closed: profile_defaults.gate_fail_closed,
            min_precision: profile_defaults.gate_min_precision,
            min_recall: profile_defaults.gate_min_recall,
            min_samples: profile_defaults.gate_min_samples,
            rollout_stable_passes_required: profile_defaults.rollout_stable_passes_required,
            ci_require_benchmark: profile_defaults.ci_require_benchmark,
            ..AccuracyGateConfig::default()
        };
        accuracy_gate.enabled = raw
            .formatter
            .accuracy
            .gate_enabled
            .unwrap_or(accuracy_gate.enabled);
        accuracy_gate.semantic_required = raw
            .formatter
            .accuracy
            .semantic_required
            .unwrap_or(accuracy_gate.semantic_required);
        accuracy_gate.fail_closed = raw
            .formatter
            .accuracy
            .fail_closed
            .unwrap_or(accuracy_gate.fail_closed);
        accuracy_gate.rollout_defer_fail_closed_until_stable = raw
            .formatter
            .accuracy
            .defer_fail_closed
            .unwrap_or(accuracy_gate.rollout_defer_fail_closed_until_stable);
        accuracy_gate.rollout_stable_passes_required = raw
            .formatter
            .accuracy
            .stable_passes
            .unwrap_or(accuracy_gate.rollout_stable_passes_required)
            .max(1);
        if let Some(path) = raw.formatter.accuracy.rollout_path.as_ref() {
            let trimmed = path.trim();
            if !trimmed.is_empty() {
                accuracy_gate.rollout_state_path = PathBuf::from(trimmed);
            }
        }
        accuracy_gate.ci_require_benchmark = raw
            .formatter
            .accuracy
            .ci_require_bench
            .unwrap_or(accuracy_gate.ci_require_benchmark);
        accuracy_gate.min_precision = raw
            .formatter
            .accuracy
            .gate_precision
            .unwrap_or(accuracy_gate.min_precision)
            .clamp(0.0, 1.0);
        accuracy_gate.min_recall = raw
            .formatter
            .accuracy
            .gate_recall
            .unwrap_or(accuracy_gate.min_recall)
            .clamp(0.0, 1.0);
        accuracy_gate.min_samples = raw
            .formatter
            .accuracy
            .gate_samples
            .unwrap_or(accuracy_gate.min_samples)
            .max(1);
        let mut accuracy_benchmark = AccuracyBenchmarkConfig {
            min_precision: profile_defaults.benchmark_min_precision,
            min_recall: profile_defaults.benchmark_min_recall,
            min_match_ratio: profile_defaults.benchmark_min_match_ratio,
            min_samples: profile_defaults.benchmark_min_samples,
            fail_closed: profile_defaults.gate_fail_closed,
            ..AccuracyBenchmarkConfig::default()
        };
        accuracy_benchmark.enabled = raw
            .formatter
            .benchmark
            .enabled
            .unwrap_or(accuracy_benchmark.enabled);
        if let Some(path) = raw.formatter.benchmark.input_dir.as_ref() {
            let trimmed = path.trim();
            if !trimmed.is_empty() {
                accuracy_benchmark.input_dir = PathBuf::from(trimmed);
            }
        }
        if let Some(path) = raw.formatter.benchmark.expected_dir.as_ref() {
            let trimmed = path.trim();
            if !trimmed.is_empty() {
                accuracy_benchmark.expected_dir = PathBuf::from(trimmed);
            }
        }
        accuracy_benchmark.min_precision = raw
            .formatter
            .benchmark
            .min_precision
            .unwrap_or(accuracy_benchmark.min_precision)
            .clamp(0.0, 1.0);
        accuracy_benchmark.min_recall = raw
            .formatter
            .benchmark
            .min_recall
            .unwrap_or(accuracy_benchmark.min_recall)
            .clamp(0.0, 1.0);
        accuracy_benchmark.min_match_ratio = raw
            .formatter
            .benchmark
            .min_match_ratio
            .unwrap_or(accuracy_benchmark.min_match_ratio)
            .clamp(0.0, 1.0);
        accuracy_benchmark.min_samples = raw
            .formatter
            .benchmark
            .min_samples
            .unwrap_or(accuracy_benchmark.min_samples)
            .max(1);
        accuracy_benchmark.fail_closed = raw
            .formatter
            .benchmark
            .fail_closed
            .unwrap_or(accuracy_benchmark.fail_closed);

        let mut project_graph = ProjectGraphConfig::default();
        project_graph.enabled = raw
            .formatter
            .graph
            .enabled
            .unwrap_or(project_graph.enabled);
        if let Some(path) = raw.formatter.graph.path.as_ref() {
            let trimmed = path.trim();
            if !trimmed.is_empty() {
                project_graph.path = PathBuf::from(trimmed);
            }
        }
        project_graph.prune_enabled = raw
            .formatter
            .graph
            .prune_enabled
            .unwrap_or(project_graph.prune_enabled);
        project_graph.retention_days = raw
            .formatter
            .graph
            .retention_days
            .unwrap_or(project_graph.retention_days)
            .max(1);
        project_graph.max_nodes = raw
            .formatter
            .graph
            .max_nodes
            .unwrap_or(project_graph.max_nodes)
            .max(1);
        project_graph.max_edges = raw
            .formatter
            .graph
            .max_edges
            .unwrap_or(project_graph.max_edges)
            .max(1);
        project_graph.tombstone_enabled = raw
            .formatter
            .graph
            .tombstone_enabled
            .unwrap_or(project_graph.tombstone_enabled);
        project_graph.tombstone_retention_days = raw
            .formatter
            .graph
            .tombstone_retention
            .unwrap_or(project_graph.tombstone_retention_days)
            .max(1);
        project_graph.tombstone_decay_days = raw
            .formatter
            .graph
            .tombstone_decay
            .unwrap_or(project_graph.tombstone_decay_days)
            .max(1);
        project_graph.convergence_decay_enabled = raw
            .formatter
            .graph
            .decay_enabled
            .unwrap_or(project_graph.convergence_decay_enabled);
        project_graph.convergence_decay_half_life_days = raw
            .formatter
            .graph
            .decay_half_life
            .unwrap_or(project_graph.convergence_decay_half_life_days)
            .max(1);
        project_graph.convergence_decay_min_count = raw
            .formatter
            .graph
            .decay_min_count
            .unwrap_or(project_graph.convergence_decay_min_count)
            .max(1);
        project_graph.incremental_neighborhood_enabled = raw
            .formatter
            .graph
            .neighborhood_enabled
            .unwrap_or(project_graph.incremental_neighborhood_enabled);
        project_graph.incremental_neighborhood_hops = raw
            .formatter
            .graph
            .neighborhood_hops
            .unwrap_or(project_graph.incremental_neighborhood_hops)
            .max(1);
        project_graph.incremental_neighborhood_max_files = raw
            .formatter
            .graph
            .neighborhood_max
            .unwrap_or(project_graph.incremental_neighborhood_max_files)
            .max(1);
        let convergence_learn_on_check = raw.formatter.convergence_learn_on_check.unwrap_or(false);

        Ok(AppConfig {
            root,
            include_patterns,
            exclude_patterns,
            jobs,
            processes,
            check,
            verbose,
            backup,
            backup_mode,
            backup_suffix,
            backup_dir,
            run_journal_dir,
            report_path,
            cache_enabled,
            cache_path,
            cache_l1_size,
            tracker_path,
            style_name,
            policy_settings,
            policy_order,
            cpp_standard,
            clang_binary,
            clang_args,
            clang_compdb_path,
            clang_args_mode,
            semantic_require_compdb,
            semantic_no_inferred,
            worker_timeout_secs,
            worker_kill_secs,
            worker_max_restarts,
            clang_format_binary,
            conflict_enabled,
            conflict_touch_threshold,
            confidence,
            retry_strategy_optimizer,
            retry,
            accuracy_gate,
            accuracy_benchmark,
            project_graph,
            convergence_learn_on_check,
            observation_only: false,
        })
    }

    fn accuracy_profile_defaults(profile: AccuracyRolloutProfile) -> AccuracyProfileDefaults {
        match profile {
            AccuracyRolloutProfile::Strict => AccuracyProfileDefaults {
                gate_semantic_required: true,
                gate_fail_closed: true,
                gate_min_precision: 0.97,
                gate_min_recall: 0.90,
                gate_min_samples: 8,
                benchmark_min_precision: 0.98,
                benchmark_min_recall: 0.94,
                benchmark_min_match_ratio: 0.95,
                benchmark_min_samples: 16,
                rollout_stable_passes_required: 8,
                ci_require_benchmark: true,
            },
            AccuracyRolloutProfile::Balanced => AccuracyProfileDefaults {
                gate_semantic_required: true,
                gate_fail_closed: false,
                gate_min_precision: 0.92,
                gate_min_recall: 0.60,
                gate_min_samples: 6,
                benchmark_min_precision: 0.94,
                benchmark_min_recall: 0.78,
                benchmark_min_match_ratio: 0.85,
                benchmark_min_samples: 10,
                rollout_stable_passes_required: 6,
                ci_require_benchmark: false,
            },
            AccuracyRolloutProfile::Adaptive => AccuracyProfileDefaults {
                gate_semantic_required: true,
                gate_fail_closed: false,
                gate_min_precision: 0.85,
                gate_min_recall: 0.35,
                gate_min_samples: 4,
                benchmark_min_precision: 0.90,
                benchmark_min_recall: 0.60,
                benchmark_min_match_ratio: 0.72,
                benchmark_min_samples: 8,
                rollout_stable_passes_required: 5,
                ci_require_benchmark: false,
            },
        }
    }

    fn load_policy_settings(&self, style_root: &Path) -> Result<HashMap<String, PolicyConfig>> {
        let mut result: HashMap<String, PolicyConfig> = HashMap::new();
        let format_dir = style_root.join("format");
        if !format_dir.exists() {
            return Err(anyhow!(
                "style format directory does not exist: {}",
                format_dir.display()
            ));
        }

        let mut files: Vec<PathBuf> = fs::read_dir(&format_dir)
            .with_context(|| format!("failed to read style format dir {}", format_dir.display()))?
            .filter_map(|entry| entry.ok().map(|item| item.path()))
            .filter(|path| path.extension().and_then(|s| s.to_str()) == Some("toml"))
            .collect();
        files.sort();

        for path in files {
            let content = fs::read_to_string(&path)
                .with_context(|| format!("failed to read policy file {}", path.display()))?;
            let parsed = toml::from_str::<Value>(&content)
                .with_context(|| format!("failed to parse policy file {}", path.display()))?;
            let table = parsed
                .as_table()
                .ok_or_else(|| anyhow!("policy file must be table: {}", path.display()))?;
            let policy_table = table
                .get("policy")
                .and_then(Value::as_table)
                .ok_or_else(|| anyhow!("policy file missing [policy] table: {}", path.display()))?;

            let incoming = PolicyConfig::from_policy_table(policy_table)
                .with_context(|| format!("invalid policy table in {}", path.display()))?;
            if let Some(existing) = result.get_mut(&incoming.name) {
                existing.merge(policy_table);
            } else {
                result.insert(incoming.name.clone(), incoming);
            }
        }

        Ok(result)
    }

    fn load_style_sets(
        &self,
        style_root: &Path,
    ) -> Result<(HashSet<String>, HashSet<String>)> {
        let enable_path = style_root.join("enable").join("enable.toml");
        if !enable_path.exists() {
            return Ok((HashSet::new(), HashSet::new()));
        }
        let content = fs::read_to_string(&enable_path)
            .with_context(|| format!("failed to read enable file {}", enable_path.display()))?;
        let parsed = toml::from_str::<RawEnableFile>(&content)
            .with_context(|| format!("failed to parse enable file {}", enable_path.display()))?;
        let enabled = parsed.enable.enabled.into_iter().collect();
        let disabled = parsed.enable.disabled.into_iter().collect();
        Ok((enabled, disabled))
    }

    fn resolve_config_path(&self, args: &CliArgs) -> Result<PathBuf> {
        if let Some(config) = &args.config {
            return Ok(PathBuf::from(config));
        }
        let cwd = std::env::current_dir().context("failed to get current directory")?;
        let local = cwd.join("config").join("config.toml");
        if local.exists() {
            return Ok(local);
        }
        Err(anyhow!("config/config.toml not found; pass --config"))
    }

    fn csv_or_repeat_list(values: &[String]) -> Vec<String> {
        let mut result = Vec::new();
        for value in values {
            for item in value.split(',') {
                let trimmed = item.trim();
                if !trimmed.is_empty() {
                    result.push(trimmed.to_string());
                }
            }
        }
        result
    }

    fn detect_cpp_std(compdb_path: &Path) -> Option<String> {
        let content = fs::read_to_string(compdb_path).ok()?;
        let parsed = serde_json::from_str::<serde_json::Value>(&content).ok()?;
        let entries = parsed.as_array()?;
        let mut counts = HashMap::<String, usize>::new();
        for entry in entries {
            let args = if let Some(arguments) =
                entry.get("arguments").and_then(|v| v.as_array())
            {
                arguments
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
            } else if let Some(command) = entry.get("command").and_then(|v| v.as_str()) {
                command.split_whitespace().collect::<Vec<_>>()
            } else {
                continue;
            };
            for arg in args {
                if let Some(std_value) = arg.strip_prefix("-std=") {
                    if std_value.contains("++") {
                        *counts.entry(std_value.to_string()).or_insert(0) += 1;
                    }
                }
            }
        }
        counts
            .into_iter()
            .max_by_key(|(_, count)| *count)
            .map(|(standard, _)| standard)
    }

    fn discover_compdb_path(root: &Path) -> Option<PathBuf> {
        let direct = root.join("compile_commands.json");
        if direct.is_file() {
            return Some(direct);
        }

        let build_default = root.join("build").join("compile_commands.json");
        if build_default.is_file() {
            return Some(build_default);
        }

        let mut build_dirs = fs::read_dir(root)
            .ok()?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let path = entry.path();
                let name = path.file_name()?.to_str()?;
                (path.is_dir() && name.starts_with("build")).then_some(path)
            })
            .collect::<Vec<_>>();
        build_dirs.sort();
        for directory in build_dirs {
            let candidate = directory.join("compile_commands.json");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }

    fn resolve_compdb(
        root: &Path,
        compdb_path: Option<PathBuf>,
    ) -> Result<Option<PathBuf>> {
        let Some(path) = compdb_path else {
            return Err(anyhow!(
                "phase-1 parse fidelity lock: compile_commands.json is required under '{}' or via formatter.clang_compdb_path",
                root.display()
            ));
        };
        if !path.is_file() {
            return Err(anyhow!(
                "phase-1 parse fidelity lock: compile_commands path is not a file: {}",
                path.display()
            ));
        }
        Self::validate_compdb_content(path.as_path())?;
        let normalized = fs::canonicalize(path.as_path()).unwrap_or(path);
        Ok(Some(normalized))
    }

    fn validate_compdb_content(path: &Path) -> Result<()> {
        let content = fs::read_to_string(path).with_context(|| {
            format!(
                "phase-1 parse fidelity lock: failed reading compile_commands {}",
                path.display()
            )
        })?;
        let parsed =
            serde_json::from_str::<serde_json::Value>(content.as_str()).with_context(|| {
                format!(
                    "phase-1 parse fidelity lock: failed parsing compile_commands JSON {}",
                    path.display()
                )
            })?;
        let entries = parsed.as_array().ok_or_else(|| {
            anyhow!(
                "phase-1 parse fidelity lock: compile_commands must be a JSON array ({})",
                path.display()
            )
        })?;
        let has_usable_entry = entries.iter().any(|entry| {
            let Some(table) = entry.as_object() else {
                return false;
            };
            let has_file = table
                .get("file")
                .and_then(|value| value.as_str())
                .is_some_and(|value| !value.trim().is_empty());
            let has_arguments = table
                .get("arguments")
                .and_then(|value| value.as_array())
                .is_some_and(|value| !value.is_empty());
            let has_command = table
                .get("command")
                .and_then(|value| value.as_str())
                .is_some_and(|value| !value.trim().is_empty());
            has_file && (has_arguments || has_command)
        });
        if !has_usable_entry {
            return Err(anyhow!(
                "phase-1 parse fidelity lock: compile_commands has no usable entries ({})",
                path.display()
            ));
        }
        Ok(())
    }
}
