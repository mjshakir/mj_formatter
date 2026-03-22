use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use anyhow::{Context, Result};
use rayon::prelude::*;

use crate::app::runner::App;
use crate::config::app_config::AppConfig;
use crate::files::file_io::FileIo;
use crate::model::file_result::FileResult;
use crate::model::policy_name::PolicyName;
use crate::parser::clang_result::ClangParseResult;
use crate::parser::manager::ParserManager;
use crate::graph::store::PersistStats;
use crate::graph::state::ClusterSnapshot;
use crate::graph::state::CulpritSnapshot as ProjectGraphCulpritSnapshot;
use crate::graph::store::ProjectGraphStoreOptions;
use crate::graph::state_updater::GraphUpdater;
use crate::runtime::cluster_telemetry::{
    PolicyClusterSnapshotEntry, PolicyClusterTelemetry,
};
use crate::runtime::graph_runtime::ProjectGraphRuntime;
use crate::runtime::retry_telemetry::{
    CulpritSnapshot, RetryLearningSnapshot, RetryLearningTelemetry,
    RetryStrategySnapshotEntry,
};

/// Pre-extracted data from `&[FileResult]` for graph refresh.
/// Allows graph refresh to run on a separate thread without borrowing results.
pub(crate) struct GraphRefreshInput {
    pub convergence_pairs: BTreeMap<(String, String), usize>,
    /// (result_index, path) for changed files; None index for neighbor files.
    pub targets: Vec<(Option<usize>, PathBuf)>,
}

/// Warnings produced by graph refresh, indexed by result position.
pub(crate) struct GraphRefreshOutput {
    pub stats: Option<PersistStats>,
    /// (result_index or None, warning_message)
    pub warnings: Vec<(Option<usize>, String)>,
}

fn eager_parse_cap(total_targets: usize, population_observation_count: u32) -> usize {
    let base = (total_targets as f64).sqrt().ceil() as usize;
    let maturity = 1.0 / (1.0 + (-3.0_f64 * (population_observation_count as f64 / 10.0 - 1.0)).exp());
    let factor = 1.5 - maturity;
    let cap = (base as f64 * factor).ceil() as usize;
    cap.max(4)
}

impl App {
    pub(crate) fn open_project_graph_runtime(
        config: &AppConfig,
    ) -> Result<Option<Arc<ProjectGraphRuntime>>> {
        if !config.project_graph.enabled {
            return Ok(None);
        }

        let retention_ms = (config.project_graph.retention_days as u64)
            .saturating_mul(24)
            .saturating_mul(60)
            .saturating_mul(60)
            .saturating_mul(1000);
        let tombstone_retention_ms = (config.project_graph.tombstone_retention_days as u64)
            .saturating_mul(24)
            .saturating_mul(60)
            .saturating_mul(60)
            .saturating_mul(1000);
        let tombstone_decay_ms = (config.project_graph.tombstone_decay_days as u64)
            .saturating_mul(24)
            .saturating_mul(60)
            .saturating_mul(60)
            .saturating_mul(1000);
        let convergence_decay_half_life_ms = (config.project_graph.convergence_decay_half_life_days
            as u64)
            .saturating_mul(24)
            .saturating_mul(60)
            .saturating_mul(60)
            .saturating_mul(1000);
        let options = ProjectGraphStoreOptions {
            prune_enabled: config.project_graph.prune_enabled,
            retention_ms: retention_ms.max(1),
            max_nodes: config.project_graph.max_nodes.max(1),
            max_edges: config.project_graph.max_edges.max(1),
            tombstone_enabled: config.project_graph.tombstone_enabled,
            tombstone_retention_ms: tombstone_retention_ms.max(1),
            tombstone_decay_ms: tombstone_decay_ms.max(1),
            convergence_decay_enabled: config.project_graph.convergence_decay_enabled,
            convergence_decay_half_life_ms: convergence_decay_half_life_ms.max(1),
            convergence_decay_min_count: config.project_graph.convergence_decay_min_count.max(1),
        };
        Ok(Some(Arc::new(
            ProjectGraphRuntime::open_with_options(config.project_graph.path.as_path(), options)
                .with_context(|| {
                    format!(
                        "failed to open project graph store {}",
                        config.project_graph.path.display()
                    )
                })?,
        )))
    }

    pub(crate) fn resolve_effective_workers(configured: usize) -> usize {
        let available = Self::available_parallelism();
        if configured == 0 {
            available
        } else {
            configured.max(1).min(available)
        }
    }

