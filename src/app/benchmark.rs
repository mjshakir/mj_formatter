use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use walkdir::WalkDir;

use crate::app::runner::{
    AccuracyBenchmarkStats, AccuracyGateRolloutSignal, AccuracyProfileThresholds, App,
    TempDirCleanupGuard,
};
use crate::cli::args::CliArgs;
use crate::config::rollout_profile::AccuracyRolloutProfile;
use crate::config::app_config::AppConfig;
use crate::model::file_result::FileResult;
use crate::runtime::rollout_state::{AccuracyObservation, AccuracyRolloutState};

impl App {
    pub(crate) fn run_accuracy_benchmark(
        config: &AppConfig,
    ) -> Result<Option<AccuracyBenchmarkStats>> {
        if !config.accuracy_benchmark.enabled {
            return Ok(None);
        }

        let input_root = config.accuracy_benchmark.input_dir.clone();
        let expected_root = config.accuracy_benchmark.expected_dir.clone();
        let mut stats = AccuracyBenchmarkStats::default();
        if !input_root.is_dir() || !expected_root.is_dir() {
            stats.error_files = 1;
            return Ok(Some(stats));
        }

        let benchmark_files = WalkDir::new(input_root.as_path())
            .follow_links(false)
            .into_iter()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_file())
            .filter_map(|entry| {
                let path = entry.path().to_path_buf();
                if !Self::is_benchmark_source_file(path.as_path()) {
                    return None;
                }
                let relative = path.strip_prefix(input_root.as_path()).ok()?;
                expected_root.join(relative).is_file().then_some(path)
            })
            .collect::<Vec<_>>();
        if benchmark_files.is_empty() {
            return Ok(Some(stats));
        }
        stats.eligible_files = benchmark_files.len();

        let mut benchmark_config = config.clone();
        benchmark_config.root = input_root.clone();
        benchmark_config.check = false;
        benchmark_config.backup = false;
        benchmark_config.check_result_cache_enabled = false;
        benchmark_config.project_graph.enabled = false;
        benchmark_config.retry_strategy_optimizer.enabled = false;
        benchmark_config.accuracy_gate.enabled = false;
        let benchmark_compdb_dir = Self::worker_temp_root().join(format!(
            "benchmark_compdb_{}_{}",
            std::process::id(),
            Self::backup_run_id()
        ));
        fs::create_dir_all(benchmark_compdb_dir.as_path()).with_context(|| {
            format!(
                "failed creating benchmark compdb temp directory {}",
                benchmark_compdb_dir.display()
            )
        })?;
        let _benchmark_compdb_guard = TempDirCleanupGuard::new(benchmark_compdb_dir.clone());
        let benchmark_compdb_path = benchmark_compdb_dir.join("compile_commands.json");
        Self::write_synthetic_compile_commands(
            benchmark_compdb_path.as_path(),
            benchmark_files.as_slice(),
            input_root.as_path(),
            config.cpp_standard.as_str(),
        )?;
        benchmark_config.clang_compdb_path = Some(benchmark_compdb_path);
        benchmark_config.semantic_require_compdb = true;
        benchmark_config.semantic_disable_inferred_includes = true;

        let benchmark_results = Self::run_processing_pass(
            &benchmark_config,
            benchmark_files.clone(),
            None,
            false,
            false,
            None,
        )?;
        let mut by_path = HashMap::<PathBuf, FileResult>::new();
        for result in benchmark_results {
            by_path.insert(result.path.clone(), result);
        }

