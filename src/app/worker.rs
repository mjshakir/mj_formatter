use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use rustc_hash::FxHashMap;
use rayon::prelude::*;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tracing::{info, warn};

use crate::app::runner::{App, TempDirCleanupGuard, WorkerManifest, WorkerOutput};
use crate::cli::args::CliArgs;
use crate::config::app_config::AppConfig;
use crate::config::loader::{
    ADAPTIVE_CONFIDENCE_STATE_PATH_ENV,
    RETRY_STRATEGY_OPTIMIZER_STATE_PATH_ENV,
};
use crate::engine::coordinator::FormatterEngine;
use crate::engine::pipeline::PolicyPipeline;
use crate::files::file_io::FileIo;
use crate::model::file_result::FileResult;
use crate::parser::clang_service::ClangParseService;
use crate::parser::manager::ParserManager;
use crate::policy::registry::PolicyRegistry;
use crate::runtime::adaptive_telemetry::{AdaptiveTelemetry, AdaptiveSnapshot};
use crate::runtime::result_cache::CheckResultCache;
use crate::runtime::scheduler::{
    persist_batch_telemetry, DispatchBatchTelemetry, DispatchFeatureCache, DispatchHistoryStore,
    DispatchMode, DispatchObservation,
};
use crate::runtime::processor::FileProcessor;
use crate::runtime::cluster_telemetry::PolicyClusterTelemetry;
use crate::runtime::telemetry::{
    PolicyTelemetry, PolicyTelemetryEntry, PolicyTelemetrySnapshotEntry,
};
use crate::runtime::graph_runtime::ProjectGraphRuntime;
use crate::runtime::retry_telemetry::RetryLearningTelemetry;

struct WorkerBatchTask {
    index: usize,
    estimated_cost: u64,
    files: Vec<PathBuf>,
    expected_coverage_fingerprint: String,
}

#[derive(Default)]
struct WorkerControllerOutput {
    results: Vec<FileResult>,
    dispatch_observations: Vec<DispatchObservation>,
    batch_telemetry: Vec<DispatchBatchTelemetry>,
    adaptive_state_shards: Vec<PathBuf>,
    retry_optimizer_state_shards: Vec<PathBuf>,
    restart_count: usize,
    worker_failures: usize,
}

struct ProcessingPassResult {
    results: Vec<FileResult>,
    observations: Vec<DispatchObservation>,
    batch_telemetry: Vec<DispatchBatchTelemetry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
enum PersistentWorkerRequest {
    Process(WorkerManifest),
    Shutdown,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
enum PersistentWorkerResponse {
    Output(Box<WorkerOutput>),
    Failure(String),
}

struct PersistentWorkerProcess {
    child: Child,
    stdin: io::BufWriter<ChildStdin>,
    responses: Receiver<Result<PersistentWorkerResponse>>,
}

impl App {
    fn encode_worker_payload<T: Serialize>(value: &T) -> Result<Vec<u8>> {
        postcard::to_allocvec(value)
            .context("failed serializing worker payload")
    }

    fn decode_worker_payload<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
        postcard::from_bytes(bytes)
            .context("failed parsing worker payload")
    }

    fn write_framed<W: Write, T: Serialize>(
        writer: &mut W,
        value: &T,
    ) -> Result<()> {
        let payload = Self::encode_worker_payload(value)?;
        crate::files::ecc_frame::write_frame(writer, &payload)
            .context("failed writing ecc-framed worker payload")?;
        writer.flush().context("failed flushing worker payload")
    }

    fn read_framed<R: Read, T: DeserializeOwned>(
        reader: &mut R,
    ) -> Result<Option<T>> {
        match crate::files::ecc_frame::read_frame(reader)? {
            Some(payload) => Ok(Some(Self::decode_worker_payload(&payload)?)),
            None => Ok(None),
        }
    }