    pub(crate) fn resolve_effective_jobs(configured: usize) -> usize {
        let available = Self::available_parallelism();
        if configured == 0 {
            available
        } else {
            configured.max(1)
        }
    }

    pub(crate) fn resolve_multiprocess_worker_count(
        configured_processes: usize,
        total_jobs: usize,
        file_count: usize,
    ) -> usize {
        if file_count == 0 {
            return 0;
        }
        Self::resolve_effective_workers(configured_processes)
            .min(total_jobs.max(1))
            .min(file_count)
    }

    pub(crate) fn distribute_worker_jobs(total_jobs: usize, worker_count: usize) -> Vec<usize> {
        if worker_count == 0 {
            return Vec::new();
        }

        let total_jobs = total_jobs.max(worker_count);
        let base = total_jobs / worker_count;
        let extra = total_jobs % worker_count;
        (0..worker_count)
            .map(|index| base + usize::from(index < extra))
            .collect()
    }

    pub(crate) fn available_parallelism() -> usize {
        thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
            .max(1)
    }

    pub(crate) fn seed_learning_state_from_project_graph(
        project_graph_runtime: Option<&Arc<ProjectGraphRuntime>>,
    ) {
        let cluster_seeded = !PolicyClusterTelemetry::snapshot_entries().is_empty();
        let retry_snapshot = RetryLearningTelemetry::snapshot();
        let retry_seeded = !retry_snapshot.strategy_outcomes.is_empty()
            || !retry_snapshot.culprit_pairs.is_empty();
        if cluster_seeded || retry_seeded {
            return;
        }

        PolicyClusterTelemetry::reset();
        RetryLearningTelemetry::reset();

        let Some(runtime) = project_graph_runtime else {
            return;
        };
        let state = runtime.snapshot().to_state_clone();
        let cluster_entries = state
            .cluster_snapshot()
            .into_iter()
            .map(|entry| PolicyClusterSnapshotEntry {
                policy: entry.policy,
                cluster: entry.cluster,
                stats: entry.stats,
            })
            .collect::<Vec<_>>();
        PolicyClusterTelemetry::load_entries(cluster_entries.as_slice());

        let retry_learning = RetryLearningSnapshot {
            strategy_outcomes: state
                .retry_snapshot()
                .into_iter()
                .map(
                    |(strategy, failure_context, stats)| RetryStrategySnapshotEntry {
                        strategy: strategy.into(),
                        failure_context,
                        stats,
                    },
                )
                .collect(),
            culprit_pairs: state
                .culprit_snapshot()
                .into_iter()
                .map(|entry| CulpritSnapshot {
                    culprit_policy: entry.culprit_policy,
                    peer_policy: entry.peer_policy,
                    count: entry.count,
                })
                .collect(),
        };
        RetryLearningTelemetry::load_snapshot(&retry_learning);
    }

    pub(crate) fn diff_policy_cluster_entries(
        baseline: &[PolicyClusterSnapshotEntry],
        current: &[PolicyClusterSnapshotEntry],
    ) -> Vec<PolicyClusterSnapshotEntry> {
        let mut baseline_map = HashMap::<
            (PolicyName, u64),
            crate::graph::state::PolicyClusterLearningStats,
        >::new();
        for entry in baseline {
            baseline_map.insert((entry.policy.clone(), entry.cluster), entry.stats.clone());
        }
        let mut delta = Vec::<PolicyClusterSnapshotEntry>::new();
        for entry in current {
            let baseline_stats = baseline_map.get(&(entry.policy.clone(), entry.cluster));
            let diff = Self::diff_policy_cluster_stats(&entry.stats, baseline_stats);
            if Self::policy_cluster_stats_has_signal(&diff) {
                delta.push(PolicyClusterSnapshotEntry {
                    policy: entry.policy.clone(),
                    cluster: entry.cluster,
                    stats: diff,
                });
            }
        }
        delta.sort_by(|left, right| {
            left.policy
                .cmp(&right.policy)
                .then(left.cluster.cmp(&right.cluster))
        });
        delta
    }

    pub(crate) fn diff_policy_cluster_stats(
        current: &crate::graph::state::PolicyClusterLearningStats,
        baseline: Option<&crate::graph::state::PolicyClusterLearningStats>,
    ) -> crate::graph::state::PolicyClusterLearningStats {
        let base = baseline.cloned().unwrap_or_default();
        crate::graph::state::PolicyClusterLearningStats {
            decisions: current.decisions.saturating_sub(base.decisions),
            apply: current.apply.saturating_sub(base.apply),
            apply_partial: current.apply_partial.saturating_sub(base.apply_partial),
            advisory_only: current.advisory_only.saturating_sub(base.advisory_only),
            block: current.block.saturating_sub(base.block),
            accepted: current.accepted.saturating_sub(base.accepted),
            regressed: current.regressed.saturating_sub(base.regressed),
            reverted: current.reverted.saturating_sub(base.reverted),
            decision_ema_bp: 0,
            outcome_ema_bp: 0,
            decision_events: 0,
            outcome_events: 0,
        }
    }

