use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::files::atomic_writer::AtomicWriter;
use crate::files::file_unit::{FileUnitKind, FileUnitLayout};
use crate::model::file_result::FileResult;
use crate::parser::manager::{ParserManager, SemanticCompdbContextKind};

const DISPATCH_HISTORY_VERSION: u8 = 1;
const DISPATCH_HISTORY_FILE_NAME: &str = "dispatch_cost_cache.bin";
const DISPATCH_FEATURE_CACHE_VERSION: u8 = 1;
const DISPATCH_FEATURE_CACHE_FILE_NAME: &str = "dispatch_feature_cache.bin";
const DISPATCH_BATCH_TELEMETRY_FILE_NAME: &str = "dispatch_batches.ndjson";
const FEATURE_SAMPLE_BYTES: usize = 64 * 1024;
const STATIC_COST_BASE: u64 = 200_000;
const STATIC_COST_MAX_BYTES: u64 = 4 * 1024 * 1024;
const RETRY_PENALTY_NS_PER_UNIT: u64 = 5_000_000;
const HISTORY_MIN_SAMPLES: u32 = 3;
const HISTORY_BLEND_STATIC_WEIGHT: u128 = 30;
const HISTORY_BLEND_HISTORY_WEIGHT: u128 = 70;
const HISTORY_BLEND_DENOMINATOR: u128 = 100;
const HISTORY_LOWER_BOUND_NUMERATOR: u128 = 1;
const HISTORY_LOWER_BOUND_DENOMINATOR: u128 = 2;
const HISTORY_UPPER_BOUND_NUMERATOR: u128 = 3;
const HISTORY_UPPER_BOUND_DENOMINATOR: u128 = 1;
const BATCH_MAX_UNITS: usize = 8;
const BATCH_MAX_PATHS: usize = 16;
const LOW_SKEW_THRESHOLD_BP: u128 = 25_000;
const MEDIUM_SKEW_THRESHOLD_BP: u128 = 60_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DispatchMode {
    InProcess,
    WorkerProcess,
}