    pub(crate) fn run_pool_entry(_args: &CliArgs, config: &AppConfig) -> Result<()> {
        let config = if std::env::var_os("FMT_OBSERVATION_ONLY").is_some() {
            let mut c = config.clone();
            c.observation_only = true;
            std::borrow::Cow::Owned(c)
        } else {
            std::borrow::Cow::Borrowed(config)
        };
        let config = config.as_ref();
        let project_graph_runtime = Self::open_project_graph_runtime(config)?;
        Self::seed_learning_state_from_project_graph(project_graph_runtime.as_ref());
        let initial_state = Self::load_adaptive_state_from_ipc();
        let adaptive_state = std::sync::Arc::new(arc_swap::ArcSwap::new(std::sync::Arc::new(
            initial_state,
        )));
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut reader = stdin.lock();
        let mut writer = stdout.lock();
        let mut batch_index = 0usize;
        while let Some(request) =
            Self::read_framed::<_, PersistentWorkerRequest>(&mut reader)?
        {
            match request {
                PersistentWorkerRequest::Process(manifest) => {
                    batch_index += 1;
                    info!(
                        batch = batch_index,
                        files = manifest.files.len(),
                        shard = manifest.shard_index,
                        "worker: received batch"
                    );
                    let batch_start = Instant::now();
                    let adaptive_baseline = AdaptiveTelemetry::snapshot();
                    let policy_baseline = PolicyTelemetry::snapshot_sorted();
                    let cluster_baseline = PolicyClusterTelemetry::snapshot_entries();
                    let retry_baseline = RetryLearningTelemetry::snapshot();
                    info!(batch = batch_index, "worker: starting processing pass");
                    let response = match Self::run_pass(
                        config,
                        manifest.files,
                        project_graph_runtime.clone(),
                        false,
                        false,
                        adaptive_state.clone(),
                    ) {
                        Ok(pass_result) => {
                            let worker_results = pass_result
                                .results
                                .into_iter()
                                .map(Self::to_worker_file_result)
                                .collect::<Vec<_>>();
                            let result_coverage_fingerprint =
                                Self::coverage_fingerprint(
                                    worker_results.as_slice(),
                                );
                            PersistentWorkerResponse::Output(Box::new(WorkerOutput {
                                shard_index: manifest.shard_index,
                                shard_coverage_fingerprint: manifest.shard_coverage_fingerprint,
                                result_coverage_fingerprint,
                                results: worker_results,
                                dispatch_observations: pass_result
                                    .observations
                                    .into_iter()
                                    .map(Self::to_worker_dispatch_observation)
                                    .collect(),
                                adaptive_telemetry: Self::diff_adaptive_snapshot(
                                    &adaptive_baseline,
                                    &AdaptiveTelemetry::snapshot(),
                                ),
                                policy_telemetry: Self::diff_telemetry(
                                    policy_baseline.as_slice(),
                                    PolicyTelemetry::snapshot_sorted().as_slice(),
                                ),
                                policy_cluster_telemetry: Self::diff_policy_cluster_entries(
                                    cluster_baseline.as_slice(),
                                    PolicyClusterTelemetry::snapshot_entries().as_slice(),
                                ),
                                retry_learning: Self::diff_retry_learning_snapshot(
                                    &retry_baseline,
                                    &RetryLearningTelemetry::snapshot(),
                                ),
                            }))
                        }
                        Err(err) => {
                            warn!(batch = batch_index, error = %err, "worker: processing pass failed");
                            PersistentWorkerResponse::Failure(format!("{err:#}"))
                        }
                    };
                    info!(
                        batch = batch_index,
                        elapsed_ms = batch_start.elapsed().as_millis() as u64,
                        "worker: writing response"
                    );
                    Self::write_framed(&mut writer, &response)?;
                }
                PersistentWorkerRequest::Shutdown => break,
            }
        }
        Self::save_adaptive_state_to_ipc(&adaptive_state);
        Ok(())
    }

    pub(crate) fn run_worker_entry(args: &CliArgs, config: &AppConfig) -> Result<()> {
        let manifest_path = args
            .worker_manifest
            .as_ref()
            .ok_or_else(|| anyhow!("worker mode requires --worker-manifest"))?;
        let result_path = args
            .worker_result
            .as_ref()
            .ok_or_else(|| anyhow!("worker mode requires --worker-result"))?;

        let manifest_bytes = fs::read(manifest_path)
            .with_context(|| format!("failed reading worker manifest {}", manifest_path))?;
        let manifest = Self::decode_worker_payload::<WorkerManifest>(&manifest_bytes)
            .with_context(|| format!("failed parsing worker manifest {}", manifest_path))?;

        let project_graph_runtime = Self::open_project_graph_runtime(config)?;
        Self::seed_learning_state_from_project_graph(project_graph_runtime.as_ref());
        let initial_state = Self::load_adaptive_state_from_ipc();
        let adaptive_state = std::sync::Arc::new(arc_swap::ArcSwap::new(std::sync::Arc::new(
            initial_state,
        )));
        let cluster_baseline = PolicyClusterTelemetry::snapshot_entries();
        let retry_baseline = RetryLearningTelemetry::snapshot();
        let pass_result = Self::run_pass(
            config,
            manifest.files,
            project_graph_runtime.clone(),
            false,
            false,
            adaptive_state.clone(),
        )?;
        let worker_results = pass_result
            .results
            .into_iter()
            .map(Self::to_worker_file_result)
            .collect::<Vec<_>>();
        let result_coverage_fingerprint =
            Self::coverage_fingerprint(worker_results.as_slice());
        let cluster_delta = Self::diff_policy_cluster_entries(
            cluster_baseline.as_slice(),
            PolicyClusterTelemetry::snapshot_entries().as_slice(),
        );
        let retry_delta = Self::diff_retry_learning_snapshot(
            &retry_baseline,
            &RetryLearningTelemetry::snapshot(),
        );
        let output = WorkerOutput {
            shard_index: manifest.shard_index,
            shard_coverage_fingerprint: manifest.shard_coverage_fingerprint,
            result_coverage_fingerprint,
            results: worker_results,
            dispatch_observations: pass_result
                .observations
                .into_iter()
                .map(Self::to_worker_dispatch_observation)
                .collect(),
            adaptive_telemetry: AdaptiveTelemetry::snapshot(),
            policy_telemetry: PolicyTelemetry::snapshot_sorted(),
            policy_cluster_telemetry: cluster_delta,
            retry_learning: retry_delta,
        };
        Self::save_adaptive_state_to_ipc(&adaptive_state);
        let output_bytes = Self::encode_worker_payload(&output)
            .context("failed serializing worker result payload")?;
        fs::write(result_path, output_bytes)
            .with_context(|| format!("failed writing worker result {}", result_path))?;
        Ok(())
    }

    pub(crate) fn run_processing_pass(
        config: &AppConfig,
        files: Vec<PathBuf>,
        project_graph_runtime: Option<Arc<ProjectGraphRuntime>>,
        allow_check_result_cache: bool,
        record_dispatch_history: bool,
        adaptive_state: std::sync::Arc<arc_swap::ArcSwap<crate::engine::certainty_filter::CertaintyFilterState>>,
    ) -> Result<Vec<FileResult>> {
        let pass_result = Self::run_pass(
            config,
            files,
            project_graph_runtime,
            allow_check_result_cache,
            record_dispatch_history,
            adaptive_state.clone(),
        )?;
        Ok(pass_result.results)
    }

