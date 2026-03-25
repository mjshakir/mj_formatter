use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::engine::accuracy_gate::AccuracyGateFailure;
use crate::engine::coordinator::FormatterEngine;
use crate::files::file_io::FileIo;
use crate::model::file_result::{FileMeta, FormatOutcome, FileResult};
use crate::model::rename_plan::SemanticRenamePlan;
use crate::runtime::result_cache::CheckResultCache;
use crate::runtime::scheduler::{DispatchObservation, ProcessedFileOutcome};

#[derive(Clone)]
pub struct FileProcessor {
    engine: Arc<FormatterEngine>,
    file_io: FileIo,
    check: bool,
    check_result_cache: Option<Arc<CheckResultCache>>,
}

impl FileProcessor {
    pub fn new(
        engine: Arc<FormatterEngine>,
        file_io: FileIo,
        check: bool,
        check_result_cache: Option<Arc<CheckResultCache>>,
    ) -> Self {
        Self {
            engine,
            file_io,
            check,
            check_result_cache,
        }
    }

    pub fn process(&self, path: PathBuf) -> ProcessedFileOutcome {
        let total_started = Instant::now();
        let original = match self.file_io.read_text(&path) {
            Ok(text) => text,
            Err(err) => {
                let total_elapsed = total_started.elapsed();
                let result = FileResult {
                    meta: FileMeta {
                        path,
                        backup_path: None,
                        engine_ms: 0.0,
                        total_ms: total_elapsed.as_secs_f64() * 1000.0,
                        boot_parse_ms: 0.0,
                    },
                    outcome: FormatOutcome::default(),
                    traces: Vec::new(),
                    error: Some(format!("read failed: {err}")),
                    warnings: Vec::new(),
                };
                let observation = DispatchObservation::from_processed_file(
                    result.meta.path.as_path(),
                    None,
                    Duration::default(),
                    total_elapsed,
                    0,
                    &result,
                );
                return ProcessedFileOutcome {
                    result,
                    observation,
                };
            }
        };
        let content_hash = if self.check && self.check_result_cache.is_some() {
            Some(CheckResultCache::content_hash(original.as_str()))
        } else {
            None
        };
        if self.check {
            if let (Some(cache), Some(hash)) =
                (self.check_result_cache.as_ref(), content_hash.as_ref())
            {
                if let Some(cached) = cache.get(path.as_path(), hash.as_str()) {
                    let observation = DispatchObservation::from_processed_file(
                        cached.meta.path.as_path(),
                        Some(original.as_str()),
                        Duration::default(),
                        total_started.elapsed(),
                        0,
                        &cached,
                    );
                    return ProcessedFileOutcome {
                        result: cached,
                        observation,
                    };
                }
            }
        }

        let engine_started = Instant::now();
        let pass_result = match self.engine.apply(&original, &path) {
            Ok(result) => result,
            Err(err) => {
                let accuracy_gate = err
                    .downcast_ref::<AccuracyGateFailure>()
                    .map(|failure| failure.decision().clone());
                let engine_elapsed = engine_started.elapsed();
                let total_elapsed = total_started.elapsed();
                let result = FileResult {
                    meta: FileMeta {
                        path,
                        backup_path: None,
                        engine_ms: engine_elapsed.as_secs_f64() * 1000.0,
                        total_ms: total_elapsed.as_secs_f64() * 1000.0,
                        boot_parse_ms: 0.0,
                    },
                    outcome: FormatOutcome {
                        accuracy_gate,
                        ..FormatOutcome::default()
                    },
                    traces: Vec::new(),
                    error: Some(format!("pipeline failed: {err}")),
                    warnings: Vec::new(),
                };
                let observation = DispatchObservation::from_processed_file(
                    result.meta.path.as_path(),
                    Some(original.as_str()),
                    engine_elapsed,
                    total_elapsed,
                    0,
                    &result,
                );
                return ProcessedFileOutcome {
                    result,
                    observation,
                };
            }
        };
        let engine_elapsed = engine_started.elapsed();
        let retry_effort_units = pass_result.metrics.retry_effort_units();
        let policy_result = pass_result.policy_result;
        let changed = !crate::parser::text_scan::TEXT_SCAN.strings_equal(&policy_result.text, &original);
        let mut rename_plans = Vec::new();
        let mut warnings = Vec::new();
        for warning in policy_result.warnings {
            if let Some(plan) = SemanticRenamePlan::from_internal_warning(warning.as_str()) {
                rename_plans.push(plan);
            } else {
                warnings.push(warning);
            }
        }

        let total_elapsed = total_started.elapsed();
        let result = FileResult {
            meta: FileMeta {
                path,
                backup_path: None,
                engine_ms: engine_elapsed.as_secs_f64() * 1000.0,
                total_ms: total_elapsed.as_secs_f64() * 1000.0,
                boot_parse_ms: pass_result.boot_parse_ms,
            },
            outcome: FormatOutcome {
                changed,
                pending_text: (changed && !self.check).then_some(policy_result.text),
                rename_plans,
                convergence_pairs: pass_result.convergence_pairs,
                violations: policy_result.violations,
                edits: policy_result.edits,
                accuracy_gate: pass_result.accuracy_gate,
                certainty: pass_result.policy_certainty,
                clang_parse: pass_result.clang_parse,
            },
            traces: pass_result.policy_traces,
            error: None,
            warnings,
        };
        if self.check {
            if let (Some(cache), Some(hash)) =
                (self.check_result_cache.as_ref(), content_hash.as_ref())
            {
                cache.put(result.meta.path.as_path(), hash.as_str(), &result);
            }
        }
        let observation = DispatchObservation::from_processed_file(
            result.meta.path.as_path(),
            Some(original.as_str()),
            engine_elapsed,
            total_started.elapsed(),
            retry_effort_units,
            &result,
        );
        ProcessedFileOutcome {
            result,
            observation,
        }
    }
}