#[derive(Clone, Debug)]
pub(crate) struct DispatchBatch {
    pub(crate) estimated_cost: u64,
    pub(crate) paths: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
pub(crate) struct ProcessedFileOutcome {
    pub(crate) result: FileResult,
    pub(crate) observation: DispatchObservation,
}

#[derive(Clone, Debug)]
pub(crate) struct DispatchObservation {
    pub(crate) path_identity: String,
    pub(crate) observed_size: u64,
    pub(crate) observed_lines: u64,
    pub(crate) elapsed_wall_ns: u64,
    pub(crate) total_wall_ns: u64,
    pub(crate) retry_effort_units: u64,
    pub(crate) retry_penalty_ns: u64,
    pub(crate) error: bool,
}

impl DispatchObservation {
    pub(crate) fn from_processed_file(
        path: &Path,
        original_text: Option<&str>,
        engine_elapsed: Duration,
        total_elapsed: Duration,
        retry_effort_units: u64,
        result: &FileResult,
    ) -> Self {
        let (observed_size, observed_lines) = if let Some(text) = original_text {
            (text.len() as u64, text.lines().count().max(1) as u64)
        } else {
            let observed_size = fs::metadata(path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            let observed_lines = if observed_size == 0 { 0 } else { 1 };
            (observed_size, observed_lines)
        };
        Self {
            path_identity: normalize_path_identity(path),
            observed_size,
            observed_lines,
            elapsed_wall_ns: engine_elapsed.as_nanos().min(u64::MAX as u128) as u64,
            total_wall_ns: total_elapsed.as_nanos().min(u64::MAX as u128) as u64,
            retry_effort_units,
            retry_penalty_ns: retry_effort_units.saturating_mul(RETRY_PENALTY_NS_PER_UNIT),
            error: result.error.is_some(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct DispatchBatchTelemetry {
    pub(crate) mode: &'static str,
    pub(crate) worker_slot: Option<usize>,
    pub(crate) batch_index: usize,
    pub(crate) estimated_cost: u64,
    pub(crate) file_count: usize,
    pub(crate) retry_effort_units: u64,
    pub(crate) error_count: usize,
    pub(crate) actual_elapsed_ns: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct DispatchHistoryEntry {
    pub(crate) samples: u32,
    pub(crate) ewma_elapsed_ns: u64,
    pub(crate) ewma_retry_penalty_ns: u64,
    pub(crate) last_observed_size: u64,
    pub(crate) last_observed_lines: u64,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct DispatchHistoryStore {
    path: PathBuf,
    entries: HashMap<String, DispatchHistoryEntry>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct DispatchHistorySnapshot {
    version: u8,
    entries: BTreeMap<String, DispatchHistoryEntry>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct DispatchFeatureCacheEntry {
    byte_len: u64,
    modified_unix_nanos: u64,
    sample_len: usize,
    estimated_line_count: u64,
    preprocessor_density_bp: u64,
    comment_density_bp: u64,
    extension_class: ExtensionClass,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct DispatchFeatureCacheSnapshot {
    version: u8,
    entries: BTreeMap<String, DispatchFeatureCacheEntry>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum DispatchUnitKind {
    Paired,
    HeaderOnly,
    ImplementationOnly,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
enum ExtensionClass {
    HeaderLike,
    #[default]
    ImplementationLike,
    Other,
}

#[derive(Clone, Debug)]
pub(crate) struct DispatchFeatures {
    pub(crate) byte_len: u64,
    pub(crate) sample_len: usize,
    pub(crate) estimated_line_count: u64,
    pub(crate) preprocessor_density_bp: u64,
    pub(crate) comment_density_bp: u64,
    extension_class: ExtensionClass,
    pub(crate) semantic_context_kind: SemanticCompdbContextKind,
    pub(crate) unit_kind: DispatchUnitKind,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DispatchEstimate {
    pub(crate) static_cost: u64,
    pub(crate) estimated_cost: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct DispatchUnit {
    pub(crate) group_key: String,
    pub(crate) kind: DispatchUnitKind,
    pub(crate) estimate: DispatchEstimate,
    pub(crate) paths: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
struct DispatchPathEstimate {
    path: PathBuf,
    estimate: DispatchEstimate,
}

#[derive(Clone, Debug, Default)]
struct VirtualLane {
    estimated_cost: u128,
    units: Vec<DispatchUnit>,
}

#[derive(Clone, Copy, Debug)]
struct WorkloadLeaseSizing {
    lane_count: usize,
    target_queue_depth: usize,
    target_batch_cost: u128,
    batch_max_units: usize,
    batch_max_paths: usize,
    singleton_threshold: u128,
}

pub(crate) struct DispatchScheduler;

#[derive(Clone, Debug, Default)]
pub(crate) struct DispatchFeatureCache {
    path: PathBuf,
    writable: bool,
    dirty: bool,
    entries: HashMap<String, DispatchFeatureCacheEntry>,
}

impl DispatchFeatureCache {
    pub(crate) fn open(run_journal_dir: &Path, writable: bool) -> Self {
        let path = run_journal_dir.join(DISPATCH_FEATURE_CACHE_FILE_NAME);
        let bytes = match fs::read(path.as_path()) {
            Ok(bytes) => bytes,
            Err(_) => {
                return Self {
                    path,
                    writable,
                    dirty: false,
                    entries: HashMap::new(),
                };
            }
        };
        let snapshot = match bincode::serde::decode_from_slice::<DispatchFeatureCacheSnapshot, _>(
            bytes.as_slice(),
            bincode::config::standard(),
        ) {
            Ok((snapshot, consumed)) if consumed == bytes.len() => snapshot,
            _ => {
                return Self {
                    path,
                    writable,
                    dirty: false,
                    entries: HashMap::new(),
                };
            }
        };
        if snapshot.version != DISPATCH_FEATURE_CACHE_VERSION {
            return Self {
                path,
                writable,
                dirty: false,
                entries: HashMap::new(),
            };
        }
        Self {
            path,
            writable,
            dirty: false,
            entries: snapshot.entries.into_iter().collect(),
        }
    }

    #[cfg(test)]
    pub(crate) fn ephemeral() -> Self {
        Self {
            path: PathBuf::new(),
            writable: false,
            dirty: false,
            entries: HashMap::new(),
        }
    }

    pub(crate) fn persist(&self) -> Result<()> {
        if !self.writable || !self.dirty || self.path.as_os_str().is_empty() {
            return Ok(());
        }
        let mut entries = self
            .entries
            .iter()
            .map(|(path, entry)| (path.clone(), entry.clone()))
            .collect::<Vec<_>>();
        entries.sort_by(|(left, _), (right, _)| left.cmp(right));
        let snapshot = DispatchFeatureCacheSnapshot {
            version: DISPATCH_FEATURE_CACHE_VERSION,
            entries: entries.into_iter().collect(),
        };
        let bytes = bincode::serde::encode_to_vec(&snapshot, bincode::config::standard())
            .context("failed serializing dispatch feature cache")?;
        AtomicWriter::write_bytes(self.path.as_path(), bytes.as_slice()).with_context(|| {
            format!(
                "failed writing dispatch feature cache {}",
                self.path.display()
            )
        })
    }

    fn load_sampled_features(&mut self, path: &Path) -> DispatchFeatureCacheEntry {
        let path_identity = normalize_path_identity(path);
        let metadata = fs::metadata(path).ok();
        let byte_len = metadata.as_ref().map(|item| item.len().max(1)).unwrap_or(1);
        let modified_unix_nanos = metadata
            .as_ref()
            .and_then(|item| item.modified().ok())
            .and_then(|item| item.duration_since(UNIX_EPOCH).ok())
            .map(|item| item.as_nanos().min(u64::MAX as u128) as u64)
            .unwrap_or(0);
        let extension_class = ExtensionClass::for_path(path);
        if let Some(entry) = self.entries.get(path_identity.as_str()) {
            if entry.byte_len == byte_len
                && entry.modified_unix_nanos == modified_unix_nanos
            {
                return entry.clone();
            }
        }
        let sample = read_path_sample(path, FEATURE_SAMPLE_BYTES);
        let analyzed = analyze_sample(sample.as_slice(), byte_len);
        let entry = DispatchFeatureCacheEntry {
            byte_len,
            modified_unix_nanos,
            sample_len: sample.len(),
            estimated_line_count: analyzed.estimated_line_count,
            preprocessor_density_bp: analyzed.preprocessor_density_bp,
            comment_density_bp: analyzed.comment_density_bp,
            extension_class,
        };
        if self.writable {
            self.entries.insert(path_identity, entry.clone());
            self.dirty = true;
        }
        entry
    }
}

impl DispatchHistoryStore {
    pub(crate) fn open(run_journal_dir: &Path) -> Self {
        let path = run_journal_dir.join(DISPATCH_HISTORY_FILE_NAME);
        let bytes = match fs::read(path.as_path()) {
            Ok(bytes) => bytes,
            Err(_) => {
                return Self {
                    path,
                    entries: HashMap::new(),
                };
            }
        };
        let snapshot = match bincode::serde::decode_from_slice::<DispatchHistorySnapshot, _>(
            bytes.as_slice(),
            bincode::config::standard(),
        ) {
            Ok((snapshot, consumed)) if consumed == bytes.len() => snapshot,
            _ => {
                return Self {
                    path,
                    entries: HashMap::new(),
                };
            }
        };
        if snapshot.version != DISPATCH_HISTORY_VERSION {
            return Self {
                path,
                entries: HashMap::new(),
            };
        }
        Self {
            path,
            entries: snapshot.entries.into_iter().collect(),
        }
    }

    #[cfg(test)]
    pub(crate) fn ephemeral() -> Self {
        Self {
            path: PathBuf::new(),
            entries: HashMap::new(),
        }
    }

    pub(crate) fn blended_cost(&self, path_identity: &str, static_cost: u64) -> u64 {
        let Some(entry) = self.entries.get(path_identity) else {
            return static_cost.max(1);
        };
        if entry.samples < HISTORY_MIN_SAMPLES {
            return static_cost.max(1);
        }
        let historical = entry
            .ewma_elapsed_ns
            .saturating_add(entry.ewma_retry_penalty_ns)
            .max(1);
        let blended = ((historical as u128).saturating_mul(HISTORY_BLEND_HISTORY_WEIGHT))
            .saturating_add((static_cost as u128).saturating_mul(HISTORY_BLEND_STATIC_WEIGHT))
            / HISTORY_BLEND_DENOMINATOR;
        let lower = ((static_cost as u128).saturating_mul(HISTORY_LOWER_BOUND_NUMERATOR)
            / HISTORY_LOWER_BOUND_DENOMINATOR)
            .max(1);
        let upper = ((static_cost as u128).saturating_mul(HISTORY_UPPER_BOUND_NUMERATOR)
            / HISTORY_UPPER_BOUND_DENOMINATOR)
            .max(lower);
        blended.clamp(lower, upper).min(u64::MAX as u128) as u64
    }

    pub(crate) fn record_observations(&mut self, observations: &[DispatchObservation]) {
        for observation in observations {
            if observation.error {
                continue;
            }
            let entry = self
                .entries
                .entry(observation.path_identity.clone())
                .or_default();
            entry.samples = entry.samples.saturating_add(1);
            entry.ewma_elapsed_ns = ewma(entry.ewma_elapsed_ns, observation.elapsed_wall_ns);
            entry.ewma_retry_penalty_ns =
                ewma(entry.ewma_retry_penalty_ns, observation.retry_penalty_ns);
            entry.last_observed_size = observation.observed_size;
            entry.last_observed_lines = observation.observed_lines;
        }
    }

    pub(crate) fn persist(&self) -> Result<()> {
        if self.path.as_os_str().is_empty() {
            return Ok(());
        }
        let mut entries = self
            .entries
            .iter()
            .map(|(path, entry)| (path.clone(), entry.clone()))
            .collect::<Vec<_>>();
        entries.sort_by(|(left, _), (right, _)| left.cmp(right));
        let snapshot = DispatchHistorySnapshot {
            version: DISPATCH_HISTORY_VERSION,
            entries: entries.into_iter().collect(),
        };
        let bytes = bincode::serde::encode_to_vec(&snapshot, bincode::config::standard())
            .context("failed serializing dispatch history")?;
        AtomicWriter::write_bytes(self.path.as_path(), bytes.as_slice())
            .with_context(|| format!("failed writing dispatch history {}", self.path.display()))
    }
}

pub(crate) fn persist_batch_telemetry(
    run_journal_dir: &Path,
    batches: &[DispatchBatchTelemetry],
) -> Result<()> {
    if run_journal_dir.as_os_str().is_empty() {
        return Ok(());
    }
    let path = run_journal_dir.join(DISPATCH_BATCH_TELEMETRY_FILE_NAME);
    let mut payload = Vec::<u8>::new();
    for batch in batches {
        let line =
            serde_json::to_string(batch).context("failed serializing dispatch batch telemetry")?;
        payload.extend_from_slice(line.as_bytes());
        payload.push(b'\n');
    }
    AtomicWriter::write_bytes(path.as_path(), payload.as_slice())
        .with_context(|| format!("failed writing dispatch batch telemetry {}", path.display()))
}

impl DispatchScheduler {
    pub(crate) fn plan_batches(
        files: Vec<PathBuf>,
        parallelism: usize,
        mode: DispatchMode,
        parser_manager: &ParserManager,
        history: &DispatchHistoryStore,
        feature_cache: &mut DispatchFeatureCache,
    ) -> Vec<DispatchBatch> {
        if files.is_empty() || parallelism == 0 {
            return Vec::new();
        }
        let ranked_units = Self::build_ranked_units(files, parser_manager, history, feature_cache);
        if ranked_units.is_empty() {
            return Vec::new();
        }
        let total_estimated_cost = ranked_units
            .iter()
            .map(|unit| unit.estimate.estimated_cost as u128)
            .sum::<u128>();
        let sizing = Self::workload_lease_sizing(ranked_units.as_slice(), parallelism, mode);
        let lanes = Self::assign_units_to_virtual_lanes(ranked_units, sizing.lane_count);
        let lane_batches =
            Self::split_virtual_lanes_into_batches(lanes, sizing, total_estimated_cost);
        Self::interleave_lane_batches(lane_batches)
    }

    fn build_ranked_units(
        files: Vec<PathBuf>,
        parser_manager: &ParserManager,
        history: &DispatchHistoryStore,
        feature_cache: &mut DispatchFeatureCache,
    ) -> Vec<DispatchUnit> {
        let layout = FileUnitLayout::from_paths(files.as_slice());
        let mut grouped = HashMap::<String, Vec<PathBuf>>::new();
        for path in files {
            let group_key = layout.group_key_for_path(path.as_path());
            grouped.entry(group_key).or_default().push(path);
        }

        let mut units = grouped
            .into_iter()
            .map(|(group_key, mut paths)| {
                paths.sort();
                let kind = paths
                    .first()
                    .map(|path| DispatchUnitKind::from(layout.kind_for_path(path.as_path())))
                    .unwrap_or(DispatchUnitKind::ImplementationOnly);
                let path_estimates = paths
                    .iter()
                    .map(|path| {
                        let features = DispatchFeatures::from_path(
                            path.as_path(),
                            kind,
                            parser_manager,
                            feature_cache,
                        );
                        let static_cost = features.static_cost();
                        let estimated_cost = history.blended_cost(
                            normalize_path_identity(path.as_path()).as_str(),
                            static_cost,
                        );
                        DispatchPathEstimate {
                            path: path.clone(),
                            estimate: DispatchEstimate {
                                static_cost,
                                estimated_cost,
                            },
                        }
                    })
                    .collect::<Vec<_>>();
                let estimate = DispatchEstimate {
                    static_cost: path_estimates
                        .iter()
                        .map(|item| item.estimate.static_cost)
                        .sum::<u64>(),
                    estimated_cost: path_estimates
                        .iter()
                        .map(|item| item.estimate.estimated_cost)
                        .sum::<u64>(),
                };
                DispatchUnit {
                    group_key,
                    kind,
                    estimate,
                    paths: path_estimates.into_iter().map(|item| item.path).collect(),
                }
            })
            .collect::<Vec<_>>();
        units.sort_by(|left, right| {
            right
                .estimate
                .estimated_cost
                .cmp(&left.estimate.estimated_cost)
                .then_with(|| {
                    left.kind
                        .dispatch_priority()
                        .cmp(&right.kind.dispatch_priority())
                })
                .then_with(|| left.group_key.cmp(&right.group_key))
        });
        units
    }

    fn workload_lease_sizing(
        ranked_units: &[DispatchUnit],
        parallelism: usize,
        mode: DispatchMode,
    ) -> WorkloadLeaseSizing {
        let mut estimates = ranked_units
            .iter()
            .map(|unit| unit.estimate.estimated_cost.max(1))
            .collect::<Vec<_>>();
        estimates.sort_unstable();
        let median = estimates
            .get(estimates.len() / 2)
            .copied()
            .unwrap_or(1)
            .max(1) as u128;
        let max_estimate = estimates.last().copied().unwrap_or(1).max(1) as u128;
        let skew_ratio_bp = max_estimate.saturating_mul(10_000).saturating_div(median);
        let target_queue_depth = if skew_ratio_bp <= LOW_SKEW_THRESHOLD_BP {
            parallelism.saturating_mul(4)
        } else if skew_ratio_bp <= MEDIUM_SKEW_THRESHOLD_BP {
            parallelism.saturating_mul(8)
        } else {
            parallelism.saturating_mul(12)
        }
        .max(parallelism)
        .max(ranked_units.len())
        .min(ranked_units.len().max(1));
        let lane_count = target_queue_depth
            .saturating_add(1)
            .saturating_div(2)
            .max(parallelism)
            .min(ranked_units.len().max(1));
        let total_estimated_cost = ranked_units
            .iter()
            .map(|unit| unit.estimate.estimated_cost as u128)
            .sum::<u128>()
            .max(1);
        let target_batch_cost = (total_estimated_cost / target_queue_depth.max(1) as u128).max(1);
        let (batch_max_units, batch_max_paths) = match mode {
            DispatchMode::InProcess => (BATCH_MAX_UNITS, BATCH_MAX_PATHS),
            DispatchMode::WorkerProcess => (1, 2),
        };
        WorkloadLeaseSizing {
            lane_count,
            target_queue_depth,
            target_batch_cost,
            batch_max_units,
            batch_max_paths,
            singleton_threshold: target_batch_cost.saturating_mul(3) / 2,
        }
    }

    fn assign_units_to_virtual_lanes(
        ranked_units: Vec<DispatchUnit>,
        lane_count: usize,
    ) -> Vec<VirtualLane> {
        let mut lanes = vec![VirtualLane::default(); lane_count];
        for unit in ranked_units {
            let mut target_index = 0usize;
            for index in 1..lanes.len() {
                if lanes[index].estimated_cost < lanes[target_index].estimated_cost {
                    target_index = index;
                }
            }
            lanes[target_index].estimated_cost = lanes[target_index]
                .estimated_cost
                .saturating_add(unit.estimate.estimated_cost as u128);
            lanes[target_index].units.push(unit);
        }
        lanes
    }

    fn split_virtual_lanes_into_batches(
        lanes: Vec<VirtualLane>,
        sizing: WorkloadLeaseSizing,
        _total_estimated_cost: u128,
    ) -> Vec<Vec<DispatchBatch>> {
        if lanes.is_empty() {
            return Vec::new();
        }
        let _target_queue_depth = sizing.target_queue_depth;

        lanes
            .into_iter()
            .map(|lane| {
                let mut batches = Vec::<DispatchBatch>::new();
                let mut current_paths = Vec::<PathBuf>::new();
                let mut current_cost = 0u128;
                let mut current_units = 0usize;
                let mut current_path_count = 0usize;

                let flush_current =
                    |batches: &mut Vec<DispatchBatch>,
                     current_paths: &mut Vec<PathBuf>,
                     current_cost: &mut u128,
                     current_units: &mut usize,
                     current_path_count: &mut usize| {
                        if current_paths.is_empty() {
                            return;
                        }
                        batches.push(DispatchBatch {
                            estimated_cost: (*current_cost).min(u64::MAX as u128) as u64,
                            paths: std::mem::take(current_paths),
                        });
                        *current_cost = 0;
                        *current_units = 0;
                        *current_path_count = 0;
                    };

                for unit in lane.units {
                    let unit_cost = unit.estimate.estimated_cost as u128;
                    let unit_path_count = unit.paths.len();
                    let is_elephant = unit_cost > sizing.singleton_threshold;
                    let would_exceed_cost = !current_paths.is_empty()
                        && current_cost.saturating_add(unit_cost) > sizing.target_batch_cost;
                    let would_exceed_units = current_units >= sizing.batch_max_units;
                    let would_exceed_paths = !current_paths.is_empty()
                        && current_path_count.saturating_add(unit_path_count)
                            > sizing.batch_max_paths;
                    if would_exceed_cost || would_exceed_units || would_exceed_paths {
                        flush_current(
                            &mut batches,
                            &mut current_paths,
                            &mut current_cost,
                            &mut current_units,
                            &mut current_path_count,
                        );
                    }
                    if is_elephant {
                        flush_current(
                            &mut batches,
                            &mut current_paths,
                            &mut current_cost,
                            &mut current_units,
                            &mut current_path_count,
                        );
                        batches.push(DispatchBatch {
                            estimated_cost: unit_cost.min(u64::MAX as u128) as u64,
                            paths: unit.paths,
                        });
                        continue;
                    }

                    current_cost = current_cost.saturating_add(unit_cost);
                    current_units = current_units.saturating_add(1);
                    current_path_count = current_path_count.saturating_add(unit_path_count);
                    current_paths.extend(unit.paths);
                }

                flush_current(
                    &mut batches,
                    &mut current_paths,
                    &mut current_cost,
                    &mut current_units,
                    &mut current_path_count,
                );
                batches
            })
            .collect()
    }

    fn interleave_lane_batches(lane_batches: Vec<Vec<DispatchBatch>>) -> Vec<DispatchBatch> {
        let lane_count = lane_batches.len();
        let max_batches = lane_batches.iter().map(Vec::len).max().unwrap_or(0);
        let mut interleaved = Vec::<DispatchBatch>::new();
        for batch_index in 0..max_batches {
            for lane_index in 0..lane_count {
                if let Some(batch) = lane_batches
                    .get(lane_index)
                    .and_then(|lane| lane.get(batch_index))
                {
                    interleaved.push(batch.clone());
                }
            }
        }
        interleaved
    }
}

impl DispatchFeatures {
    fn from_path(
        path: &Path,
        unit_kind: DispatchUnitKind,
        parser_manager: &ParserManager,
        feature_cache: &mut DispatchFeatureCache,
    ) -> Self {
        let sampled = feature_cache.load_sampled_features(path);
        Self {
            byte_len: sampled.byte_len.max(1),
            sample_len: sampled.sample_len,
            estimated_line_count: sampled.estimated_line_count,
            preprocessor_density_bp: sampled.preprocessor_density_bp,
            comment_density_bp: sampled.comment_density_bp,
            extension_class: sampled.extension_class,
            semantic_context_kind: parser_manager.semantic_compdb_context_kind_for_path(path),
            unit_kind,
        }
    }

    fn static_cost(&self) -> u64 {
        debug_assert!(self.sample_len <= FEATURE_SAMPLE_BYTES);
        let byte_term = self.byte_len.min(STATIC_COST_MAX_BYTES).saturating_mul(18);
        let line_term = self.estimated_line_count.saturating_mul(2_400);
        let macro_term = self
            .preprocessor_density_bp
            .saturating_mul(line_term)
            .saturating_div(10_000)
            .saturating_div(2);
        let comment_term = self
            .comment_density_bp
            .saturating_mul(line_term)
            .saturating_div(10_000)
            .saturating_div(6);
        let base_cost = STATIC_COST_BASE
            .saturating_add(byte_term)
            .saturating_add(line_term)
            .saturating_add(macro_term)
            .saturating_add(comment_term) as u128;
        let cost = base_cost
            .saturating_mul(self.unit_kind.multiplier_bp() as u128)
            .saturating_mul(self.extension_class.multiplier_bp() as u128)
            .saturating_mul(semantic_context_multiplier_bp(self.semantic_context_kind) as u128)
            .saturating_div(10_000)
            .saturating_div(10_000)
            .saturating_div(10_000);
        cost.max(1).min(u64::MAX as u128) as u64
    }
}

impl DispatchUnitKind {
    fn dispatch_priority(self) -> u8 {
        match self {
            Self::Paired => 0,
            Self::HeaderOnly => 1,
            Self::ImplementationOnly => 2,
        }
    }

    fn multiplier_bp(self) -> u64 {
        match self {
            Self::Paired => 13_000,
            Self::HeaderOnly => 11_500,
            Self::ImplementationOnly => 10_000,
        }
    }
}

impl From<FileUnitKind> for DispatchUnitKind {
    fn from(value: FileUnitKind) -> Self {
        match value {
            FileUnitKind::Paired => Self::Paired,
            FileUnitKind::HeaderOnly => Self::HeaderOnly,
            FileUnitKind::ImplementationOnly => Self::ImplementationOnly,
        }
    }
}

impl ExtensionClass {
    fn for_path(path: &Path) -> Self {
        match path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .as_deref()
        {
            Some("h" | "hh" | "hpp" | "hxx") => Self::HeaderLike,
            Some("c" | "cc" | "cpp" | "cxx") => Self::ImplementationLike,
            _ => Self::Other,
        }
    }

    fn multiplier_bp(self) -> u64 {
        match self {
            Self::HeaderLike => 11_500,
            Self::ImplementationLike => 10_000,
            Self::Other => 9_500,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct SampleAnalysis {
    estimated_line_count: u64,
    preprocessor_density_bp: u64,
    comment_density_bp: u64,
}

fn semantic_context_multiplier_bp(kind: SemanticCompdbContextKind) -> u64 {
    match kind {
        SemanticCompdbContextKind::None => 8_500,
        SemanticCompdbContextKind::Exact => 10_500,
        SemanticCompdbContextKind::PairedSourceHeuristic => 12_000,
        SemanticCompdbContextKind::HeaderConsensus => 15_500,
        SemanticCompdbContextKind::SourceConsensus => 13_500,
    }
}

fn read_path_sample(path: &Path, max_bytes: usize) -> Vec<u8> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return Vec::new(),
    };
    let mut buffer = vec![0u8; max_bytes];
    match file.read(buffer.as_mut_slice()) {
        Ok(read_len) => {
            buffer.truncate(read_len);
            buffer
        }
        Err(_) => Vec::new(),
    }
}

fn analyze_sample(sample: &[u8], byte_len: u64) -> SampleAnalysis {
    if sample.is_empty() {
        return SampleAnalysis {
            estimated_line_count: if byte_len > 0 { 1 } else { 0 },
            ..SampleAnalysis::default()
        };
    }
    let text = String::from_utf8_lossy(sample);
    let mut lines = 0u64;
    let mut preprocessor_lines = 0u64;
    let mut comment_lines = 0u64;
    let mut in_block_comment = false;

    for line in text.lines() {
        lines = lines.saturating_add(1);
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            preprocessor_lines = preprocessor_lines.saturating_add(1);
        }

        let mut counts_as_comment = in_block_comment || trimmed.starts_with("//");
        if trimmed.starts_with("/*") {
            counts_as_comment = true;
            if !trimmed.contains("*/") {
                in_block_comment = true;
            }
        }
        if in_block_comment {
            counts_as_comment = true;
            if trimmed.contains("*/") {
                in_block_comment = false;
            }
        }
        if counts_as_comment {
            comment_lines = comment_lines.saturating_add(1);
        }
    }
    let sample_line_count = lines.max(1);
    let estimated_line_count = if byte_len as usize > sample.len() && !sample.is_empty() {
        sample_line_count
            .saturating_mul(byte_len)
            .saturating_div(sample.len() as u64)
            .max(1)
    } else {
        sample_line_count
    };
    SampleAnalysis {
        estimated_line_count,
        preprocessor_density_bp: preprocessor_lines
            .saturating_mul(10_000)
            .saturating_div(sample_line_count),
        comment_density_bp: comment_lines
            .saturating_mul(10_000)
            .saturating_div(sample_line_count),
    }
}

fn normalize_path_identity(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/")
}

fn ewma(current: u64, observed: u64) -> u64 {
    if current == 0 {
        return observed;
    }
    ((current as u128)
        .saturating_mul(7)
        .saturating_add(observed as u128))
    .saturating_div(8)
    .min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        DispatchFeatureCache, DispatchHistoryEntry, DispatchHistoryStore, DispatchMode,
        DispatchObservation, DispatchScheduler,
    };
    use crate::parser::manager::ParserManager;

    fn temp_dir(prefix: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("{prefix}_{stamp}"));
        fs::create_dir_all(dir.as_path()).expect("create temp dir");
        dir
    }

    #[test]
    fn plan_batches_is_deterministic_for_same_input() {
        let dir = temp_dir("mj_dispatch_deterministic");
        let files = [("a.cpp", 32usize), ("b.cpp", 640usize), ("c.hpp", 96usize)]
            .into_iter()
            .map(|(name, size)| {
                let path = dir.join(name);
                fs::write(path.as_path(), vec![b'x'; size]).expect("write fixture");
                path
            })
            .collect::<Vec<_>>();
        let parser_manager = ParserManager::new();
        let history = DispatchHistoryStore::ephemeral();
        let mut feature_cache = DispatchFeatureCache::ephemeral();

        let left = DispatchScheduler::plan_batches(
            files.clone(),
            2,
            DispatchMode::InProcess,
            &parser_manager,
            &history,
            &mut feature_cache,
        )
        .into_iter()
        .map(|batch| batch.paths)
        .collect::<Vec<_>>();
        let mut feature_cache = DispatchFeatureCache::ephemeral();
        let right = DispatchScheduler::plan_batches(
            files,
            2,
            DispatchMode::InProcess,
            &parser_manager,
            &history,
            &mut feature_cache,
        )
        .into_iter()
        .map(|batch| batch.paths)
        .collect::<Vec<_>>();

        assert_eq!(left, right);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn plan_batches_keeps_paired_units_together() {
        let dir = temp_dir("mj_dispatch_paired");
        let include_dir = dir.join("include");
        let src_dir = dir.join("src");
        fs::create_dir_all(include_dir.as_path()).expect("create include dir");
        fs::create_dir_all(src_dir.as_path()).expect("create src dir");
        let header = include_dir.join("Widget.hpp");
        let implementation = src_dir.join("Widget.cpp");
        let other = dir.join("Other.cpp");
        fs::write(header.as_path(), vec![b'a'; 64]).expect("write header");
        fs::write(implementation.as_path(), vec![b'b'; 96]).expect("write implementation");
        fs::write(other.as_path(), vec![b'c'; 1024]).expect("write other");
        let parser_manager = ParserManager::new();
        let history = DispatchHistoryStore::ephemeral();
        let mut feature_cache = DispatchFeatureCache::ephemeral();

        let batches = DispatchScheduler::plan_batches(
            vec![header.clone(), implementation.clone(), other],
            2,
            DispatchMode::WorkerProcess,
            &parser_manager,
            &history,
            &mut feature_cache,
        );
        assert!(batches
            .iter()
            .any(|batch| batch.paths.contains(&header) && batch.paths.contains(&implementation)));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn history_blending_requires_three_samples() {
        let mut history = DispatchHistoryStore::ephemeral();
        history.entries.insert(
            "a".to_string(),
            DispatchHistoryEntry {
                samples: 2,
                ewma_elapsed_ns: 900,
                ewma_retry_penalty_ns: 100,
                last_observed_size: 1,
                last_observed_lines: 1,
            },
        );
        assert_eq!(history.blended_cost("a", 100), 100);
        history.entries.insert(
            "a".to_string(),
            DispatchHistoryEntry {
                samples: 3,
                ewma_elapsed_ns: 900,
                ewma_retry_penalty_ns: 100,
                last_observed_size: 1,
                last_observed_lines: 1,
            },
        );
        assert_eq!(history.blended_cost("a", 100), 300);
    }

    #[test]
    fn corrupt_history_payload_is_ignored() {
        let dir = temp_dir("mj_dispatch_history");
        let path = dir.join("dispatch_cost_cache.bin");
        fs::write(path.as_path(), b"not-bincode").expect("write corrupt history");
        let history = DispatchHistoryStore::open(dir.as_path());
        assert!(history.entries.is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn persisted_history_round_trips() {
        let dir = temp_dir("mj_dispatch_history_round_trip");
        let mut history = DispatchHistoryStore::open(dir.as_path());
        history.record_observations(&[DispatchObservation {
            path_identity: "a".to_string(),
            observed_size: 10,
            observed_lines: 2,
            elapsed_wall_ns: 80,
            total_wall_ns: 100,
            retry_effort_units: 2,
            retry_penalty_ns: 8,
            error: false,
        }]);
        history.persist().expect("persist history");

        let reloaded = DispatchHistoryStore::open(dir.as_path());
        let entry = reloaded.entries.get("a").expect("reloaded entry");
        assert_eq!(entry.samples, 1);
        assert_eq!(entry.ewma_elapsed_ns, 80);
        assert_eq!(entry.ewma_retry_penalty_ns, 8);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn record_observations_uses_ewma() {
        let mut history = DispatchHistoryStore::ephemeral();
        history.record_observations(&[
            DispatchObservation {
                path_identity: "a".to_string(),
                observed_size: 10,
                observed_lines: 2,
                elapsed_wall_ns: 80,
                total_wall_ns: 100,
                retry_effort_units: 2,
                retry_penalty_ns: 8,
                error: false,
            },
            DispatchObservation {
                path_identity: "a".to_string(),
                observed_size: 12,
                observed_lines: 3,
                elapsed_wall_ns: 160,
                total_wall_ns: 190,
                retry_effort_units: 3,
                retry_penalty_ns: 16,
                error: false,
            },
        ]);
        let entry = history.entries.get("a").expect("history entry");
        assert_eq!(entry.samples, 2);
        assert_eq!(entry.ewma_elapsed_ns, 90);
        assert_eq!(entry.ewma_retry_penalty_ns, 9);
        assert_eq!(entry.last_observed_size, 12);
        assert_eq!(entry.last_observed_lines, 3);
    }
}
