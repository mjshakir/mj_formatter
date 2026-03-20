use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::thread;
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn, Level};

use crate::cli::args::CliArgs;
use crate::config::app_config::AppConfig;
use crate::config::loader::ConfigLoader;
use crate::engine::accuracy_gate::AccuracyGateDecision;
use crate::engine::retry_optimizer::RetryStrategyOptimizer;
use crate::files::finder::FileFinder;
use crate::files::file_io::FileIo;
use crate::model::edit::Edit;
use crate::model::file_result::FileResult;
use crate::model::exec_trace::PolicyExecutionTrace;
use crate::model::policy_name::PolicyName;
use crate::model::run_summary::RunSummary;
use crate::model::rename_plan::SemanticRenamePlan;
use crate::model::violation::Violation;
use crate::parser::clang_service::ClangParseService;
use crate::parser::manager::ParserManager;
use crate::engine::population_context::PopulationContext;
use crate::engine::catalog::PolicyCapabilityMatrix;
use crate::policy::registry::PolicyRegistry;
use crate::runtime::rollout_state::AccuracyRolloutState;
use crate::runtime::adaptive_telemetry::{AdaptiveTelemetry, AdaptiveTelemetrySnapshot};
use crate::runtime::backup::BackupManifest;
use crate::runtime::scheduler::{
    DispatchBatch, DispatchFeatureCache, DispatchHistoryStore, DispatchMode, DispatchObservation,
    DispatchScheduler,
};
use crate::runtime::logging::RuntimeLogging;
use crate::runtime::cluster_telemetry::{
    PolicyClusterSnapshotEntry, PolicyClusterTelemetry,
};
use crate::runtime::telemetry::{PolicyTelemetry, PolicyTelemetrySnapshotEntry};
use crate::model::report_record::ReportRecord;
use crate::runtime::reporter::ReporterProcess;
use crate::runtime::retry_telemetry::{RetryLearningSnapshot, RetryLearningTelemetry};
use crate::runtime::journal::RunJournal;
use crate::runtime::toolchain::ToolchainRequirements;

pub struct App;

pub(crate) struct SemanticPropagationOutcome {
    pub(crate) index: usize,
    pub(crate) pending_text: Option<String>,
    pub(crate) edits: Vec<Edit>,
    pub(crate) warning: Option<String>,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct AccuracyBenchmarkStats {
    pub(crate) eligible_files: usize,
    pub(crate) considered_files: usize,
    pub(crate) exact_matches: usize,
    pub(crate) true_positive: usize,
    pub(crate) false_positive: usize,
    pub(crate) false_negative: usize,
    pub(crate) true_negative: usize,
    pub(crate) error_files: usize,
}

impl AccuracyBenchmarkStats {
    pub(crate) fn precision(&self) -> f64 {
        let denom = self.true_positive + self.false_positive;
        if denom == 0 {
            1.0
        } else {
            (self.true_positive as f64 / denom as f64).clamp(0.0, 1.0)
        }
    }

    pub(crate) fn recall(&self) -> f64 {
        let denom = self.true_positive + self.false_negative;
        if denom == 0 {
            1.0
        } else {
            (self.true_positive as f64 / denom as f64).clamp(0.0, 1.0)
        }
    }

