use std::sync::OnceLock;

use dashmap::DashMap;
use serde::{Deserialize, Serialize};

use crate::model::policy_name::PolicyName;
use crate::model::retry_strategy::RetryStrategyName;
use crate::graph::state::RetryStats;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RetryStrategySnapshotEntry {
    pub strategy: RetryStrategyName,
    pub failure_context: String,
    pub stats: RetryStats,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CulpritSnapshot {
    pub culprit_policy: PolicyName,
    pub peer_policy: PolicyName,
    pub count: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RetryLearningSnapshot {
    pub strategy_outcomes: Vec<RetryStrategySnapshotEntry>,
    pub culprit_pairs: Vec<CulpritSnapshot>,
}

struct RetryLearningMaps {
    strategy_outcomes: DashMap<(RetryStrategyName, String), RetryStats>,
    culprit_pairs: DashMap<(String, String), u64>,
}

fn global() -> &'static RetryLearningMaps {
    static STATE: OnceLock<RetryLearningMaps> = OnceLock::new();
    STATE.get_or_init(|| RetryLearningMaps {
        strategy_outcomes: DashMap::new(),
        culprit_pairs: DashMap::new(),
    })
}

pub struct RetryLearningTelemetry;

impl RetryLearningTelemetry {
    pub fn reset() {
        let maps = global();
        maps.strategy_outcomes.clear();
        maps.culprit_pairs.clear();
    }

    pub fn snapshot() -> RetryLearningSnapshot {
        let maps = global();
        let mut strategy_outcomes: Vec<_> = maps
            .strategy_outcomes
            .iter()
            .map(|entry| {
                let (strategy, context) = entry.key();
                RetryStrategySnapshotEntry {
                    strategy: strategy.clone(),
                    failure_context: context.clone(),
                    stats: entry.value().clone(),
                }
            })
            .collect();
        strategy_outcomes.sort_by(|left, right| {
            left.strategy
                .cmp(&right.strategy)
                .then(left.failure_context.cmp(&right.failure_context))
        });
        let mut culprit_pairs: Vec<_> = maps
            .culprit_pairs
            .iter()
            .map(|entry| {
                let (culprit, peer) = entry.key();
                CulpritSnapshot {
                    culprit_policy: culprit.clone().into(),
                    peer_policy: peer.clone().into(),
                    count: *entry.value(),
                }
            })
            .collect();
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

    pub fn merge_snapshot(snapshot: &RetryLearningSnapshot) {
        if snapshot.strategy_outcomes.is_empty() && snapshot.culprit_pairs.is_empty() {
            return;
        }
        let maps = global();
        for entry in &snapshot.strategy_outcomes {
            if entry.strategy.as_str().trim().is_empty() || entry.stats.attempts == 0 {
                continue;
            }
            let mut target = maps
                .strategy_outcomes
                .entry((entry.strategy.clone(), entry.failure_context.clone()))
                .or_default();
            target.attempts = target.attempts.saturating_add(entry.stats.attempts);
            target.successes = target.successes.saturating_add(entry.stats.successes);
        }
        for entry in &snapshot.culprit_pairs {
            if entry.culprit_policy.as_str().trim().is_empty()
                || entry.peer_policy.as_str().trim().is_empty()
                || entry.count == 0
            {
                continue;
            }
            *maps
                .culprit_pairs
                .entry((
                    entry.culprit_policy.to_string(),
                    entry.peer_policy.to_string(),
                ))
                .or_insert(0) += entry.count;
        }
    }

    pub fn load_snapshot(snapshot: &RetryLearningSnapshot) {
        Self::reset();
        Self::merge_snapshot(snapshot);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use crate::runtime::retry_telemetry::{
        RetryLearningSnapshot, RetryLearningTelemetry, RetryStrategySnapshotEntry,
    };

    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
        match GUARD.get_or_init(|| Mutex::new(())).lock() {
            Ok(value) => value,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    #[test]
    fn snapshot_accumulates_counts() {
        let _guard = test_guard();
        RetryLearningTelemetry::reset();
        let snapshot = RetryLearningSnapshot {
            strategy_outcomes: vec![RetryStrategySnapshotEntry {
                strategy: "raise_confidence".into(),
                failure_context: "tree".to_string(),
                stats: crate::graph::state::RetryStats {
                    attempts: 3,
                    successes: 2,
                },
            }],
            culprit_pairs: vec![],
        };
        RetryLearningTelemetry::merge_snapshot(&snapshot);
        RetryLearningTelemetry::merge_snapshot(&snapshot);
        let merged = RetryLearningTelemetry::snapshot();
        assert_eq!(merged.strategy_outcomes.len(), 1);
        assert_eq!(merged.strategy_outcomes[0].stats.attempts, 6);
        assert_eq!(merged.strategy_outcomes[0].stats.successes, 4);
    }
}