    fn run_pass(
        config: &AppConfig,
        files: Vec<PathBuf>,
        project_graph_runtime: Option<Arc<ProjectGraphRuntime>>,
        allow_check_result_cache: bool,
        record_dispatch_history: bool,
        adaptive_state: std::sync::Arc<arc_swap::ArcSwap<crate::engine::certainty_filter::CertaintyFilterState>>,
    ) -> Result<ProcessingPassResult> {
        let effective_jobs = Self::resolve_effective_jobs(config.jobs);
        ClangParseService::configure(effective_jobs);
        crate::policy::clang_format_service::ClangFormatService::configure(effective_jobs);
        if let Err(err) = ClangParseService::global() {
            warn!(error = %err, "failed to initialize clang parse service eagerly");
        }
        Self::seed_learning_state_from_project_graph(project_graph_runtime.as_ref());
        let _cluster_read_guard = PolicyClusterTelemetry::begin_read_model();
        let parser_manager = ParserManager::with_full_config(
            config.clang_binary.clone(),
            config.clang_args.clone(),
            config.clang_compdb_path.clone(),
            config.clang_args_mode,
            config.cpp_standard.clone(),
            config.semantic_require_compdb,
            !config.semantic_no_inferred,
        );
        let policies = PolicyRegistry::build_enabled(config);
        let pipeline = PolicyPipeline::new(
            policies,
            parser_manager.clone(),
            config.policy_settings.clone(),
            config.confidence.clone(),
            config.conflict_enabled,
            config.conflict_touch_threshold,
            adaptive_state.clone(),
        );
        let engine = FormatterEngine::new(
            pipeline,
            parser_manager.clone(),
            config.retry.clone(),
            config.accuracy_gate.clone(),
            project_graph_runtime,
        );
        if let Some(tracker) = crate::engine::context_tracker::PolicyContextTracker::load_from_path(&config.tracker_path) {
            if config.verbose {
                tracing::info!("loaded policy context tracker from {}", config.tracker_path.display());
            }
            engine.set_context_tracker(tracker);
        }
        engine.set_observation_only(config.observation_only);
        let engine = Arc::new(engine);
        let file_io = FileIo::new(config);
        let check_result_cache =
            if allow_check_result_cache && config.check && config.cache_enabled {
                Some(Arc::new(CheckResultCache::open(
                    config.cache_path.clone(),
                    true,
                    config.cache_l1_size,
                    Self::cache_fingerprint(config),
                )))
            } else {
                None
            };
        let processor =
            FileProcessor::new(engine.clone(), file_io, config.check, check_result_cache.clone());
        let mut dispatch_history = DispatchHistoryStore::open(config.run_journal_dir.as_path());
        let mut feature_cache =
            DispatchFeatureCache::open(config.run_journal_dir.as_path(), record_dispatch_history);
        let scheduled_batches = Self::build_file_batches(
            files,
            effective_jobs,
            DispatchMode::InProcess,
            &parser_manager,
            &dispatch_history,
            &mut feature_cache,
        );
        if let Err(err) = feature_cache.persist() {
            warn!(error = %err, "failed to persist dispatch feature cache");
        }
        let total_files = scheduled_batches
            .iter()
            .map(|batch| batch.paths.len())
            .sum::<usize>();
        // Snapshot batch metadata before consuming paths for telemetry reconstruction.
        let batch_meta: Vec<(u64, usize)> = scheduled_batches
            .iter()
            .map(|b| (b.estimated_cost, b.paths.len()))
            .collect();
        // Flatten into per-file tasks carrying batch_index. Paths are moved, not cloned.
        let flat_tasks: Vec<_> = scheduled_batches
            .into_iter()
            .enumerate()
            .flat_map(|(batch_index, batch)| {
                batch.paths.into_iter().map(move |path| (batch_index, path))
            })
            .collect();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(effective_jobs.max(1))
            .build()
            .expect("failed to build rayon thread pool");

        let pass_started = Instant::now();
        let processor = Arc::new(processor);
        let file_outcomes: Vec<_> = {
            let processor = processor.clone();
            pool.install(|| {
                flat_tasks
                    .into_par_iter()
                    .map(|(batch_index, path)| (batch_index, processor.process(path)))
                    .collect()
            })
        };
        let pass_elapsed_ns = pass_started
            .elapsed()
            .as_nanos()
            .min(u64::MAX as u128) as u64;

        // Reconstruct per-batch telemetry from per-file outcomes (sequential O(N), cheap).
        let batch_count = batch_meta.len();
        let mut batch_retry_effort = vec![0u64; batch_count];
        let mut batch_error_count = vec![0usize; batch_count];
        let mut file_results = Vec::with_capacity(total_files);
        let mut observations = Vec::with_capacity(total_files);
        for (batch_index, outcome) in file_outcomes {
            batch_retry_effort[batch_index] = batch_retry_effort[batch_index]
                .saturating_add(outcome.observation.retry_effort_units);
            batch_error_count[batch_index] = batch_error_count[batch_index]
                .saturating_add(usize::from(outcome.observation.error));
            file_results.push(outcome.result);
            observations.push(outcome.observation);
        }
        let batch_telemetry = batch_meta
            .iter()
            .enumerate()
            .map(|(batch_index, batch_info)| DispatchBatchTelemetry {
                mode: "in_process",
                worker_slot: None,
                batch_index,
                estimated_cost: batch_info.0,
                file_count: batch_info.1,
                retry_effort_units: batch_retry_effort[batch_index],
                error_count: batch_error_count[batch_index],
                actual_elapsed_ns: pass_elapsed_ns / (batch_count.max(1) as u64),
            })
            .collect::<Vec<_>>();
        let results = ProcessingPassResult {
            results: file_results,
            observations,
            batch_telemetry,
        };
        if let Some(cache) = check_result_cache.as_ref() {
            cache.flush().context(format!(
                "failed persisting check-result cache {}",
                config.cache_path.display()
            ))?;
        }
        if record_dispatch_history {
            if let Err(err) = engine.save_context_tracker(&config.tracker_path) {
                warn!(error = %err, "failed to persist policy context tracker");
            }
        }
        if record_dispatch_history {
            dispatch_history.record_observations(results.observations.as_slice());
            if let Err(err) = dispatch_history.persist() {
                warn!(error = %err, "failed to persist dispatch history");
            }
            if let Err(err) = persist_batch_telemetry(
                config.run_journal_dir.as_path(),
                results.batch_telemetry.as_slice(),
            ) {
                warn!(error = %err, "failed to persist dispatch batch telemetry");
            }
        }
        Ok(results)
    }