    pub(crate) fn match_ratio(&self) -> f64 {
        if self.considered_files == 0 {
            1.0
        } else {
            (self.exact_matches as f64 / self.considered_files as f64).clamp(0.0, 1.0)
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct AccuracyProfileThresholds {
    pub(crate) gate_min_precision: f64,
    pub(crate) gate_min_recall: f64,
    pub(crate) gate_min_samples: usize,
    pub(crate) benchmark_min_precision: f64,
    pub(crate) benchmark_min_recall: f64,
    pub(crate) benchmark_min_match_ratio: f64,
    pub(crate) benchmark_min_samples: usize,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct AccuracyGateRolloutSignal {
    pub(crate) precision: f64,
    pub(crate) recall: f64,
    pub(crate) considered_files: usize,
    pub(crate) failing_files: usize,
    pub(crate) semantic_required_unmet_files: usize,
    pub(crate) match_ratio: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct WorkerManifest {
    pub(crate) shard_index: usize,
    pub(crate) shard_coverage_fingerprint: String,
    pub(crate) files: Vec<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct SyntheticCompileCommand {
    pub(crate) directory: String,
    pub(crate) file: String,
    pub(crate) arguments: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct WorkerConvergencePair {
    pub(crate) loser: PolicyName,
    pub(crate) winner: PolicyName,
    pub(crate) count: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct WorkerFileResult {
    pub(crate) path: PathBuf,
    pub(crate) changed: bool,
    pub(crate) pending_text: Option<String>,
    pub(crate) semantic_rename_plans: Vec<SemanticRenamePlan>,
    pub(crate) convergence_pairs: Vec<WorkerConvergencePair>,
    pub(crate) violations: Vec<Violation>,
    pub(crate) edits: Vec<Edit>,
    #[serde(default)]
    pub(crate) policy_traces: Vec<PolicyExecutionTrace>,
    #[serde(default)]
    pub(crate) accuracy_gate: Option<AccuracyGateDecision>,
    pub(crate) error: Option<String>,
    pub(crate) warnings: Vec<String>,
    #[serde(default)]
    pub(crate) elapsed_engine_ms: f64,
    #[serde(default)]
    pub(crate) elapsed_total_ms: f64,
    #[serde(default)]
    pub(crate) policy_certainty: Option<crate::engine::catalog::PolicyCertainty>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct WorkerDispatchObservation {
    pub(crate) path_identity: String,
    pub(crate) observed_size: u64,
    pub(crate) observed_lines: u64,
    pub(crate) elapsed_wall_ns: u64,
    #[serde(default)]
    pub(crate) total_wall_ns: u64,
    #[serde(default)]
    pub(crate) retry_effort_units: u64,
    pub(crate) retry_penalty_ns: u64,
    pub(crate) error: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct WorkerOutput {
    pub(crate) shard_index: usize,
    pub(crate) shard_coverage_fingerprint: String,
    pub(crate) result_coverage_fingerprint: String,
    pub(crate) results: Vec<WorkerFileResult>,
    #[serde(default)]
    pub(crate) dispatch_observations: Vec<WorkerDispatchObservation>,
    pub(crate) adaptive_telemetry: AdaptiveTelemetrySnapshot,
    pub(crate) policy_telemetry: Vec<PolicyTelemetrySnapshotEntry>,
    pub(crate) policy_cluster_telemetry: Vec<PolicyClusterSnapshotEntry>,
    pub(crate) retry_learning: RetryLearningSnapshot,
}

pub(crate) struct TempDirCleanupGuard {
    path: PathBuf,
}

impl TempDirCleanupGuard {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for TempDirCleanupGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.path.as_path());
    }
}

impl App {
    pub fn run() -> Result<()> {
        let args = CliArgs::parse();
        if args.clang_parse_helper {
            return ClangParseService::run_helper_stdio();
        }
        if args.reporter {
            let report_path = args
                .reporter_path
                .as_deref()
                .unwrap_or("report.ndjson");
            return crate::runtime::reporter::run_reporter_entry(
                std::path::Path::new(report_path),
            );
        }
        let loader = ConfigLoader;
        let mut config = loader.load(&args)?;
        RuntimeLogging::init(config.verbose);
        PolicyTelemetry::reset();
        AdaptiveTelemetry::reset();
        PolicyClusterTelemetry::reset();
        RetryLearningTelemetry::reset();
        let mut accuracy_rollout_state =
            AccuracyRolloutState::open(config.accuracy_gate.rollout_state_path.as_path());
        let requested_rollout_profile = config.accuracy_gate.profile;
        let effective_rollout_profile =
            accuracy_rollout_state.effective_profile(requested_rollout_profile);
        config.accuracy_gate.profile = effective_rollout_profile;
        Self::apply_accuracy_profile_thresholds(&mut config);
        let requested_fail_closed = config.accuracy_gate.fail_closed;
        let effective_fail_closed = accuracy_rollout_state.effective_fail_closed(
            requested_rollout_profile,
            requested_fail_closed,
            config.accuracy_gate.rollout_defer_fail_closed_until_stable,
        );
        config.accuracy_gate.fail_closed = effective_fail_closed;
        if Self::is_ci_environment()
            && config.accuracy_gate.ci_require_benchmark
            && !config.accuracy_benchmark.enabled
        {
            return Err(anyhow!(
                "accuracy rollout profile '{}' requires accuracy_benchmark_enabled=true in CI",
                config.accuracy_gate.profile.as_str()
            ));
        }
        if args.undo {
            BackupManifest::restore(
                config.backup_dir.as_path(),
                args.undo_run.as_deref(),
            )?;
            return Ok(());
        }

        ToolchainRequirements::verify(&config.clang_binary, &config.clang_format_binary)?;

        if args.benchmark_only {
            return Self::run_benchmark_only_entry(
                &mut config,
                &mut accuracy_rollout_state,
                requested_rollout_profile,
                requested_fail_closed,
            );
        }

        if args.worker_pool {
            return Self::run_worker_pool_entry(&args, &config);
        }

        if args.worker_manifest.is_some() || args.worker_result.is_some() {
            return Self::run_worker_entry(&args, &config);
        }

        let policies = PolicyRegistry::build_enabled(&config);
        if args.list_policies {
            println!("style: {}", config.style_name);
            for policy in &policies {
                println!(
                    "policy: {:30} parse={}",
                    policy.name(),
                    if PolicyCapabilityMatrix::for_policy(policy.name()).semantic_rewrite { "hybrid" } else { "tree-sitter" }
                );
            }
            return Ok(());
        }

        let mut run_journal = RunJournal::start(&config);

        if config.backup && !config.check && std::env::var("FORMATTER_BACKUP_RUN").is_err() {
            std::env::set_var("FORMATTER_BACKUP_RUN", Self::backup_run_id());
        }

        let finder = FileFinder::new(&config)?;
        let mut files = finder.collect()?;
        if files.is_empty() {
            println!("No files matched include/exclude patterns.");
            if let Some(journal) = run_journal.as_mut() {
                journal.finish_success(&RunSummary::default());
            }
            return Ok(());
        }
        files.sort();

        let parser_manager = ParserManager::with_full_config(
            config.clang_binary.clone(),
            config.clang_args.clone(),
            config.clang_compdb_path.clone(),
            config.clang_args_mode,
            config.cpp_standard.clone(),
            config.semantic_require_compdb,
            !config.semantic_disable_inferred_includes,
        );
        let project_graph_runtime = Self::open_project_graph_runtime(&config)?;
        let file_io = FileIo::new(&config);

        let effective_jobs = Self::resolve_effective_jobs(config.jobs);
        let effective_processes =
            Self::resolve_multiprocess_worker_count(config.processes, effective_jobs, files.len());
        let multi_process_enabled = effective_processes > 1 && files.len() > 1;
        if config.verbose {
            let worker_job_plan = if multi_process_enabled {
                Self::distribute_worker_jobs(effective_jobs, effective_processes)
            } else {
                vec![effective_jobs]
            };
            debug!(
                files = files.len(),
                requested_processes = config.processes,
                requested_jobs = config.jobs,
                effective_processes,
                effective_jobs,
                worker_job_plan = ?worker_job_plan,
                multiprocess = multi_process_enabled,
                "execution plan"
            );
        }

        let observation_started = Instant::now();
        let population_context = {
            let certainty_path = config
                .check_result_cache_path
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .join("certainty_filter_state.bin");
            let warm_start = crate::engine::filter_store::CertaintyFilterStore::load_from_path(
                &certainty_path,
                None,
            );
            let mut obs_config = config.clone();
            obs_config.check = true;
            let max_iterations = 10usize;
            let mut prev_population: Option<PopulationContext> = warm_start.and_then(|store| {
                if !store.has_sufficient_observations(1) {
                    return None;
                }
                let measurements = store.extract_measurements();
                if measurements.is_empty() {
                    return None;
                }
                let ctx = PopulationContext::compute_from_measurements(&measurements);
                if ctx.file_count == 0 {
                    return None;
                }
                if config.verbose {
                    info!(
                        population_file_count = ctx.file_count,
                        "warm start from persisted certainty state"
                    );
                }
                Some(ctx)
            });
            let mut final_population: Option<PopulationContext> = prev_population.clone();
            let obs_max = if prev_population.is_some() { 1 } else { max_iterations };
            if prev_population.is_some() && config.verbose {
                info!("warm start available — limiting observation to 1 iteration");
            }
            for iteration in 0..obs_max {
                let obs_results = if multi_process_enabled {
                    Self::run_multiprocess_pass(
                        &args,
                        &obs_config,
                        files.clone(),
                        effective_processes,
                        false,
                        prev_population.clone(),
                    )?
                } else {
                    Self::run_processing_pass(
                        &obs_config,
                        files.clone(),
                        project_graph_runtime.clone(),
                        false,
                        false,
                        prev_population.clone(),
                    )?
                };
                let certainties: Vec<&crate::engine::catalog::PolicyCertainty> = obs_results
                    .iter()
                    .filter_map(|r| r.policy_certainty.as_ref())
                    .collect();
                let measurements: Vec<[f64; 5]> = certainties
                    .iter()
                    .map(|c| [c.structural, c.semantic, c.coverage, c.richness, c.edit_success])
                    .collect();
                if measurements.is_empty() {
                    break;
                }
                let ctx = PopulationContext::compute_from_measurements(&measurements);
                if ctx.file_count == 0 {
                    break;
                }
                let stable_model_avg = if certainties.is_empty() {
                    0.0
                } else {
                    certainties.iter().map(|c| c.stable_model_prob).sum::<f64>()
                        / certainties.len() as f64
                };
                let variance_converged = prev_population.as_ref().is_some_and(|prev| {
                    let prev_var: f64 = prev.dim_stats.iter().map(|d| d.variance).sum();
                    let curr_var: f64 = ctx.dim_stats.iter().map(|d| d.variance).sum();
                    let delta = (prev_var - curr_var).abs();
                    delta < prev_var * 0.01
                });
                let obs_median = (iteration as u32 + 1)
                    + if prev_population.is_some() && iteration == 0 { 1 } else { 0 };
                let kalman_converged = crate::engine::fuzzy_inference::fuzzy_observation_converged(
                    stable_model_avg,
                    obs_median,
                    certainties.len(),
                );
                let converged = variance_converged && kalman_converged;
                if config.verbose {
                    info!(
                        iteration = iteration + 1,
                        max_iterations,
                        converged,
                        variance_converged,
                        kalman_converged,
                        stable_model_avg = format!("{:.4}", stable_model_avg),
                        obs_median,
                        population_file_count = ctx.file_count,
                        "observation iteration"
                    );
                }
                prev_population = Some(ctx.clone());
                let mut final_ctx = ctx;
                final_ctx.prior_observation_count = obs_median;
                final_population = Some(final_ctx);
                if converged {
                    break;
                }
            }
            final_population
        };
        if config.verbose {
            info!(
                files = files.len(),
                population_file_count = population_context.as_ref().map(|c| c.file_count).unwrap_or(0),
                elapsed_ms = observation_started.elapsed().as_millis() as u64,
                "observation phase complete"
            );
        }

        let population_edit_success = population_context
            .as_ref()
            .filter(|ctx| ctx.file_count >= 3)
            .map(|ctx| ctx.prior_estimates[4].clamp(0.0, 1.0))
            .unwrap_or(0.5);
        let started = Instant::now();
        let mut results = if multi_process_enabled {
            Self::run_multiprocess_pass(&args, &config, files, effective_processes, false, population_context)?
        } else {
            Self::run_processing_pass(&config, files, project_graph_runtime.clone(), true, true, population_context)?
        };

        results.sort_by(|left, right| left.path.cmp(&right.path));

        let parallel_pool = if effective_jobs > 1 {
            rayon::ThreadPoolBuilder::new()
                .num_threads(effective_jobs)
                .build()
                .ok()
        } else {
            None
        };

        if !config.check {
            Self::apply_project_wide_semantic_renames(
                &file_io,
                &parser_manager,
                &mut results,
                parallel_pool.as_ref(),
                population_edit_success,
            );
            Self::apply_write_phase(&file_io, &mut results, parallel_pool.as_ref());
            BackupManifest::write(&config, results.as_slice()).with_context(|| {
                format!(
                    "failed writing backup manifest under {}",
                    config.backup_dir.display()
                )
            })?;
        }
        let project_graph_stats = if let Some(project_graph) = project_graph_runtime.as_ref() {
            if config.check {
                if config.convergence_learn_on_check {
                    Self::refresh_project_graph(
                        project_graph,
                        &file_io,
                        &parser_manager,
                        &config.project_graph,
                        &mut results,
                        parallel_pool.as_ref(),
                        false,
                    )
                } else {
                    None
                }
            } else {
                Self::refresh_project_graph(
                    project_graph,
                    &file_io,
                    &parser_manager,
                    &config.project_graph,
                    &mut results,
                    parallel_pool.as_ref(),
                    true,
                )
            }
        } else {
            None
        };

        let report_dir = config.report_path.parent().unwrap_or(Path::new("."));
        let report_name = config
            .report_path
            .file_name()
            .unwrap_or("report.ndjson".as_ref());
        let run_ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let run_report_path = report_dir.join(run_ts.to_string()).join(report_name);
        let reporter = ReporterProcess::spawn(run_report_path)
            .context("failed spawning reporter process")?;
        for result in &results {
            reporter.try_send(ReportRecord::from(result));
        }

        let mut summary = RunSummary::default();
        let mut total_edits = 0usize;
        let mut backup_writes = 0usize;
        for result in &results {
            summary.merge_file(
                result.changed,
                result.violations.len(),
                result.error.is_some(),
                result.warnings.iter().filter(|w| !w.starts_with("internal:")).count(),
            );
            total_edits += result.edits.len();
            if result.backup_path.is_some() {
                backup_writes += 1;
            }

            if config.verbose {
                if let Some(error) = &result.error {
                    error!(path = %result.path.display(), error = %error, "file failed");
                }
                for warning in &result.warnings {
                    if warning.starts_with("internal:") {
                        continue;
                    }
                    warn!(path = %result.path.display(), warning = %warning, "file warning");
                }
                for violation in &result.violations {
                    let column = violation.column.unwrap_or(1);
                    println!(
                        "VIOLATION {}:{}:{} [{}] {}",
                        result.path.display(),
                        violation.line,
                        column,
                        violation.policy,
                        violation.message
                    );
                }
                for edit in &result.edits {
                    println!(
                        "EDIT {}:{} [{}] '{}' -> '{}'",
                        result.path.display(),
                        edit.line,
                        edit.policy,
                        edit.before,
                        edit.after
                    );
                }
                if let Some(backup_path) = &result.backup_path {
                    println!(
                        "BACKUP {} -> {}",
                        result.path.display(),
                        backup_path.display()
                    );
                }
            }
        }

        let elapsed = started.elapsed();
        let throughput = summary.files_processed as f64 / elapsed.as_secs_f64().max(0.000_1);
        let contract_violation_count = Self::contract_violation_count(results.as_slice());

        println!("files processed: {}", summary.files_processed);
        println!("files changed: {}", summary.files_changed);
        println!("violations: {}", summary.violations);
        println!("contract violations: {}", contract_violation_count);
        println!("errors: {}", summary.errors);
        println!("warnings: {}", summary.warnings);
        println!("edits: {}", total_edits);
        println!("backups: {}", backup_writes);
        println!("elapsed: {:.3}s", elapsed.as_secs_f64());
        println!("throughput: {:.2} files/s", throughput);
        if let Some(stats) = project_graph_stats {
            println!(
                "project_graph: generation={} prune={} tombstone={} nodes {}->{} edges {}->{} metrics {}->{} tombstones {}->{}",
                stats.generation,
                stats.prune_enabled,
                stats.tombstone_enabled,
                stats.before.nodes,
                stats.after.nodes,
                stats.before.edges,
                stats.after.edges,
                stats.before.metrics,
                stats.after.metrics,
                stats.before.tombstones,
                stats.after.tombstones
            );
            if stats.changed() {
                println!(
                    "project_graph_pruned: nodes={} edges={} metrics={} tombstones_added={} tombstones_removed={}",
                    stats.nodes_removed(),
                    stats.edges_removed(),
                    stats.metrics_removed(),
                    stats.tombstones_added(),
                    stats.tombstones_removed()
                );
            }
        }

        if tracing::enabled!(Level::INFO) {
            let telemetry = PolicyTelemetry::snapshot_sorted();
            if !telemetry.is_empty() {
                info!(policies = telemetry.len(), "policy telemetry summary");
                for item in telemetry.iter().take(8) {
                    info!(
                        policy = %item.policy,
                        runs = item.entry.runs,
                        failures = item.entry.failures,
                        fatals = item.entry.fatals,
                        blocked = item.entry.blocked,
                        confidence_decisions = item.entry.confidence_decisions,
                        confidence_apply = item.entry.confidence_apply,
                        confidence_apply_partial = item.entry.confidence_apply_partial,
                        confidence_advisory_only = item.entry.confidence_advisory_only,
                        confidence_block = item.entry.confidence_block,
                        reason_low_consensus = item.entry.reason_low_consensus,
                        reason_parser_disagreement = item.entry.reason_parser_disagreement,
                        reason_clang_diagnostics = item.entry.reason_clang_diagnostics,
                        edits = item.entry.total_edits,
                        violations = item.entry.total_violations,
                        avg_ms = item.entry.avg_elapsed_ms(),
                        total_ms = item.entry.total_elapsed_ms(),
                        max_ms = item.entry.max_elapsed_ns as f64 / 1_000_000.0,
                        "policy telemetry"
                    );
                }
            }

            let adaptive = AdaptiveTelemetry::snapshot();
            if adaptive.threshold_evaluations > 0 || adaptive.outcomes_total() > 0 {
                info!(
                    threshold_evaluations = adaptive.threshold_evaluations,
                    threshold_applied = adaptive.threshold_applied,
                    threshold_canary = adaptive.threshold_canary,
                    threshold_suspended = adaptive.threshold_suspended,
                    confidence_decisions = adaptive.confidence_decisions,
                    confidence_apply = adaptive.confidence_apply,
                    confidence_apply_partial = adaptive.confidence_apply_partial,
                    confidence_advisory_only = adaptive.confidence_advisory_only,
                    confidence_block = adaptive.confidence_block,
                    reason_low_consensus = adaptive.reason_low_consensus,
                    reason_parser_consensus_strict = adaptive.reason_parser_consensus_strict,
                    reason_parser_consensus_adaptive_hardened =
                        adaptive.reason_parser_consensus_adaptive_hardened,
                    reason_parser_consensus_adaptive_relaxed =
                        adaptive.reason_parser_consensus_adaptive_relaxed,
                    reason_context_coverage_low = adaptive.reason_context_coverage_low,
                    reason_semantic_consensus_low = adaptive.reason_semantic_consensus_low,
                    reason_parser_disagreement = adaptive.reason_parser_disagreement,
                    reason_clang_diagnostics = adaptive.reason_clang_diagnostics,
                    first_pass = adaptive.outcomes_first_pass,
                    after_retry = adaptive.outcomes_after_retry,
                    reverted = adaptive.outcomes_reverted,
                    rollback_events = adaptive.rollback_events,
                    last_threshold = adaptive.last_threshold,
                    last_delta = adaptive.last_delta,
                    ema_failure = adaptive.last_ema_failure_rate,
                    ema_revert = adaptive.last_ema_revert_rate,
                    max_abs_drift = adaptive.max_abs_drift,
                    "adaptive confidence telemetry"
                );
            }
        }

        let adaptive = AdaptiveTelemetry::snapshot();
        if adaptive.threshold_evaluations > 0 || adaptive.outcomes_total() > 0 {
            println!(
                "adaptive_confidence: outcomes(first_pass={}, after_retry={}, reverted={}) threshold(apply={}, canary={}, suspended={}) confidence(decisions={}, apply={}, partial={}, advisory={}, block={}) reasons(low_consensus={}, parser_disagreement={}, clang_diagnostics={}) rollbacks={} drift(max={:.3}, last={:.3}) ema(failure={:.3}, revert={:.3}) delta(last={:.3})",
                adaptive.outcomes_first_pass,
                adaptive.outcomes_after_retry,
                adaptive.outcomes_reverted,
                adaptive.threshold_applied,
                adaptive.threshold_canary,
                adaptive.threshold_suspended,
                adaptive.confidence_decisions,
                adaptive.confidence_apply,
                adaptive.confidence_apply_partial,
                adaptive.confidence_advisory_only,
                adaptive.confidence_block,
                adaptive.reason_low_consensus,
                adaptive.reason_parser_disagreement,
                adaptive.reason_clang_diagnostics,
                adaptive.rollback_events,
                adaptive.max_abs_drift,
                adaptive.last_drift,
                adaptive.last_ema_failure_rate,
                adaptive.last_ema_revert_rate,
                adaptive.last_delta
            );
        }

        reporter.finish().with_context(|| {
            format!(
                "failed finishing reporter process at {}",
                report_dir.display()
            )
        })?;
        let gate_rollout_signal = if config.accuracy_gate.enabled {
            Self::collect_accuracy_gate_rollout_signal(results.as_slice())
        } else {
            None
        };
        let benchmark_spawned = Self::spawn_async_accuracy_benchmark(&args, &config)?;
        if benchmark_spawned {
            println!("accuracy_benchmark: scheduled async");
        } else {
            Self::finalize_accuracy_validation(
                &mut config,
                &mut accuracy_rollout_state,
                requested_rollout_profile,
                requested_fail_closed,
                gate_rollout_signal,
            )?;
        }
        if Self::is_ci_environment()
            && config.accuracy_gate.ci_require_benchmark
            && config.accuracy_gate.enabled
            && contract_violation_count > 0
        {
            return Err(anyhow!(
                "accuracy gate CI contract violation threshold exceeded: {} > 0",
                contract_violation_count
            ));
        }
        if let Some(journal) = run_journal.as_mut() {
            journal.finish_success(&summary);
        }

        Ok(())
    }

    pub(crate) fn is_benchmark_source_file(path: &Path) -> bool {
        matches!(
            path.extension()
                .and_then(|value| value.to_str())
                .map(|value| value.to_ascii_lowercase())
                .as_deref(),
            Some("c")
                | Some("cc")
                | Some("cpp")
                | Some("cxx")
                | Some("h")
                | Some("hh")
                | Some("hpp")
                | Some("hxx")
                | Some("ipp")
                | Some("inl")
        )
    }

    pub(crate) fn write_synthetic_compile_commands(
        output_path: &Path,
        files: &[PathBuf],
        include_root: &Path,
        cpp_standard: &str,
    ) -> Result<()> {
        let include_root =
            fs::canonicalize(include_root).unwrap_or_else(|_| include_root.to_path_buf());
        let include_root_arg = format!("-I{}", include_root.to_string_lossy());
        let std_flag = format!("-std={cpp_standard}");
        let mut commands = Vec::<SyntheticCompileCommand>::with_capacity(files.len());
        for file in files {
            let canonical = fs::canonicalize(file).unwrap_or_else(|_| file.clone());
            let directory = canonical
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| include_root.clone());
            let file_arg = canonical.to_string_lossy().to_string();
            commands.push(SyntheticCompileCommand {
                directory: directory.to_string_lossy().to_string(),
                file: file_arg.clone(),
                arguments: vec![
                    "clang++".to_string(),
                    std_flag.clone(),
                    "-x".to_string(),
                    "c++".to_string(),
                    "-fsyntax-only".to_string(),
                    include_root_arg.clone(),
                    file_arg,
                ],
            });
        }
        let payload = serde_json::to_vec(commands.as_slice())
            .context("failed serializing synthetic benchmark compile_commands")?;
        fs::write(output_path, payload).with_context(|| {
            format!(
                "failed writing synthetic benchmark compile_commands {}",
                output_path.display()
            )
        })?;
        Ok(())
    }

    pub(crate) fn to_worker_file_result(result: FileResult) -> WorkerFileResult {
        let mut convergence_pairs = Vec::with_capacity(result.convergence_pairs.len());
        for ((loser, winner), count) in result.convergence_pairs {
            convergence_pairs.push(WorkerConvergencePair {
                loser: loser.into(),
                winner: winner.into(),
                count,
            });
        }
        WorkerFileResult {
            path: result.path,
            changed: result.changed,
            pending_text: result.pending_text,
            semantic_rename_plans: result.semantic_rename_plans,
            convergence_pairs,
            violations: result.violations,
            edits: result.edits,
            policy_traces: result.policy_traces,
            accuracy_gate: result.accuracy_gate,
            error: result.error,
            warnings: result.warnings,
            elapsed_engine_ms: result.elapsed_engine_ms,
            elapsed_total_ms: result.elapsed_total_ms,
            policy_certainty: result.policy_certainty,
        }
    }

    pub(crate) fn from_worker_file_result(result: WorkerFileResult) -> FileResult {
        let mut convergence_pairs = BTreeMap::new();
        for pair in result.convergence_pairs {
            if pair.count == 0 {
                continue;
            }
            *convergence_pairs
                .entry((pair.loser.to_string(), pair.winner.to_string()))
                .or_insert(0usize) += pair.count;
        }
        FileResult {
            path: result.path,
            changed: result.changed,
            pending_text: result.pending_text,
            semantic_rename_plans: result.semantic_rename_plans,
            convergence_pairs,
            violations: result.violations,
            edits: result.edits,
            policy_traces: result.policy_traces,
            accuracy_gate: result.accuracy_gate,
            error: result.error,
            backup_path: None,
            warnings: result.warnings,
            elapsed_engine_ms: result.elapsed_engine_ms,
            elapsed_total_ms: result.elapsed_total_ms,
            policy_certainty: result.policy_certainty,
        }
    }

    pub(crate) fn to_worker_dispatch_observation(
        observation: DispatchObservation,
    ) -> WorkerDispatchObservation {
        WorkerDispatchObservation {
            path_identity: observation.path_identity,
            observed_size: observation.observed_size,
            observed_lines: observation.observed_lines,
            elapsed_wall_ns: observation.elapsed_wall_ns,
            total_wall_ns: observation.total_wall_ns,
            retry_effort_units: observation.retry_effort_units,
            retry_penalty_ns: observation.retry_penalty_ns,
            error: observation.error,
        }
    }

    pub(crate) fn from_worker_dispatch_observation(
        observation: WorkerDispatchObservation,
    ) -> DispatchObservation {
        DispatchObservation {
            path_identity: observation.path_identity,
            observed_size: observation.observed_size,
            observed_lines: observation.observed_lines,
            elapsed_wall_ns: observation.elapsed_wall_ns,
            total_wall_ns: observation.total_wall_ns,
            retry_effort_units: observation.retry_effort_units,
            retry_penalty_ns: observation.retry_penalty_ns,
            error: observation.error,
        }
    }

    pub(crate) fn validate_worker_shard_coverage(
        index: usize,
        results: &[WorkerFileResult],
        expected_paths: &[PathBuf],
    ) -> Result<()> {
        let expected_counts = Self::path_counts_from_paths(expected_paths);
        let actual_counts = Self::path_counts_from_worker_results(results);
        if expected_counts == actual_counts {
            return Ok(());
        }
        let mut missing = Vec::<String>::new();
        let mut extras = Vec::<String>::new();
        for (key, expected) in &expected_counts {
            let actual = actual_counts.get(key).copied().unwrap_or(0);
            if actual < *expected {
                missing.push(format!("{key} ({actual}/{expected})"));
            }
        }
        for (key, actual) in &actual_counts {
            let expected = expected_counts.get(key).copied().unwrap_or(0);
            if *actual > expected {
                extras.push(format!("{key} ({actual}/{expected})"));
            }
        }
        missing.sort();
        extras.sort();
        Err(anyhow!(
            "worker {} produced non-deterministic shard coverage: missing [{}]; extras [{}]",
            index + 1,
            missing.join(", "),
            extras.join(", ")
        ))
    }

    fn path_counts_from_paths(paths: &[PathBuf]) -> HashMap<String, usize> {
        let mut counts = HashMap::<String, usize>::new();
        for path in paths {
            let key = Self::path_identity(path.as_path());
            *counts.entry(key).or_insert(0) += 1;
        }
        counts
    }

    fn path_counts_from_worker_results(results: &[WorkerFileResult]) -> HashMap<String, usize> {
        let mut counts = HashMap::<String, usize>::new();
        for result in results {
            let key = Self::path_identity(result.path.as_path());
            *counts.entry(key).or_insert(0) += 1;
        }
        counts
    }

    pub(crate) fn coverage_fingerprint_from_paths(paths: &[PathBuf]) -> String {
        let counts = Self::path_counts_from_paths(paths);
        Self::coverage_fingerprint_from_counts(&counts)
    }

    pub(crate) fn coverage_fingerprint_from_worker_results(results: &[WorkerFileResult]) -> String {
        let counts = Self::path_counts_from_worker_results(results);
        Self::coverage_fingerprint_from_counts(&counts)
    }

    fn coverage_fingerprint_from_counts(counts: &HashMap<String, usize>) -> String {
        let mut entries = counts
            .iter()
            .map(|(path, count)| format!("{path}:{count}"))
            .collect::<Vec<_>>();
        entries.sort();
        let payload = entries.join("|");
        let checksum = crc32fast::hash(payload.as_bytes());
        format!("{checksum:08x}-{:x}", payload.len())
    }

    pub(crate) fn validate_and_order_multiprocess_results(
        results: Vec<FileResult>,
        expected_paths: &[PathBuf],
    ) -> Result<Vec<FileResult>> {
        let mut expected_counts = HashMap::<String, usize>::new();
        for path in expected_paths {
            let key = Self::path_identity(path.as_path());
            *expected_counts.entry(key).or_insert(0) += 1;
        }
        let mut keyed = Vec::<(String, FileResult)>::with_capacity(results.len());
        for result in results {
            let key = Self::path_identity(result.path.as_path());
            keyed.push((key, result));
        }

        let mut actual_counts = HashMap::<String, usize>::new();
        for (key, _) in &keyed {
            *actual_counts.entry(key.clone()).or_insert(0) += 1;
        }
        if expected_counts != actual_counts {
            let mut missing = Vec::<String>::new();
            let mut extras = Vec::<String>::new();
            for (key, expected) in &expected_counts {
                let actual = actual_counts.get(key).copied().unwrap_or(0);
                if actual < *expected {
                    missing.push(format!("{key} ({actual}/{expected})"));
                }
            }
            for (key, actual) in &actual_counts {
                let expected = expected_counts.get(key).copied().unwrap_or(0);
                if *actual > expected {
                    extras.push(format!("{key} ({actual}/{expected})"));
                }
            }
            missing.sort();
            extras.sort();
            return Err(anyhow!(
                "multiprocess deterministic merge mismatch: missing [{}]; extras [{}]",
                missing.join(", "),
                extras.join(", ")
            ));
        }

        keyed.sort_by(|(left_key, left_result), (right_key, right_result)| {
            left_key
                .cmp(right_key)
                .then_with(|| left_result.path.cmp(&right_result.path))
        });
        Ok(keyed.into_iter().map(|(_, result)| result).collect())
    }

    pub(crate) fn wait_worker_output_with_timeout(
        mut child: Child,
        timeout: Duration,
        kill_grace: Duration,
    ) -> Result<std::process::Output> {
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => {
                    return child
                        .wait_with_output()
                        .context("failed collecting worker output");
                }
                Ok(None) => {}
                Err(err) => {
                    return Err(anyhow!("failed polling worker process: {err}"));
                }
            }

            if started.elapsed() >= timeout {
                let _ = child.kill();
                let kill_started = Instant::now();
                loop {
                    match child.try_wait() {
                        Ok(Some(_)) => break,
                        Ok(None) => {
                            if kill_started.elapsed() >= kill_grace {
                                break;
                            }
                            thread::sleep(Duration::from_millis(25));
                        }
                        Err(_) => break,
                    }
                }
                let stderr = child
                    .wait_with_output()
                    .map(|output| Self::worker_log_excerpt(output.stderr.as_slice(), 48))
                    .unwrap_or_else(|_| "<stderr unavailable>".to_string());
                return Err(anyhow!(
                    "worker timed out after {}s (kill grace {}s); stderr: {}",
                    timeout.as_secs(),
                    kill_grace.as_secs(),
                    stderr
                ));
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    pub(crate) fn merge_worker_learning_states(
        config: &AppConfig,
        adaptive_state_shards: &[PathBuf],
        retry_optimizer_state_shards: &[PathBuf],
    ) -> Result<()> {
        let _ = adaptive_state_shards;
        if config.retry_strategy_optimizer.enabled {
            RetryStrategyOptimizer::merge_state_files(
                config.retry_strategy_optimizer.path.as_path(),
                retry_optimizer_state_shards,
                &config.retry_strategy_optimizer,
            )
            .with_context(|| {
                format!(
                    "failed merging retry strategy optimizer worker states into {}",
                    config.retry_strategy_optimizer.path.display()
                )
            })?;
        }
        Ok(())
    }

    pub(crate) fn append_worker_base_args(
        command: &mut Command,
        args: &CliArgs,
        config: &AppConfig,
        worker_jobs: usize,
    ) {
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
        if config.check {
            command.arg("--check");
        }
        command.arg("--jobs").arg(worker_jobs.to_string());
    }

    #[cfg(test)]
    pub(crate) fn build_file_shards(files: Vec<PathBuf>, worker_count: usize) -> Vec<Vec<PathBuf>> {
        let parser_manager = ParserManager::new();
        let history = DispatchHistoryStore::ephemeral();
        let mut feature_cache = DispatchFeatureCache::ephemeral();
        Self::build_file_batches(
            files,
            worker_count,
            DispatchMode::WorkerProcess,
            &parser_manager,
            &history,
            &mut feature_cache,
        )
        .into_iter()
        .map(|batch| batch.paths)
        .collect()
    }

    pub(crate) fn build_file_batches(
        files: Vec<PathBuf>,
        parallelism: usize,
        mode: DispatchMode,
        parser_manager: &ParserManager,
        history: &DispatchHistoryStore,
        feature_cache: &mut DispatchFeatureCache,
    ) -> Vec<DispatchBatch> {
        DispatchScheduler::plan_batches(
            files,
            parallelism,
            mode,
            parser_manager,
            history,
            feature_cache,
        )
    }

    #[cfg(test)]
    pub(crate) fn schedule_files_for_parallel(files: Vec<PathBuf>) -> Vec<PathBuf> {
        let parser_manager = ParserManager::new();
        let history = DispatchHistoryStore::ephemeral();
        let mut feature_cache = DispatchFeatureCache::ephemeral();
        Self::build_file_batches(
            files,
            1,
            DispatchMode::InProcess,
            &parser_manager,
            &history,
            &mut feature_cache,
        )
        .into_iter()
        .flat_map(|batch| batch.paths)
        .collect()
    }

    #[cfg(test)]
    pub(crate) fn path_sort_key(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "/")
    }

    pub(crate) fn worker_temp_root() -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "fmt_workers_{}_{}",
            std::process::id(),
            stamp
        ))
    }

    pub(crate) fn worker_log_excerpt(bytes: &[u8], max_lines: usize) -> String {
        let text = String::from_utf8_lossy(bytes);
        let mut lines = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        if lines.is_empty() {
            return "<no stderr output>".to_string();
        }
        if lines.len() > max_lines.max(1) {
            let keep = max_lines.max(1);
            lines = lines.split_off(lines.len() - keep);
        }
        lines.join(" | ")
    }

    pub(crate) fn is_ci_environment() -> bool {
        std::env::var("CI")
            .ok()
            .map(|value| {
                let normalized = value.trim().to_ascii_lowercase();
                !normalized.is_empty() && normalized != "0" && normalized != "false"
            })
            .unwrap_or(false)
    }

    pub(crate) fn backup_run_id() -> String {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_millis().to_string())
            .unwrap_or_else(|_| "0".to_string())
    }

    pub(crate) fn check_result_cache_fingerprint(config: &AppConfig) -> String {
        let mut lines = Vec::<String>::new();
        lines.push(format!("root={}", config.root.display()));
        lines.push(format!("style={}", config.style_name));
        lines.push(format!("check={}", config.check));
        lines.push(format!("clang_binary={}", config.clang_binary));
        lines.push(format!(
            "clang_compdb_path={}",
            config
                .clang_compdb_path
                .as_ref()
                .map(|value| value.display().to_string())
                .unwrap_or_default()
        ));
        lines.push(format!("clang_args_mode={:?}", config.clang_args_mode));
        lines.push(format!(
            "semantic_fidelity=require_compdb:{} disable_inferred_includes:{}",
            config.semantic_require_compdb, config.semantic_disable_inferred_includes
        ));
        lines.push(format!(
            "accuracy_gate={}:{:.3}:{:.3}:{}:{}:{}:{}:{}:{}:{}",
            config.accuracy_gate.enabled,
            config.accuracy_gate.min_precision,
            config.accuracy_gate.min_recall,
            config.accuracy_gate.min_samples,
            config.accuracy_gate.semantic_required,
            config.accuracy_gate.fail_closed,
            config.accuracy_gate.profile.as_str(),
            config.accuracy_gate.rollout_defer_fail_closed_until_stable,
            config.accuracy_gate.rollout_stable_passes_required,
            config.accuracy_gate.ci_require_benchmark
        ));
        for include in &config.include_patterns {
            lines.push(format!("include={include}"));
        }
        for exclude in &config.exclude_patterns {
            lines.push(format!("exclude={exclude}"));
        }
        for arg in &config.clang_args {
            lines.push(format!("clang_arg={arg}"));
        }
        let mut policy_names = config.policy_settings.keys().cloned().collect::<Vec<_>>();
        policy_names.sort();
        for name in policy_names {
            if let Some(policy) = config.policy_settings.get(name.as_str()) {
                lines.push(format!(
                    "policy={}:{:?}:{:?}:{:?}:{}",
                    name, policy.enabled, policy.policy_type, policy.touch_contract, policy.raw
                ));
            }
        }
        let payload = lines.join("\n");
        let checksum = crc32fast::hash(payload.as_bytes());
        format!("{checksum:08x}-{:x}", payload.len())
    }

    pub(crate) fn path_identity(path: &std::path::Path) -> String {
        std::fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .replace('\\', "/")
    }

    pub(crate) fn collect_convergence_pairs(
        results: &[FileResult],
    ) -> BTreeMap<(String, String), usize> {
        let mut pairs = BTreeMap::<(String, String), usize>::new();
        for result in results {
            for ((loser, winner), count) in &result.convergence_pairs {
                if *count == 0 {
                    continue;
                }
                *pairs.entry((loser.clone(), winner.clone())).or_insert(0) += count;
            }
        }
        pairs
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::time::Duration;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::app::runner::{App, WorkerFileResult};
    use crate::engine::accuracy_gate::{
        AccuracyGateDecision, AccuracyGateReason, AccuracyGateStatus,
    };
    use crate::model::file_result::FileResult;
    use crate::runtime::scheduler::DispatchObservation;

    #[test]
    fn collect_convergence_pairs_aggregates_multiple_files() {
        let first = FileResult {
            path: PathBuf::from("a.hpp"),
            convergence_pairs: BTreeMap::from([(
                ("naming_conventions".to_string(), "clang_format".to_string()),
                2usize,
            )]),
            ..FileResult::default()
        };
        let second = FileResult {
            path: PathBuf::from("b.hpp"),
            convergence_pairs: BTreeMap::from([
                (
                    ("naming_conventions".to_string(), "clang_format".to_string()),
                    3usize,
                ),
                (
                    ("snake_case".to_string(), "clang_format".to_string()),
                    1usize,
                ),
            ]),
            ..FileResult::default()
        };
        let pairs = App::collect_convergence_pairs(&[first, second]);
        assert_eq!(
            pairs.get(&("naming_conventions".to_string(), "clang_format".to_string())),
            Some(&5usize)
        );
        assert_eq!(
            pairs.get(&("snake_case".to_string(), "clang_format".to_string())),
            Some(&1usize)
        );
    }

    #[test]
    fn resolve_effective_workers_caps_to_available_parallelism() {
        let available = App::available_parallelism();
        let requested = available.saturating_add(8);
        assert_eq!(App::resolve_effective_workers(requested), available);
        assert_eq!(App::resolve_effective_workers(0), available);
    }

    #[test]
    fn resolve_effective_jobs_respects_requested_budget() {
        let available = App::available_parallelism();
        let requested = available.saturating_add(8);
        assert_eq!(App::resolve_effective_jobs(requested), requested);
        assert_eq!(App::resolve_effective_jobs(0), available);
    }

    #[test]
    fn worker_log_excerpt_handles_empty_stderr() {
        assert_eq!(App::worker_log_excerpt(&[], 8), "<no stderr output>");
        assert_eq!(
            App::worker_log_excerpt(b"\n\n   \n", 8),
            "<no stderr output>"
        );
    }

    #[test]
    fn worker_dispatch_observation_round_trips() {
        let observation = DispatchObservation {
            path_identity: "src/a.cpp".to_string(),
            observed_size: 128,
            observed_lines: 16,
            elapsed_wall_ns: 5_000,
            total_wall_ns: 7_500,
            retry_effort_units: 3,
            retry_penalty_ns: 1_000,
            error: false,
        };
        let encoded = App::to_worker_dispatch_observation(observation.clone());
        let decoded = App::from_worker_dispatch_observation(encoded);
        assert_eq!(decoded.path_identity, observation.path_identity);
        assert_eq!(decoded.observed_size, observation.observed_size);
        assert_eq!(decoded.observed_lines, observation.observed_lines);
        assert_eq!(decoded.elapsed_wall_ns, observation.elapsed_wall_ns);
        assert_eq!(decoded.total_wall_ns, observation.total_wall_ns);
        assert_eq!(decoded.retry_effort_units, observation.retry_effort_units);
        assert_eq!(decoded.retry_penalty_ns, observation.retry_penalty_ns);
        assert_eq!(decoded.error, observation.error);
    }

    #[test]
    fn worker_log_excerpt_keeps_tail_lines() {
        let sample = b"line1\nline2\nline3\nline4\n";
        assert_eq!(
            App::worker_log_excerpt(sample, 2),
            "line3 | line4".to_string()
        );
    }

    #[test]
    fn validate_and_order_multiprocess_results_sorts_paths() {
        let expected = vec![
            PathBuf::from("include/B.hpp"),
            PathBuf::from("include/A.hpp"),
            PathBuf::from("src/C.cpp"),
        ];
        let results = vec![
            FileResult {
                path: PathBuf::from("src/C.cpp"),
                ..FileResult::default()
            },
            FileResult {
                path: PathBuf::from("include/B.hpp"),
                ..FileResult::default()
            },
            FileResult {
                path: PathBuf::from("include/A.hpp"),
                ..FileResult::default()
            },
        ];
        let ordered = App::validate_and_order_multiprocess_results(results, expected.as_slice())
            .expect("ordered results");
        assert_eq!(ordered.len(), 3);
        assert_eq!(ordered[0].path, PathBuf::from("include/A.hpp"));
        assert_eq!(ordered[1].path, PathBuf::from("include/B.hpp"));
        assert_eq!(ordered[2].path, PathBuf::from("src/C.cpp"));
    }

    #[test]
    fn validate_and_order_multiprocess_results_rejects_coverage_mismatch() {
        let expected = vec![PathBuf::from("a.cpp"), PathBuf::from("b.cpp")];
        let results = vec![
            FileResult {
                path: PathBuf::from("a.cpp"),
                ..FileResult::default()
            },
            FileResult {
                path: PathBuf::from("a.cpp"),
                ..FileResult::default()
            },
        ];
        let error = App::validate_and_order_multiprocess_results(results, expected.as_slice())
            .expect_err("coverage mismatch should fail");
        assert!(error.to_string().contains("deterministic merge mismatch"));
    }

    #[test]
    fn validate_worker_shard_coverage_accepts_exact_set() {
        let expected = vec![PathBuf::from("a.cpp"), PathBuf::from("b.cpp")];
        let results = vec![
            WorkerFileResult {
                path: PathBuf::from("b.cpp"),
                changed: false,
                pending_text: None,
                semantic_rename_plans: Vec::new(),
                convergence_pairs: Vec::new(),
                violations: Vec::new(),
                edits: Vec::new(),
                policy_traces: Vec::new(),
                accuracy_gate: None,
                error: None,
                warnings: Vec::new(),
                elapsed_engine_ms: 0.0,
                elapsed_total_ms: 0.0,
                policy_certainty: None,
            },
            WorkerFileResult {
                path: PathBuf::from("a.cpp"),
                changed: false,
                pending_text: None,
                semantic_rename_plans: Vec::new(),
                convergence_pairs: Vec::new(),
                violations: Vec::new(),
                edits: Vec::new(),
                policy_traces: Vec::new(),
                accuracy_gate: None,
                error: None,
                warnings: Vec::new(),
                elapsed_engine_ms: 0.0,
                elapsed_total_ms: 0.0,
                policy_certainty: None,
            },
        ];
        App::validate_worker_shard_coverage(0, results.as_slice(), expected.as_slice())
            .expect("coverage should match");
    }

    #[test]
    fn validate_worker_shard_coverage_rejects_duplicates() {
        let expected = vec![PathBuf::from("a.cpp"), PathBuf::from("b.cpp")];
        let results = vec![
            WorkerFileResult {
                path: PathBuf::from("a.cpp"),
                changed: false,
                pending_text: None,
                semantic_rename_plans: Vec::new(),
                convergence_pairs: Vec::new(),
                violations: Vec::new(),
                edits: Vec::new(),
                policy_traces: Vec::new(),
                accuracy_gate: None,
                error: None,
                warnings: Vec::new(),
                elapsed_engine_ms: 0.0,
                elapsed_total_ms: 0.0,
                policy_certainty: None,
            },
            WorkerFileResult {
                path: PathBuf::from("a.cpp"),
                changed: false,
                pending_text: None,
                semantic_rename_plans: Vec::new(),
                convergence_pairs: Vec::new(),
                violations: Vec::new(),
                edits: Vec::new(),
                policy_traces: Vec::new(),
                accuracy_gate: None,
                error: None,
                warnings: Vec::new(),
                elapsed_engine_ms: 0.0,
                elapsed_total_ms: 0.0,
                policy_certainty: None,
            },
        ];
        let error = App::validate_worker_shard_coverage(0, results.as_slice(), expected.as_slice())
            .expect_err("coverage mismatch should fail");
        assert!(error
            .to_string()
            .contains("non-deterministic shard coverage"));
    }

    #[test]
    fn coverage_fingerprint_is_order_invariant() {
        let first = vec![
            PathBuf::from("src/a.cpp"),
            PathBuf::from("src/b.cpp"),
            PathBuf::from("src/a.cpp"),
        ];
        let second = vec![
            PathBuf::from("src/b.cpp"),
            PathBuf::from("src/a.cpp"),
            PathBuf::from("src/a.cpp"),
        ];
        let left = App::coverage_fingerprint_from_paths(first.as_slice());
        let right = App::coverage_fingerprint_from_paths(second.as_slice());
        assert_eq!(left, right);
    }

    #[test]
    fn coverage_fingerprint_changes_when_multiplicity_changes() {
        let first = vec![PathBuf::from("src/a.cpp"), PathBuf::from("src/b.cpp")];
        let second = vec![
            PathBuf::from("src/a.cpp"),
            PathBuf::from("src/a.cpp"),
            PathBuf::from("src/b.cpp"),
        ];
        let left = App::coverage_fingerprint_from_paths(first.as_slice());
        let right = App::coverage_fingerprint_from_paths(second.as_slice());
        assert_ne!(left, right);
    }

    #[test]
    fn schedule_files_for_parallel_prefers_heavier_files_deterministically() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("fmt_sched_{stamp}"));
        fs::create_dir_all(temp_dir.as_path()).expect("create temp dir");
        let small = temp_dir.join("small.cpp");
        let medium = temp_dir.join("medium.cpp");
        let large = temp_dir.join("large.cpp");
        fs::write(small.as_path(), vec![b'a'; 32]).expect("write small");
        fs::write(medium.as_path(), vec![b'b'; 128]).expect("write medium");
        fs::write(large.as_path(), vec![b'c'; 512]).expect("write large");

        let ordered = App::schedule_files_for_parallel(vec![small.clone(), large.clone(), medium]);
        assert_eq!(ordered.first(), Some(&large));

        let _ = fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn build_file_shards_is_input_order_invariant() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("fmt_shards_{stamp}"));
        fs::create_dir_all(temp_dir.as_path()).expect("create temp dir");
        let files = [
            ("a.cpp", 32usize),
            ("b.cpp", 400usize),
            ("c.cpp", 96usize),
            ("d.cpp", 512usize),
            ("e.cpp", 24usize),
            ("f.cpp", 280usize),
        ]
        .into_iter()
        .map(|(name, size)| {
            let path = temp_dir.join(name);
            fs::write(path.as_path(), vec![b'x'; size]).expect("write fixture");
            path
        })
        .collect::<Vec<_>>();
        let mut reversed = files.clone();
        reversed.reverse();

        let left = App::build_file_shards(files, 3)
            .into_iter()
            .map(|mut shard| {
                shard.sort();
                shard
                    .into_iter()
                    .map(|path| App::path_sort_key(path.as_path()))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let right = App::build_file_shards(reversed, 3)
            .into_iter()
            .map(|mut shard| {
                shard.sort();
                shard
                    .into_iter()
                    .map(|path| App::path_sort_key(path.as_path()))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        assert_eq!(left, right);

        let _ = fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn build_file_shards_keeps_paired_units_together() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("fmt_unit_shards_{stamp}"));
        let include_dir = temp_dir.join("include");
        let src_dir = temp_dir.join("src");
        fs::create_dir_all(include_dir.as_path()).expect("create include dir");
        fs::create_dir_all(src_dir.as_path()).expect("create src dir");
        let pair_header = include_dir.join("Widget.hpp");
        let pair_impl = src_dir.join("Widget.cpp");
        let heavy = temp_dir.join("Heavy.cpp");
        let light = temp_dir.join("Light.hpp");
        fs::write(pair_header.as_path(), vec![b'a'; 64]).expect("write pair header");
        fs::write(pair_impl.as_path(), vec![b'b'; 96]).expect("write pair impl");
        fs::write(heavy.as_path(), vec![b'c'; 1024]).expect("write heavy");
        fs::write(light.as_path(), vec![b'd'; 48]).expect("write light");

        let shards = App::build_file_shards(
            vec![
                pair_header.clone(),
                pair_impl.clone(),
                heavy.clone(),
                light.clone(),
            ],
            2,
        );
        assert!(shards
            .iter()
            .any(|shard| { shard.contains(&pair_header) && shard.contains(&pair_impl) }));

        let _ = fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn accuracy_gate_decision_renders_boundary_message() {
        let decision = AccuracyGateDecision {
            status: AccuracyGateStatus::WarningOnly,
            precision: 0.923,
            recall: 0.811,
            reasons: vec![
                AccuracyGateReason::SemanticRequiredUnmet,
                AccuracyGateReason::PrecisionBelowThreshold {
                    actual: 0.923,
                    minimum: 0.940,
                },
            ],
        };
        assert_eq!(
            decision.summary(),
            "accuracy_gate: precision=0.923 recall=0.811 reasons=[semantic_required_unmet,precision_below_threshold(0.923 < 0.940)]"
        );
    }

    #[test]
    fn collects_gate_rollout_signal_from_file_results() {
        let results = vec![
            FileResult {
                path: PathBuf::from("a.cpp"),
                accuracy_gate: Some(AccuracyGateDecision {
                    status: AccuracyGateStatus::WarningOnly,
                    precision: 1.0,
                    recall: 1.0,
                    reasons: vec![AccuracyGateReason::SemanticRequiredUnmet],
                }),
                ..FileResult::default()
            },
            FileResult {
                path: PathBuf::from("b.cpp"),
                accuracy_gate: Some(AccuracyGateDecision {
                    status: AccuracyGateStatus::FailedClosed,
                    precision: 0.3,
                    recall: 0.2,
                    reasons: vec![AccuracyGateReason::PrecisionBelowThreshold {
                        actual: 0.3,
                        minimum: 0.920,
                    }],
                }),
                error: Some("pipeline failed: accuracy gate fail-closed for b.cpp".to_string()),
                ..FileResult::default()
            },
            FileResult {
                path: PathBuf::from("c.cpp"),
                error: None,
                ..FileResult::default()
            },
        ];
        let signal = App::collect_accuracy_gate_rollout_signal(results.as_slice())
            .expect("gate rollout signal");
        assert_eq!(signal.considered_files, 3);
        assert_eq!(signal.failing_files, 2);
        assert_eq!(signal.semantic_required_unmet_files, 1);
        assert!((signal.precision - ((1.0 + 0.3 + 1.0) / 3.0)).abs() <= 1e-9);
        assert!((signal.recall - ((1.0 + 0.2 + 1.0) / 3.0)).abs() <= 1e-9);
        assert!((signal.match_ratio - (1.0 / 3.0)).abs() <= 1e-9);
    }

    #[cfg(unix)]
    #[test]
    fn worker_wait_timeout_reports_hung_worker() {
        let child = Command::new("sh")
            .arg("-c")
            .arg("sleep 2")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sleepy worker");
        let err = App::wait_worker_output_with_timeout(
            child,
            Duration::from_millis(100),
            Duration::from_millis(50),
        )
        .expect_err("worker should time out");
        assert!(err.to_string().contains("timed out"));
    }

    #[cfg(unix)]
    #[test]
    fn worker_wait_timeout_collects_successful_output() {
        let child = Command::new("sh")
            .arg("-c")
            .arg("printf ok")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn quick worker");
        let output = App::wait_worker_output_with_timeout(
            child,
            Duration::from_secs(2),
            Duration::from_millis(50),
        )
        .expect("worker should complete");
        assert!(output.status.success());
    }
}