    pub(crate) fn policy_cluster_stats_has_signal(
        stats: &crate::graph::state::PolicyClusterLearningStats,
    ) -> bool {
        stats.decisions > 0
            || stats.apply > 0
            || stats.apply_partial > 0
            || stats.advisory_only > 0
            || stats.block > 0
            || stats.accepted > 0
            || stats.regressed > 0
            || stats.reverted > 0
            || stats.decision_events > 0
            || stats.outcome_events > 0
    }

    pub(crate) fn diff_retry_learning_snapshot(
        baseline: &RetryLearningSnapshot,
        current: &RetryLearningSnapshot,
    ) -> RetryLearningSnapshot {
        let mut baseline_strategy = HashMap::<
            (crate::model::retry_strategy::RetryStrategyName, String),
            crate::graph::state::RetryStats,
        >::new();
        for entry in &baseline.strategy_outcomes {
            baseline_strategy.insert(
                (entry.strategy.clone(), entry.failure_context.clone()),
                entry.stats.clone(),
            );
        }
        let mut strategy_outcomes = Vec::<RetryStrategySnapshotEntry>::new();
        for entry in &current.strategy_outcomes {
            let baseline_stats = baseline_strategy
                .get(&(entry.strategy.clone(), entry.failure_context.clone()))
                .cloned()
                .unwrap_or_default();
            let attempts = entry.stats.attempts.saturating_sub(baseline_stats.attempts);
            let successes = entry
                .stats
                .successes
                .saturating_sub(baseline_stats.successes);
            if attempts == 0 && successes == 0 {
                continue;
            }
            strategy_outcomes.push(RetryStrategySnapshotEntry {
                strategy: entry.strategy.clone(),
                failure_context: entry.failure_context.clone(),
                stats: crate::graph::state::RetryStats {
                    attempts,
                    successes,
                },
            });
        }
        strategy_outcomes.sort_by(|left, right| {
            left.strategy
                .cmp(&right.strategy)
                .then(left.failure_context.cmp(&right.failure_context))
        });

        let mut baseline_pairs = HashMap::<(PolicyName, PolicyName), u64>::new();
        for entry in &baseline.culprit_pairs {
            baseline_pairs.insert(
                (entry.culprit_policy.clone(), entry.peer_policy.clone()),
                entry.count,
            );
        }
        let mut culprit_pairs = Vec::<CulpritSnapshot>::new();
        for entry in &current.culprit_pairs {
            let baseline_count = baseline_pairs
                .get(&(entry.culprit_policy.clone(), entry.peer_policy.clone()))
                .copied()
                .unwrap_or(0);
            let count = entry.count.saturating_sub(baseline_count);
            if count == 0 {
                continue;
            }
            culprit_pairs.push(CulpritSnapshot {
                culprit_policy: entry.culprit_policy.clone(),
                peer_policy: entry.peer_policy.clone(),
                count,
            });
        }
        culprit_pairs.sort_by(|left, right| {
            left.culprit_policy
                .cmp(&right.culprit_policy)
                .then(left.peer_policy.cmp(&right.peer_policy))
        });

        RetryLearningSnapshot {
            strategy_outcomes,
            culprit_pairs,
        }
    }

    fn parse_project_graph_target(
        index: Option<usize>,
        path: PathBuf,
        file_io: &FileIo,
        parser_manager: &ParserManager,
    ) -> (
        Option<usize>,
        PathBuf,
        Option<Arc<ClangParseResult>>,
        Option<String>,
    ) {
        let text = match file_io.read_text(&path) {
            Ok(content) => content,
            Err(err) => {
                return (
                    index,
                    path,
                    None,
                    Some(format!(
                        "project_graph: failed reading file for graph update: {err}"
                    )),
                );
            }
        };
        let parse = match parser_manager.parse_clang(text.as_str(), &path) {
            Ok(value) => value,
            Err(err) => {
                return (
                    index,
                    path,
                    None,
                    Some(format!(
                        "project_graph: clang parse failed for graph update: {err}"
                    )),
                );
            }
        };
        (index, path, Some(parse), None)
    }