    pub(crate) fn run_multiprocess_pass(
        args: &CliArgs,
        config: &AppConfig,
        files: Vec<PathBuf>,
        requested_processes: usize,
        allow_check_result_cache: bool,
        adaptive_state: std::sync::Arc<arc_swap::ArcSwap<crate::engine::certainty_filter::CertaintyFilterState>>,
    ) -> Result<Vec<FileResult>> {
        let total_jobs = Self::resolve_effective_jobs(config.jobs);
        let worker_count =
            Self::resolve_multiprocess_worker_count(requested_processes, total_jobs, files.len());
        if worker_count <= 1 {
            return Self::run_processing_pass(
                config,
                files,
                Self::open_project_graph_runtime(config)?,
                allow_check_result_cache,
                true,
                adaptive_state.clone(),
            );
        }

        let expected_input_files = files.clone();
        let parser_manager = ParserManager::with_full_config(
            config.clang_binary.clone(),
            config.clang_args.clone(),
            config.clang_compdb_path.clone(),
            config.clang_args_mode,
            config.cpp_standard.clone(),
            config.semantic_require_compdb,
            !config.semantic_no_inferred,
        );
        let mut dispatch_history = DispatchHistoryStore::open(config.run_journal_dir.as_path());
        let mut feature_cache = DispatchFeatureCache::open(config.run_journal_dir.as_path(), true);
        let batches = Self::build_file_batches(
            files,
            worker_count,
            DispatchMode::WorkerProcess,
            &parser_manager,
            &dispatch_history,
            &mut feature_cache,
        );
        if let Err(err) = feature_cache.persist() {
            warn!(error = %err, "failed to persist dispatch feature cache");
        }
        let _total_estimated_batch_cost = batches
            .iter()
            .map(|batch| batch.estimated_cost)
            .sum::<u64>();
        if batches.len() <= 1 {
            return Self::run_processing_pass(
                config,
                expected_input_files,
                Self::open_project_graph_runtime(config)?,
                allow_check_result_cache,
                true,
                adaptive_state.clone(),
            );
        }
        let worker_job_plan = Self::distribute_worker_jobs(total_jobs, worker_count);
        let temp_root = Self::worker_temp_root();
        fs::create_dir_all(temp_root.as_path()).with_context(|| {
            format!(
                "failed creating worker temp directory {}",
                temp_root.display()
            )
        })?;
        let _temp_guard = TempDirCleanupGuard::new(temp_root.clone());
        let base_timeout_secs = config.worker_timeout_secs.max(10) as u128;
        let kill_grace = Duration::from_secs(config.worker_kill_secs.max(1));
        let worker_base_timeout = Duration::from_secs(
            base_timeout_secs.min(7200) as u64,
        );

        let current_exe = std::env::current_exe().context("failed to resolve current binary")?;
        let tasks = batches
            .into_iter()
            .enumerate()
            .map(|(index, batch)| WorkerBatchTask {
                index,
                estimated_cost: batch.estimated_cost,
                expected_coverage_fingerprint: Self::coverage_fingerprint(
                    batch.paths.as_slice(),
                ),
                files: batch.paths,
            })
            .collect::<Vec<_>>();
        let median_cost = {
            let mut costs: Vec<u64> = tasks.iter().map(|t| t.estimated_cost).collect();
            costs.sort_unstable();
            costs.get(costs.len() / 2).copied().unwrap_or(1).max(1) as u128
        };
        let pop_ctx_path: Option<PathBuf> = None;
        let next_batch = AtomicUsize::new(0);
        let thread_results = thread::scope(|scope| -> Result<Vec<WorkerControllerOutput>> {
            let mut handles = Vec::with_capacity(worker_job_plan.len());
            for (worker_slot, worker_jobs) in worker_job_plan.iter().copied().enumerate() {
                let current_exe = current_exe.as_path();
                let temp_root = temp_root.as_path();
                let tasks = tasks.as_slice();
                let next_batch = &next_batch;
                let pop_path = pop_ctx_path.as_deref();
                handles.push(scope.spawn(move || -> Result<WorkerControllerOutput> {
                    let mut output = WorkerControllerOutput::default();
                    let adaptive_state_path =
                        temp_root.join(format!("worker_slot_{worker_slot}.adaptive_state.json"));
                    let retry_optimizer_state_path = temp_root
                        .join(format!("worker_slot_{worker_slot}.retry_optimizer_state.json"));
                    let mut worker = Self::spawn_worker(
                        current_exe,
                        args,
                        config,
                        worker_jobs,
                        worker_slot,
                        adaptive_state_path.as_path(),
                        retry_optimizer_state_path.as_path(),
                        pop_path,
                    )?;
                    loop {
                        let batch_index = next_batch.fetch_add(1, Ordering::Relaxed);
                        let Some(task) = tasks.get(batch_index) else {
                            break;
                        };
                        let manifest = WorkerManifest {
                            shard_index: task.index,
                            shard_coverage_fingerprint: task.expected_coverage_fingerprint.clone(),
                            files: task.files.clone(),
                        };
                        let mut recovery_attempt = 0usize;
                        let worker_output = loop {
                            let request =
                                PersistentWorkerRequest::Process(manifest.clone());
                            let lease_started = Instant::now();
                            if let Err(err) = worker.send(&request) {
                                output.worker_failures =
                                    output.worker_failures.saturating_add(1);
                                if recovery_attempt >= config.worker_max_restarts {
                                    return Err(err.context(format!(
                                        "worker batch {} exhausted restart budget after {} restart(s)",
                                        task.index + 1,
                                        recovery_attempt
                                    )));
                                }
                                recovery_attempt = recovery_attempt.saturating_add(1);
                                output.restart_count = output.restart_count.saturating_add(1);
                                warn!(
                                    worker = worker_slot + 1,
                                    batch = task.index + 1,
                                    restart = recovery_attempt,
                                    error = %err,
                                    "multiprocess worker failed before lease dispatch; restarting worker"
                                );
                                Self::terminate_worker(
                                    worker,
                                    worker_base_timeout,
                                    kill_grace,
                                );
                                worker = Self::spawn_worker(
                                    current_exe,
                                    args,
                                    config,
                                    worker_jobs,
                                    worker_slot,
                                    adaptive_state_path.as_path(),
                                    retry_optimizer_state_path.as_path(),
                                    pop_path,
                                )?;
                                continue;
                            }
                            let batch_cost_ratio = (task.estimated_cost as u128)
                                .max(median_cost) / median_cost;
                            let batch_timeout = Duration::from_secs(
                                (base_timeout_secs * batch_cost_ratio)
                                    .min(7200) as u64,
                            );
                            match Self::collect_output(
                                task,
                                &worker,
                                batch_timeout,
                            ) {
                                Ok(result) => {
                                    let retry_effort_units = result
                                        .dispatch_observations
                                        .iter()
                                        .map(|observation| observation.retry_effort_units)
                                        .sum::<u64>();
                                    let error_count = result
                                        .results
                                        .iter()
                                        .filter(|result| result.error.is_some())
                                        .count();
                                    output.batch_telemetry.push(DispatchBatchTelemetry {
                                        mode: "worker_process",
                                        worker_slot: Some(worker_slot),
                                        batch_index: task.index,
                                        estimated_cost: task.estimated_cost,
                                        file_count: task.files.len(),
                                        retry_effort_units,
                                        error_count,
                                        actual_elapsed_ns: lease_started
                                            .elapsed()
                                            .as_nanos()
                                            .min(u64::MAX as u128)
                                            as u64,
                                    });
                                    break result;
                                }
                                Err(err) => {
                                    output.worker_failures =
                                        output.worker_failures.saturating_add(1);
                                    if recovery_attempt >= config.worker_max_restarts {
                                        return Err(err.context(format!(
                                            "worker batch {} exhausted restart budget after {} restart(s)",
                                            task.index + 1,
                                            recovery_attempt
                                        )));
                                    }
                                    recovery_attempt = recovery_attempt.saturating_add(1);
                                    output.restart_count =
                                        output.restart_count.saturating_add(1);
                                    warn!(
                                        worker = worker_slot + 1,
                                        batch = task.index + 1,
                                        restart = recovery_attempt,
                                        error = %err,
                                        "multiprocess worker failed; restarting batch"
                                    );
                                    Self::terminate_worker(
                                        worker,
                                        worker_base_timeout,
                                        kill_grace,
                                    );
                                    worker = Self::spawn_worker(
                                        current_exe,
                                        args,
                                        config,
                                        worker_jobs,
                                        worker_slot,
                                        adaptive_state_path.as_path(),
                                        retry_optimizer_state_path.as_path(),
                                        pop_path,
                                    )?;
                                    continue;
                                }
                            }
                        };

                        AdaptiveTelemetry::merge_snapshot(&worker_output.adaptive_telemetry);
                        PolicyTelemetry::merge_entries(worker_output.policy_telemetry.as_slice());
                        PolicyClusterTelemetry::merge_entries(
                            worker_output.policy_cluster_telemetry.as_slice(),
                        );
                        RetryLearningTelemetry::merge_snapshot(&worker_output.retry_learning);
                        output.dispatch_observations.extend(
                            worker_output
                                .dispatch_observations
                                .into_iter()
                                .map(Self::from_worker_dispatch_observation),
                        );
                        output.results.extend(
                            worker_output
                                .results
                                .into_iter()
                                .map(Self::from_worker_file_result),
                        );
                    }
                    if let Err(err) =
                        Self::shutdown_worker(worker, worker_base_timeout, kill_grace)
                    {
                        output.worker_failures = output.worker_failures.saturating_add(1);
                        warn!(
                            worker = worker_slot + 1,
                            error = %err,
                            "failed to shut down persistent worker cleanly"
                        );
                    }
                    output.adaptive_state_shards.push(adaptive_state_path);
                    output
                        .retry_optimizer_state_shards
                        .push(retry_optimizer_state_path);
                    Ok(output)
                }));
            }

            let mut outputs = Vec::with_capacity(handles.len());
            for handle in handles {
                match handle.join() {
                    Ok(Ok(output)) => outputs.push(output),
                    Ok(Err(err)) => return Err(err),
                    Err(_) => {
                        return Err(anyhow!("multiprocess worker controller thread panicked"));
                    }
                }
            }
            Ok(outputs)
        })?;

        let mut merged = Vec::new();
        let mut merged_observations = Vec::<DispatchObservation>::new();
        let mut merged_batch_telemetry = Vec::<DispatchBatchTelemetry>::new();
        let mut adaptive_state_shards = Vec::<PathBuf>::new();
        let mut retry_optimizer_state_shards = Vec::<PathBuf>::new();
        let mut restart_count = 0usize;
        let mut worker_failures = 0usize;
        for result in thread_results {
            merged.extend(result.results);
            merged_observations.extend(result.dispatch_observations);
            merged_batch_telemetry.extend(result.batch_telemetry);
            adaptive_state_shards.extend(result.adaptive_state_shards);
            retry_optimizer_state_shards.extend(result.retry_optimizer_state_shards);
            restart_count = restart_count.saturating_add(result.restart_count);
            worker_failures = worker_failures.saturating_add(result.worker_failures);
        }

        dispatch_history.record_observations(merged_observations.as_slice());
        if let Err(err) = dispatch_history.persist() {
            warn!(error = %err, "failed to persist dispatch history");
        }
        if let Err(err) = persist_batch_telemetry(
            config.run_journal_dir.as_path(),
            merged_batch_telemetry.as_slice(),
        ) {
            warn!(error = %err, "failed to persist dispatch batch telemetry");
        }

        Self::merge_worker_learning_states(
            config,
            adaptive_state_shards.as_slice(),
            retry_optimizer_state_shards.as_slice(),
        )?;
        if restart_count > 0 || worker_failures > 0 {
            warn!(
                workers = worker_count,
                restarts = restart_count,
                worker_failures,
                worker_jobs = ?worker_job_plan,
                "multiprocess execution completed with worker failures"
            );
        } else if config.verbose {
            info!(
                workers = worker_count,
                worker_jobs = ?worker_job_plan,
                "multiprocess execution completed"
            );
        }

        Self::validate_and_order_multiprocess_results(merged, expected_input_files.as_slice())
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_worker(
        current_exe: &Path,
        args: &CliArgs,
        config: &AppConfig,
        worker_jobs: usize,
        worker_slot: usize,
        adaptive_state_path: &Path,
        retry_optimizer_state_path: &Path,
        population_measurements_path: Option<&Path>,
    ) -> Result<PersistentWorkerProcess> {
        let mut command = Command::new(current_exe);
        Self::append_base_args(&mut command, args, config, worker_jobs);
        let stderr = if config.verbose {
            Stdio::inherit()
        } else {
            Stdio::null()
        };
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .env(
                ADAPTIVE_CONFIDENCE_STATE_PATH_ENV,
                adaptive_state_path.as_os_str(),
            )
            .env(
                RETRY_STRATEGY_OPTIMIZER_STATE_PATH_ENV,
                retry_optimizer_state_path.as_os_str(),
            );
        if let Some(pop_path) = population_measurements_path {
            command.env("FMT_POPULATION_PATH", pop_path.as_os_str());
        }
        if config.observation_only {
            command.env("FMT_OBSERVATION_ONLY", "1");
        }
        command
            .arg("--processes")
            .arg("1")
            .arg("--worker-pool");

        let mut child = command.spawn().with_context(|| {
            format!(
                "failed spawning persistent worker process {}",
                worker_slot + 1
            )
        })?;
        let stdin = io::BufWriter::with_capacity(
            65536,
            child
                .stdin
                .take()
                .ok_or_else(|| anyhow!("persistent worker {} missing stdin", worker_slot + 1))?,
        );
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("persistent worker {} missing stdout", worker_slot + 1))?;
        let (response_tx, responses) = mpsc::channel();
        thread::spawn(move || {
            let mut stdout = io::BufReader::new(stdout);
            loop {
                match App::read_framed::<_, PersistentWorkerResponse>(&mut stdout) {
                    Ok(Some(response)) => {
                        if response_tx.send(Ok(response)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(err) => {
                        let _ = response_tx.send(Err(err));
                        break;
                    }
                }
            }
        });
        Ok(PersistentWorkerProcess {
            child,
            stdin,
            responses,
        })
    }

    fn collect_output(
        task: &WorkerBatchTask,
        worker: &PersistentWorkerProcess,
        worker_timeout: Duration,
    ) -> Result<WorkerOutput> {
        let response = match worker.responses.recv_timeout(worker_timeout) {
            Ok(Ok(response)) => response,
            Ok(Err(err)) => return Err(err),
            Err(RecvTimeoutError::Timeout) => {
                return Err(anyhow!(
                    "worker {} timed out after {}s",
                    task.index + 1,
                    worker_timeout.as_secs()
                ))
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(anyhow!("worker {} response channel closed", task.index + 1))
            }
        };
        let parsed = match response {
            PersistentWorkerResponse::Output(output) => *output,
            PersistentWorkerResponse::Failure(message) => {
                return Err(anyhow!(
                    "worker {} failed processing shard: {}",
                    task.index + 1,
                    message
                ))
            }
        };
        if parsed.shard_index != task.index {
            return Err(anyhow!(
                "worker {} output shard index mismatch: expected {}, got {}",
                task.index + 1,
                task.index,
                parsed.shard_index
            ));
        }
        if parsed.shard_coverage_fingerprint != task.expected_coverage_fingerprint {
            return Err(anyhow!(
                "worker {} shard fingerprint mismatch: expected {}, got {}",
                task.index + 1,
                task.expected_coverage_fingerprint,
                parsed.shard_coverage_fingerprint
            ));
        }
        if parsed.result_coverage_fingerprint != task.expected_coverage_fingerprint {
            return Err(anyhow!(
                "worker {} result fingerprint mismatch: expected {}, got {}",
                task.index + 1,
                task.expected_coverage_fingerprint,
                parsed.result_coverage_fingerprint
            ));
        }
        if parsed.results.len() != task.files.len() {
            return Err(anyhow!(
                "worker {} produced {} result(s), expected {}",
                task.index + 1,
                parsed.results.len(),
                task.files.len()
            ));
        }
        Self::validate_worker_shard_coverage(
            task.index,
            parsed.results.as_slice(),
            task.files.as_slice(),
        )?;
        Ok(parsed)
    }

    fn shutdown_worker(
        mut worker: PersistentWorkerProcess,
        worker_timeout: Duration,
        kill_grace: Duration,
    ) -> Result<()> {
        let _ = worker.send(&PersistentWorkerRequest::Shutdown);
        drop(worker.stdin);
        let output =
            Self::wait_worker_output_with_timeout(worker.child, worker_timeout, kill_grace)
                .context("failed waiting for persistent worker shutdown")?;
        if !output.status.success() {
            let stderr = Self::worker_log_excerpt(output.stderr.as_slice(), 48);
            return Err(anyhow!(
                "persistent worker exited with status {}; stderr: {}",
                output.status,
                stderr
            ));
        }
        Ok(())
    }

    fn terminate_worker(
        worker: PersistentWorkerProcess,
        _worker_timeout: Duration,
        kill_grace: Duration,
    ) {
        let _ =
            Self::shutdown_worker(worker, Duration::from_millis(1), kill_grace);
    }

    fn load_adaptive_state_from_ipc() -> crate::engine::certainty_filter::CertaintyFilterState {
        if let Ok(path) = std::env::var(ADAPTIVE_CONFIDENCE_STATE_PATH_ENV) {
            let p = std::path::PathBuf::from(&path);
            if let Some(state) = crate::engine::certainty_filter::CertaintyFilterState::load_from_path(&p) {
                return state;
            }
        }
        crate::engine::certainty_filter::CertaintyFilterState::new()
    }

    fn save_adaptive_state_to_ipc(
        adaptive_state: &std::sync::Arc<arc_swap::ArcSwap<crate::engine::certainty_filter::CertaintyFilterState>>,
    ) {
        if let Ok(path) = std::env::var(ADAPTIVE_CONFIDENCE_STATE_PATH_ENV) {
            let p = std::path::PathBuf::from(&path);
            let state = adaptive_state.load();
            if let Err(err) = state.save_to_path(&p) {
                warn!(error = %err, "worker: failed to save adaptive state shard");
            }
        }
    }

    fn diff_adaptive_snapshot(
        previous: &AdaptiveSnapshot,
        current: &AdaptiveSnapshot,
    ) -> AdaptiveSnapshot {
        AdaptiveSnapshot {
            threshold_evaluations: current
                .threshold_evaluations
                .saturating_sub(previous.threshold_evaluations),
            threshold_applied: current
                .threshold_applied
                .saturating_sub(previous.threshold_applied),
            threshold_canary: current
                .threshold_canary
                .saturating_sub(previous.threshold_canary),
            threshold_suspended: current
                .threshold_suspended
                .saturating_sub(previous.threshold_suspended),
            confidence_decisions: current
                .confidence_decisions
                .saturating_sub(previous.confidence_decisions),
            confidence_apply: current
                .confidence_apply
                .saturating_sub(previous.confidence_apply),
            confidence_apply_partial: current
                .confidence_apply_partial
                .saturating_sub(previous.confidence_apply_partial),
            confidence_advisory_only: current
                .confidence_advisory_only
                .saturating_sub(previous.confidence_advisory_only),
            confidence_block: current
                .confidence_block
                .saturating_sub(previous.confidence_block),
            reason_low_consensus: current
                .reason_low_consensus
                .saturating_sub(previous.reason_low_consensus),
            reason_parser_strict: current
                .reason_parser_strict
                .saturating_sub(previous.reason_parser_strict),
            reason_parser_hardened: current
                .reason_parser_hardened
                .saturating_sub(previous.reason_parser_hardened),
            reason_parser_relaxed: current
                .reason_parser_relaxed
                .saturating_sub(previous.reason_parser_relaxed),
            reason_coverage_low: current
                .reason_coverage_low
                .saturating_sub(previous.reason_coverage_low),
            reason_semantic_low: current
                .reason_semantic_low
                .saturating_sub(previous.reason_semantic_low),
            reason_parser_disagreement: current
                .reason_parser_disagreement
                .saturating_sub(previous.reason_parser_disagreement),
            reason_clang_diagnostics: current
                .reason_clang_diagnostics
                .saturating_sub(previous.reason_clang_diagnostics),
            outcomes_first_pass: current
                .outcomes_first_pass
                .saturating_sub(previous.outcomes_first_pass),
            outcomes_after_retry: current
                .outcomes_after_retry
                .saturating_sub(previous.outcomes_after_retry),
            outcomes_reverted: current
                .outcomes_reverted
                .saturating_sub(previous.outcomes_reverted),
            rollback_events: current
                .rollback_events
                .saturating_sub(previous.rollback_events),
            last_threshold: current.last_threshold,
            last_delta: current.last_delta,
            last_ema_failure_rate: current.last_ema_failure_rate,
            last_ema_revert_rate: current.last_ema_revert_rate,
            last_drift: current.last_drift,
            max_abs_drift: if current.max_abs_drift > previous.max_abs_drift {
                current.max_abs_drift
            } else {
                0.0
            },
        }
    }

    fn diff_telemetry(
        previous: &[PolicyTelemetrySnapshotEntry],
        current: &[PolicyTelemetrySnapshotEntry],
    ) -> Vec<PolicyTelemetrySnapshotEntry> {
        let previous_by_policy = previous
            .iter()
            .map(|item| (item.policy.clone(), item.entry.clone()))
            .collect::<FxHashMap<_, _>>();
        let mut delta = Vec::new();
        for item in current {
            let baseline = previous_by_policy.get(&item.policy);
            let entry = PolicyTelemetryEntry {
                runs: item
                    .entry
                    .runs
                    .saturating_sub(baseline.map_or(0, |value| value.runs)),
                failures: item
                    .entry
                    .failures
                    .saturating_sub(baseline.map_or(0, |value| value.failures)),
                fatals: item
                    .entry
                    .fatals
                    .saturating_sub(baseline.map_or(0, |value| value.fatals)),
                blocked: item
                    .entry
                    .blocked
                    .saturating_sub(baseline.map_or(0, |value| value.blocked)),
                confidence_decisions: item
                    .entry
                    .confidence_decisions
                    .saturating_sub(baseline.map_or(0, |value| value.confidence_decisions)),
                confidence_apply: item
                    .entry
                    .confidence_apply
                    .saturating_sub(baseline.map_or(0, |value| value.confidence_apply)),
                confidence_apply_partial: item
                    .entry
                    .confidence_apply_partial
                    .saturating_sub(baseline.map_or(0, |value| value.confidence_apply_partial)),
                confidence_advisory_only: item
                    .entry
                    .confidence_advisory_only
                    .saturating_sub(baseline.map_or(0, |value| value.confidence_advisory_only)),
                confidence_block: item
                    .entry
                    .confidence_block
                    .saturating_sub(baseline.map_or(0, |value| value.confidence_block)),
                reason_low_consensus: item
                    .entry
                    .reason_low_consensus
                    .saturating_sub(baseline.map_or(0, |value| value.reason_low_consensus)),
                reason_parser_strict: item
                    .entry
                    .reason_parser_strict
                    .saturating_sub(
                        baseline.map_or(0, |value| value.reason_parser_strict),
                    ),
                reason_parser_hardened: item
                    .entry
                    .reason_parser_hardened
                    .saturating_sub(
                        baseline.map_or(0, |value| value.reason_parser_hardened),
                    ),
                reason_parser_relaxed: item
                    .entry
                    .reason_parser_relaxed
                    .saturating_sub(
                        baseline.map_or(0, |value| value.reason_parser_relaxed),
                    ),
                reason_coverage_low: item
                    .entry
                    .reason_coverage_low
                    .saturating_sub(baseline.map_or(0, |value| value.reason_coverage_low)),
                reason_semantic_low: item
                    .entry
                    .reason_semantic_low
                    .saturating_sub(
                        baseline.map_or(0, |value| value.reason_semantic_low),
                    ),
                reason_parser_disagreement: item
                    .entry
                    .reason_parser_disagreement
                    .saturating_sub(baseline.map_or(0, |value| value.reason_parser_disagreement)),
                reason_clang_diagnostics: item
                    .entry
                    .reason_clang_diagnostics
                    .saturating_sub(baseline.map_or(0, |value| value.reason_clang_diagnostics)),
                total_elapsed_ns: item
                    .entry
                    .total_elapsed_ns
                    .saturating_sub(baseline.map_or(0, |value| value.total_elapsed_ns)),
                max_elapsed_ns: if item.entry.max_elapsed_ns
                    > baseline.map_or(0, |value| value.max_elapsed_ns)
                {
                    item.entry.max_elapsed_ns
                } else {
                    0
                },
                total_edits: item
                    .entry
                    .total_edits
                    .saturating_sub(baseline.map_or(0, |value| value.total_edits)),
                total_violations: item
                    .entry
                    .total_violations
                    .saturating_sub(baseline.map_or(0, |value| value.total_violations)),
            };
            if entry.runs == 0
                && entry.failures == 0
                && entry.fatals == 0
                && entry.blocked == 0
                && entry.confidence_decisions == 0
                && entry.total_elapsed_ns == 0
                && entry.max_elapsed_ns == 0
                && entry.total_edits == 0
                && entry.total_violations == 0
            {
                continue;
            }
            delta.push(PolicyTelemetrySnapshotEntry {
                policy: item.policy.clone(),
                entry,
            });
        }
        delta
    }
}

impl PersistentWorkerProcess {
    fn send(&mut self, request: &PersistentWorkerRequest) -> Result<()> {
        App::write_framed(&mut self.stdin, request)
    }
}