        for input_path in benchmark_files {
            let Ok(relative_path) = input_path.strip_prefix(input_root.as_path()) else {
                stats.error_files = stats.error_files.saturating_add(1);
                continue;
            };
            let expected_path = expected_root.join(relative_path);
            let input_text = match fs::read_to_string(input_path.as_path()) {
                Ok(value) => value,
                Err(_) => {
                    stats.error_files = stats.error_files.saturating_add(1);
                    continue;
                }
            };
            let expected_text = match fs::read_to_string(expected_path.as_path()) {
                Ok(value) => value,
                Err(_) => {
                    stats.error_files = stats.error_files.saturating_add(1);
                    continue;
                }
            };
            let Some(result) = by_path.remove(input_path.as_path()) else {
                stats.error_files = stats.error_files.saturating_add(1);
                continue;
            };
            let actual_text = if result.error.is_some() {
                stats.error_files = stats.error_files.saturating_add(1);
                input_text.clone()
            } else if result.changed {
                result.pending_text.unwrap_or_else(|| input_text.clone())
            } else {
                input_text.clone()
            };

            let expected_change = expected_text != input_text;
            let actual_change = actual_text != input_text;
            let fixed = actual_text == expected_text;
            stats.considered_files = stats.considered_files.saturating_add(1);
            if fixed {
                stats.exact_matches = stats.exact_matches.saturating_add(1);
            }
            if expected_change {
                if fixed {
                    stats.true_positive = stats.true_positive.saturating_add(1);
                } else {
                    stats.false_negative = stats.false_negative.saturating_add(1);
                }
            } else if actual_change {
                stats.false_positive = stats.false_positive.saturating_add(1);
            } else {
                stats.true_negative = stats.true_negative.saturating_add(1);
            }
        }