    pub(crate) fn extract_graph_refresh_input(
        results: &[FileResult],
        project_graph: &ProjectGraphRuntime,
        project_graph_config: &crate::config::types::ProjectGraphConfig,
        include_parse_updates: bool,
        population_observation_count: u32,
    ) -> (GraphRefreshInput, Vec<(Option<usize>, String)>) {
        let convergence_pairs = Self::collect_convergence_pairs(results);
        let edit_counts: Vec<usize> = results.iter().map(|r| r.outcome.edits.len()).collect();
        let mut targets = if include_parse_updates {
            results
                .iter()
                .enumerate()
                .filter(|(_, result)| result.error.is_none() && result.outcome.changed)
                .map(|(index, result)| (Some(index), result.meta.path.clone()))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let mut input_warnings: Vec<(Option<usize>, String)> = Vec::new();
        if include_parse_updates
            && project_graph_config.incremental_neighborhood_enabled
            && !targets.is_empty()
        {
            let graph_snapshot = project_graph.snapshot();
            let changed_paths = targets
                .iter()
                .map(|(_, path)| path.clone())
                .collect::<Vec<_>>();
            let neighbors = graph_snapshot.affected_file_paths(
                changed_paths.as_slice(),
                project_graph_config.incremental_neighborhood_hops.max(1),
                project_graph_config
                    .incremental_neighborhood_max_files
                    .max(1),
            );
            let mut seen = targets
                .iter()
                .map(|(_, path)| Self::path_identity(path.as_path()))
                .collect::<HashSet<_>>();
            let mut added = 0usize;
            for neighbor in neighbors {
                if !neighbor.exists() {
                    continue;
                }
                let key = Self::path_identity(neighbor.as_path());
                if seen.insert(key) {
                    targets.push((None, neighbor));
                    added = added.saturating_add(1);
                }
            }
            if added > 0 {
                let first_ok = results.iter().position(|r| r.error.is_none());
                input_warnings.push((first_ok, format!(
                    "project_graph: expanded incremental refresh with {} affected-neighbor file(s)",
                    added
                )));
            }
        }
        let cap = eager_parse_cap(targets.len(), population_observation_count);
        if targets.len() > cap {
            targets.sort_by(|a, b| {
                let edits_a = a.0.map(|i| edit_counts.get(i).copied().unwrap_or(0)).unwrap_or(0);
                let edits_b = b.0.map(|i| edit_counts.get(i).copied().unwrap_or(0)).unwrap_or(0);
                edits_b.cmp(&edits_a)
            });
            let deferred = targets.len() - cap;
            targets.truncate(cap);
            let first_ok = results.iter().position(|r| r.error.is_none());
            input_warnings.push((first_ok, format!(
                "project_graph: deferred parse refresh for {} lower-impact file(s) (cap={}, obs={})",
                deferred, cap, population_observation_count
            )));
        }
        (GraphRefreshInput {
            convergence_pairs,
            targets,
        }, input_warnings)
    }

    pub(crate) fn refresh_project_graph_owned(
        input: GraphRefreshInput,
        project_graph: &ProjectGraphRuntime,
        file_io: &FileIo,
        parser_manager: &ParserManager,
        parallel_pool: Option<&rayon::ThreadPool>,
    ) -> GraphRefreshOutput {
        let mut warnings: Vec<(Option<usize>, String)> = Vec::new();

        let graph_snapshot = project_graph.snapshot();
        let persisted_state = graph_snapshot.to_state_clone();
        let persisted_cluster = persisted_state
            .cluster_snapshot()
            .into_iter()
            .map(|entry| PolicyClusterSnapshotEntry {
                policy: entry.policy,
                cluster: entry.cluster,
                stats: entry.stats,
            })
            .collect::<Vec<_>>();
        let cluster_learning_entries = PolicyClusterTelemetry::snapshot_entries();
        let cluster_learning_changed = persisted_cluster != cluster_learning_entries;
        let persisted_retry = RetryLearningSnapshot {
            strategy_outcomes: persisted_state
                .retry_snapshot()
                .into_iter()
                .map(|(strategy, failure_context, stats)| RetryStrategySnapshotEntry {
                    strategy: strategy.into(),
                    failure_context,
                    stats,
                })
                .collect(),
            culprit_pairs: persisted_state
                .culprit_snapshot()
                .into_iter()
                .map(|entry| CulpritSnapshot {
                    culprit_policy: entry.culprit_policy,
                    peer_policy: entry.peer_policy,
                    count: entry.count,
                })
                .collect(),
        };
        let retry_learning_delta = Self::diff_retry_learning_snapshot(
            &persisted_retry,
            &RetryLearningTelemetry::snapshot(),
        );
        let cluster_learning_state_entries = cluster_learning_entries
            .iter()
            .map(|entry| ClusterSnapshot {
                policy: entry.policy.clone(),
                cluster: entry.cluster,
                stats: entry.stats.clone(),
            })
            .collect::<Vec<_>>();
        let retry_strategy_entries = retry_learning_delta
            .strategy_outcomes
            .iter()
            .map(|entry| {
                (
                    entry.strategy.clone(),
                    entry.failure_context.clone(),
                    entry.stats.attempts,
                    entry.stats.successes,
                )
            })
            .collect::<Vec<_>>();
        let culprit_pairs = retry_learning_delta
            .culprit_pairs
            .iter()
            .map(|entry| ProjectGraphCulpritSnapshot {
                culprit_policy: entry.culprit_policy.clone(),
                peer_policy: entry.peer_policy.clone(),
                count: entry.count,
            })
            .collect::<Vec<_>>();

        if input.targets.is_empty()
            && input.convergence_pairs.is_empty()
            && !cluster_learning_changed
            && retry_strategy_entries.is_empty()
            && culprit_pairs.is_empty()
        {
            return GraphRefreshOutput {
                stats: None,
                warnings,
            };
        }

        let file_io_for_parse = Arc::new(file_io.clone());
        let parser_for_parse = Arc::new(parser_manager.clone());
        let outcomes: Vec<_> = if let Some(pool) = parallel_pool {
            let fio = file_io_for_parse.clone();
            let parser = parser_for_parse.clone();
            pool.install(|| {
                input
                    .targets
                    .into_par_iter()
                    .map(|(index, path)| {
                        Self::parse_project_graph_target(index, path, &fio, &parser)
                    })
                    .collect()
            })
        } else {
            input
                .targets
                .into_iter()
                .map(|(index, path)| {
                    Self::parse_project_graph_target(
                        index,
                        path,
                        &file_io_for_parse,
                        &parser_for_parse,
                    )
                })
                .collect()
        };

        let mut file_parses = Vec::with_capacity(outcomes.len());
        for (index, path, parse, warning) in outcomes {
            if let Some(parse) = parse {
                file_parses.push((path, parse));
            }
            if let Some(warning) = warning {
                warnings.push((index, warning));
            }
        }

        if file_parses.is_empty()
            && input.convergence_pairs.is_empty()
            && !cluster_learning_changed
            && retry_strategy_entries.is_empty()
            && culprit_pairs.is_empty()
        {
            return GraphRefreshOutput {
                stats: None,
                warnings,
            };
        }

        let stats = match project_graph.update_state_with_stats(|state| {
            for (path, parse) in &file_parses {
                GraphUpdater::apply_clang_parse(state, path.as_path(), parse);
            }
            if !input.convergence_pairs.is_empty() {
                state.record_convergence_pairs(&input.convergence_pairs);
            }
            if cluster_learning_changed {
                state.replace_clusters(
                    cluster_learning_state_entries.as_slice(),
                );
            }
            for (strategy, context, attempts, successes) in &retry_strategy_entries {
                state.record_retry(
                    strategy.as_str(),
                    context.as_str(),
                    *attempts,
                    *successes,
                );
            }
            if !culprit_pairs.is_empty() {
                state.record_culprits(culprit_pairs.as_slice());
            }
        }) {
            Ok((_, stats)) => Some(stats),
            Err(err) => {
                warnings.push((None, format!("project_graph: persist update failed: {err}")));
                None
            }
        };

        GraphRefreshOutput { stats, warnings }
    }

}

#[cfg(test)]
mod tests {
    use super::eager_parse_cap;

    #[test]
    fn cap_scales_sublinearly() {
        // Cold start (obs=0): sqrt(31)≈6 * 1.5 = 9
        let cap = eager_parse_cap(31, 0);
        assert!(cap >= 4 && cap <= 12, "cold cap for 31: {cap}");
        // Warm start (obs=20): sqrt(31)≈6 * ~0.5 = 3 → clamped to 4
        let cap_warm = eager_parse_cap(31, 20);
        assert!(cap_warm >= 4, "warm cap for 31: {cap_warm}");
        assert!(cap_warm <= cap, "warm should be <= cold: {cap_warm} vs {cap}");
        // Small batch: always fits
        let cap_small = eager_parse_cap(4, 0);
        assert!(cap_small >= 4, "small batch cap: {cap_small}");
    }
}