        Ok(Some(stats))
    }

    pub(crate) fn spawn_async_accuracy_benchmark(
        args: &CliArgs,
        config: &AppConfig,
    ) -> Result<bool> {
        if !config.accuracy_benchmark.enabled || Self::is_ci_environment() {
            return Ok(false);
        }
        let current_exe = std::env::current_exe().context("failed to resolve current binary")?;
        let mut command = Command::new(current_exe);
        if let Some(config_path) = args.config.as_ref() {
            command.arg("--config").arg(config_path);
        }
        if let Some(style) = args.style.as_ref() {
            command.arg("--style").arg(style);
        }
        if let Some(root) = args.root.as_ref() {
            command.arg("--root").arg(root);
        }
        for include in &args.include {
            command.arg("--include").arg(include);
        }
        for exclude in &args.exclude {
            command.arg("--exclude").arg(exclude);
        }
        for enable in &args.enable {
            command.arg("--enable").arg(enable);
        }
        for disable in &args.disable {
            command.arg("--disable").arg(disable);
        }
        if let Some(jobs) = args.jobs {
            command.arg("--jobs").arg(jobs.to_string());
        }
        command
            .arg("--benchmark-only")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
            .spawn()
            .context("failed spawning async accuracy benchmark")?;
        Ok(true)
    }

    pub(crate) fn run_benchmark_only_entry(
        config: &mut AppConfig,
        accuracy_rollout_state: &mut AccuracyRolloutState,
        requested_rollout_profile: AccuracyRolloutProfile,
        requested_fail_closed: bool,
    ) -> Result<()> {
        if !config.accuracy_benchmark.enabled {
            return Ok(());
        }
        Self::finalize_accuracy_validation(
            config,
            accuracy_rollout_state,
            requested_rollout_profile,
            requested_fail_closed,
            None,
        )
    }

    pub(crate) fn finalize_accuracy_validation(
        config: &mut AppConfig,
        accuracy_rollout_state: &mut AccuracyRolloutState,
        requested_rollout_profile: AccuracyRolloutProfile,
        requested_fail_closed: bool,
        gate_rollout_signal: Option<AccuracyGateRolloutSignal>,
    ) -> Result<()> {
        if let Some(stats) = Self::run_accuracy_benchmark(config)? {
            let precision = stats.precision();
            let recall = stats.recall();
            let match_ratio = stats.match_ratio();
            let min_precision = config.accuracy_benchmark.min_precision;
            let min_recall = config.accuracy_benchmark.min_recall;
            let min_match_ratio = config.accuracy_benchmark.min_match_ratio;
            let required_samples = config
                .accuracy_benchmark
                .min_samples
                .min(stats.eligible_files.max(1));
            println!(
                "accuracy_benchmark: files={} exact_match={:.3} precision={:.3} recall={:.3} errors={}",
                stats.considered_files,
                match_ratio,
                precision,
                recall,
                stats.error_files
            );
            let benchmark_failed = stats.considered_files < required_samples
                || precision + f64::EPSILON < min_precision
                || recall + f64::EPSILON < min_recall
                || match_ratio + f64::EPSILON < min_match_ratio;
            let benchmark_passed = !benchmark_failed;
            accuracy_rollout_state.observe_benchmark(
                requested_rollout_profile,
                AccuracyObservation {
                    passed: benchmark_passed,
                    precision,
                    recall,
                    match_ratio,
                    min_samples_met: stats.considered_files >= required_samples,
                    stable_passes_required: config.accuracy_gate.rollout_stable_passes_required,
                },
            )?;
            let rollout_status = accuracy_rollout_state.status(requested_rollout_profile);
            config.accuracy_gate.profile = rollout_status.effective_profile;
            Self::apply_accuracy_profile_thresholds(config);
            let gate_fail_closed_effective = accuracy_rollout_state.effective_fail_closed(
                rollout_status.requested_profile,
                requested_fail_closed,
                config.accuracy_gate.rollout_defer_fail_closed_until_stable,
            );
            let benchmark_fail_closed_effective = accuracy_rollout_state.effective_fail_closed(
                rollout_status.requested_profile,
                config.accuracy_benchmark.fail_closed,
                config.accuracy_gate.rollout_defer_fail_closed_until_stable,
            );
            config.accuracy_gate.fail_closed = gate_fail_closed_effective;
            Self::print_accuracy_rollout_status(&rollout_status, gate_fail_closed_effective);
            if benchmark_failed {
                let message = format!(
                    "accuracy benchmark below threshold: precision={:.3}/{:.3}, recall={:.3}/{:.3}, match={:.3}/{:.3}, files={}/{}",
                    precision,
                    min_precision,
                    recall,
                    min_recall,
                    match_ratio,
                    min_match_ratio,
                    stats.considered_files,
                    required_samples
                );
                if benchmark_fail_closed_effective {
                    return Err(anyhow::anyhow!(message));
                }
                tracing::warn!(message = %message, "accuracy benchmark below threshold");
            }
        } else {
            if let Some(signal) = gate_rollout_signal {
                let gate_min_samples_met =
                    signal.considered_files >= config.accuracy_gate.min_samples.max(1);
                let gate_threshold_miss = signal.precision + f64::EPSILON
                    < config.accuracy_gate.min_precision
                    || signal.recall + f64::EPSILON < config.accuracy_gate.min_recall;
                let semantic_required_unmet = config.accuracy_gate.semantic_required
                    && signal.semantic_required_unmet_files > 0;
                let gate_failed = gate_min_samples_met
                    && (signal.failing_files > 0 || gate_threshold_miss || semantic_required_unmet);
                accuracy_rollout_state.observe_gate_signal(
                    requested_rollout_profile,
                    AccuracyObservation {
                        passed: !gate_failed,
                        precision: signal.precision,
                        recall: signal.recall,
                        match_ratio: signal.match_ratio,
                        min_samples_met: gate_min_samples_met,
                        stable_passes_required: config.accuracy_gate.rollout_stable_passes_required,
                    },
                )?;
                let rollout_status = accuracy_rollout_state.status(requested_rollout_profile);
                config.accuracy_gate.profile = rollout_status.effective_profile;
                Self::apply_accuracy_profile_thresholds(config);
                let gate_fail_closed_effective = accuracy_rollout_state.effective_fail_closed(
                    rollout_status.requested_profile,
                    requested_fail_closed,
                    config.accuracy_gate.rollout_defer_fail_closed_until_stable,
                );
                config.accuracy_gate.fail_closed = gate_fail_closed_effective;
                Self::print_accuracy_rollout_status(&rollout_status, gate_fail_closed_effective);
                if gate_failed {
                    let message = format!(
                        "accuracy gate signal below threshold: precision={:.3}/{:.3}, recall={:.3}/{:.3}, match={:.3}, failing_files={} semantic_required_unmet_files={} files={} min_samples_met={}",
                        signal.precision,
                        config.accuracy_gate.min_precision,
                        signal.recall,
                        config.accuracy_gate.min_recall,
                        signal.match_ratio,
                        signal.failing_files,
                        signal.semantic_required_unmet_files,
                        signal.considered_files,
                        gate_min_samples_met
                    );
                    if gate_fail_closed_effective {
                        return Err(anyhow::anyhow!(message));
                    }
                    tracing::warn!(message = %message, "accuracy rollout below threshold");
                }
            }
            if Self::is_ci_environment()
                && config.accuracy_gate.ci_require_benchmark
                && config.accuracy_gate.enabled
            {
                return Err(anyhow::anyhow!(
                    "accuracy benchmark is required in CI for profile '{}'",
                    config.accuracy_gate.profile.as_str()
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn apply_accuracy_profile_thresholds(config: &mut AppConfig) {
        let defaults = Self::accuracy_profile_thresholds(config.accuracy_gate.profile);
        match config.accuracy_gate.profile {
            crate::config::rollout_profile::AccuracyRolloutProfile::Strict
            | crate::config::rollout_profile::AccuracyRolloutProfile::Balanced => {
                config.accuracy_gate.min_precision = config
                    .accuracy_gate
                    .min_precision
                    .max(defaults.gate_min_precision);
                config.accuracy_gate.min_recall = config
                    .accuracy_gate
                    .min_recall
                    .max(defaults.gate_min_recall);
                config.accuracy_gate.min_samples = config
                    .accuracy_gate
                    .min_samples
                    .max(defaults.gate_min_samples);
                config.accuracy_benchmark.min_precision = config
                    .accuracy_benchmark
                    .min_precision
                    .max(defaults.benchmark_min_precision);
                config.accuracy_benchmark.min_recall = config
                    .accuracy_benchmark
                    .min_recall
                    .max(defaults.benchmark_min_recall);
                config.accuracy_benchmark.min_match_ratio = config
                    .accuracy_benchmark
                    .min_match_ratio
                    .max(defaults.benchmark_min_match_ratio);
                config.accuracy_benchmark.min_samples = config
                    .accuracy_benchmark
                    .min_samples
                    .max(defaults.benchmark_min_samples);
            }
            crate::config::rollout_profile::AccuracyRolloutProfile::Adaptive => {
                config.accuracy_gate.min_precision = config
                    .accuracy_gate
                    .min_precision
                    .min(defaults.gate_min_precision);
                config.accuracy_gate.min_recall = config
                    .accuracy_gate
                    .min_recall
                    .min(defaults.gate_min_recall);
                config.accuracy_gate.min_samples = config
                    .accuracy_gate
                    .min_samples
                    .min(defaults.gate_min_samples);
                config.accuracy_benchmark.min_precision = config
                    .accuracy_benchmark
                    .min_precision
                    .min(defaults.benchmark_min_precision);
                config.accuracy_benchmark.min_recall = config
                    .accuracy_benchmark
                    .min_recall
                    .min(defaults.benchmark_min_recall);
                config.accuracy_benchmark.min_match_ratio = config
                    .accuracy_benchmark
                    .min_match_ratio
                    .min(defaults.benchmark_min_match_ratio);
                config.accuracy_benchmark.min_samples = config
                    .accuracy_benchmark
                    .min_samples
                    .min(defaults.benchmark_min_samples);
            }
        }
    }

    pub(crate) fn accuracy_profile_thresholds(
        profile: crate::config::rollout_profile::AccuracyRolloutProfile,
    ) -> AccuracyProfileThresholds {
        match profile {
            crate::config::rollout_profile::AccuracyRolloutProfile::Strict => {
                AccuracyProfileThresholds {
                    gate_min_precision: 0.97,
                    gate_min_recall: 0.90,
                    gate_min_samples: 8,
                    benchmark_min_precision: 0.98,
                    benchmark_min_recall: 0.94,
                    benchmark_min_match_ratio: 0.95,
                    benchmark_min_samples: 16,
                }
            }
            crate::config::rollout_profile::AccuracyRolloutProfile::Balanced => {
                AccuracyProfileThresholds {
                    gate_min_precision: 0.92,
                    gate_min_recall: 0.60,
                    gate_min_samples: 6,
                    benchmark_min_precision: 0.94,
                    benchmark_min_recall: 0.78,
                    benchmark_min_match_ratio: 0.85,
                    benchmark_min_samples: 10,
                }
            }
            crate::config::rollout_profile::AccuracyRolloutProfile::Adaptive => {
                AccuracyProfileThresholds {
                    gate_min_precision: 0.85,
                    gate_min_recall: 0.35,
                    gate_min_samples: 4,
                    benchmark_min_precision: 0.90,
                    benchmark_min_recall: 0.60,
                    benchmark_min_match_ratio: 0.72,
                    benchmark_min_samples: 8,
                }
            }
        }
    }

    pub(crate) fn contract_violation_count(results: &[FileResult]) -> usize {
        results
            .iter()
            .map(|result| {
                result
                    .violations
                    .iter()
                    .filter(|violation| {
                        matches!(
                            violation.policy.as_str(),
                            "semantic_contract" | "post_edit_check"
                        )
                    })
                    .count()
            })
            .sum()
    }
}
