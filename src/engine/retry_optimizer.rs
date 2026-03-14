use std::collections::BTreeMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::config::optimizer_config::RetryStrategyOptimizerConfig;
use crate::files::atomic_writer::AtomicWriter;
use crate::files::codec::StateCodec;

const STATE_SCHEMA_VERSION: u32 = 2;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct StrategyStats {
    attempts: u64,
    successes: u64,
    ema_success: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PersistedRetryStrategyState {
    schema_version: u32,
    total_observations: u64,
    global: BTreeMap<String, StrategyStats>,
    by_context: BTreeMap<String, BTreeMap<String, StrategyStats>>,
    ema_retry_success: f64,
    tuned_max_bonus: i32,
    auto_tune_updates: u64,
    last_updated_unix_ms: u64,
}

impl Default for PersistedRetryStrategyState {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            total_observations: 0,
            global: BTreeMap::new(),
            by_context: BTreeMap::new(),
            ema_retry_success: 0.0,
            tuned_max_bonus: 0,
            auto_tune_updates: 0,
            last_updated_unix_ms: 0,
        }
    }
}

pub struct RetryStrategyOptimizer;

impl RetryStrategyOptimizer {
    pub fn merge_state_files(
        target_path: &Path,
        shard_paths: &[std::path::PathBuf],
        config: &RetryStrategyOptimizerConfig,
    ) -> anyhow::Result<()> {
        let mut merged = Self::load_state(target_path);
        let mut merged_any = false;
        for shard in shard_paths {
            let shard_state = Self::load_state(shard.as_path());
            if shard_state.total_observations == 0 {
                continue;
            }
            merged_any = true;
            merged = Self::merge_states(merged, shard_state, config);
        }
        if merged_any {
            let (min_bonus, max_bonus) = Self::bonus_bounds_for_config(config);
            merged.tuned_max_bonus = merged.tuned_max_bonus.clamp(min_bonus, max_bonus);
            merged.schema_version = STATE_SCHEMA_VERSION;
            Self::persist_state(target_path, &merged)?;
        }
        Ok(())
    }

    fn load_state(path: &Path) -> PersistedRetryStrategyState {
        if !path.exists() {
            return PersistedRetryStrategyState::default();
        }
        let persisted = match StateCodec::read_decode_binary::<PersistedRetryStrategyState>(path) {
            Ok(value) => value,
            Err(error) => {
                Self::quarantine_corrupted_state(path, format!("decode_failed: {error}").as_str());
                warn!(
                    error = %error,
                    path = %path.display(),
                    "retry strategy optimizer state load failed; using defaults"
                );
                return PersistedRetryStrategyState::default();
            }
        };
        if persisted.schema_version != STATE_SCHEMA_VERSION {
            let reason = format!(
                "schema_mismatch expected={} found={}",
                STATE_SCHEMA_VERSION, persisted.schema_version
            );
            Self::quarantine_corrupted_state(path, reason.as_str());
            warn!(
                path = %path.display(),
                expected = STATE_SCHEMA_VERSION,
                found = persisted.schema_version,
                "retry strategy optimizer schema mismatch; using defaults"
            );
            return PersistedRetryStrategyState::default();
        }
        persisted
    }

    fn quarantine_corrupted_state(path: &Path, reason: &str) {
        let file_name = path
            .file_name()
            .and_then(|item| item.to_str())
            .unwrap_or("retry_strategy_optimizer_state.bin");
        let quarantined = path.with_file_name(format!("{file_name}.corrupt.{}", current_unix_ms()));
        if let Err(rename_error) = std::fs::rename(path, quarantined.as_path()) {
            warn!(
                path = %path.display(),
                quarantined = %quarantined.display(),
                reason = %reason,
                error = %rename_error,
                "failed to quarantine retry strategy optimizer state"
            );
        } else {
            warn!(
                path = %path.display(),
                quarantined = %quarantined.display(),
                reason = %reason,
                "quarantined retry strategy optimizer state"
            );
        }
    }

    fn persist_state(path: &Path, state: &PersistedRetryStrategyState) -> anyhow::Result<()> {
        let bytes = StateCodec::encode_binary(state)?;
        AtomicWriter::write_bytes(path, bytes.as_slice())?;
        Ok(())
    }

    fn bonus_bounds_for_config(config: &RetryStrategyOptimizerConfig) -> (i32, i32) {
        let min_bonus = config.auto_tune_min_bonus.max(0);
        let max_bonus = config.auto_tune_max_bonus_cap.max(min_bonus.max(1));
        (min_bonus, max_bonus)
    }

    fn merge_states(
        mut base: PersistedRetryStrategyState,
        incoming: PersistedRetryStrategyState,
        config: &RetryStrategyOptimizerConfig,
    ) -> PersistedRetryStrategyState {
        let base_obs = base.total_observations;
        let incoming_obs = incoming.total_observations;
        base.ema_retry_success = weighted_average(
            base.ema_retry_success,
            base_obs,
            incoming.ema_retry_success,
            incoming_obs,
        );
        base.total_observations = base
            .total_observations
            .saturating_add(incoming.total_observations);
        base.auto_tune_updates = base
            .auto_tune_updates
            .saturating_add(incoming.auto_tune_updates);
        base.last_updated_unix_ms = base.last_updated_unix_ms.max(incoming.last_updated_unix_ms);
        base.tuned_max_bonus = weighted_average(
            base.tuned_max_bonus as f64,
            base_obs,
            incoming.tuned_max_bonus as f64,
            incoming_obs,
        )
        .round() as i32;

        Self::merge_strategy_map(&mut base.global, &incoming.global);
        for (context, incoming_map) in incoming.by_context {
            let entry = base.by_context.entry(context).or_default();
            Self::merge_strategy_map(entry, &incoming_map);
        }
        let (min_bonus, max_bonus) = Self::bonus_bounds_for_config(config);
        base.tuned_max_bonus = base.tuned_max_bonus.clamp(min_bonus, max_bonus);
        base
    }

    fn merge_strategy_map(
        target: &mut BTreeMap<String, StrategyStats>,
        incoming: &BTreeMap<String, StrategyStats>,
    ) {
        for (strategy, incoming_stats) in incoming {
            let entry = target.entry(strategy.clone()).or_default();
            entry.ema_success = weighted_average(
                entry.ema_success,
                entry.attempts,
                incoming_stats.ema_success,
                incoming_stats.attempts,
            );
            entry.attempts = entry.attempts.saturating_add(incoming_stats.attempts);
            entry.successes = entry.successes.saturating_add(incoming_stats.successes);
        }
    }
}

fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn weighted_average(a: f64, a_weight: u64, b: f64, b_weight: u64) -> f64 {
    let total = a_weight.saturating_add(b_weight);
    if total == 0 {
        return 0.0;
    }
    ((a * a_weight as f64) + (b * b_weight as f64)) / total as f64
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs, path::PathBuf};

    use crate::config::optimizer_config::RetryStrategyOptimizerConfig;
    use crate::files::codec::StateCodec;

    use super::{PersistedRetryStrategyState, RetryStrategyOptimizer, StrategyStats};

    fn temp_path(name: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("mj_fmt_retry_optimizer_{name}_{stamp}.json"))
    }

    #[test]
    fn merge_state_files_accumulates_worker_shards() {
        let target = temp_path("merge_target");
        let shard_a = temp_path("merge_a");
        let shard_b = temp_path("merge_b");
        let mut a_global = BTreeMap::<String, StrategyStats>::new();
        a_global.insert(
            "top_only".to_string(),
            StrategyStats {
                attempts: 5,
                successes: 5,
                ema_success: 1.0,
            },
        );
        let mut a_context_entry = BTreeMap::<String, StrategyStats>::new();
        a_context_entry.insert(
            "top_only".to_string(),
            StrategyStats {
                attempts: 5,
                successes: 5,
                ema_success: 1.0,
            },
        );
        let mut a_context = BTreeMap::<String, BTreeMap<String, StrategyStats>>::new();
        a_context.insert("tree_error_regressed".to_string(), a_context_entry);
        let a = PersistedRetryStrategyState {
            schema_version: 2,
            total_observations: 5,
            global: a_global,
            by_context: a_context,
            ema_retry_success: 1.0,
            tuned_max_bonus: 150,
            auto_tune_updates: 1,
            last_updated_unix_ms: 10,
        };

        let mut b_global = BTreeMap::<String, StrategyStats>::new();
        b_global.insert(
            "top_only".to_string(),
            StrategyStats {
                attempts: 7,
                successes: 6,
                ema_success: 0.9,
            },
        );
        let mut b_context_entry = BTreeMap::<String, StrategyStats>::new();
        b_context_entry.insert(
            "top_only".to_string(),
            StrategyStats {
                attempts: 7,
                successes: 6,
                ema_success: 0.9,
            },
        );
        let mut b_context = BTreeMap::<String, BTreeMap<String, StrategyStats>>::new();
        b_context.insert("tree_error_regressed".to_string(), b_context_entry);
        let b = PersistedRetryStrategyState {
            schema_version: 2,
            total_observations: 7,
            global: b_global,
            by_context: b_context,
            ema_retry_success: 0.9,
            tuned_max_bonus: 140,
            auto_tune_updates: 2,
            last_updated_unix_ms: 20,
        };
        fs::write(
            shard_a.as_path(),
            StateCodec::encode_binary(&a).expect("encode shard a"),
        )
        .expect("write shard a");
        fs::write(
            shard_b.as_path(),
            StateCodec::encode_binary(&b).expect("encode shard b"),
        )
        .expect("write shard b");

        let config = RetryStrategyOptimizerConfig::default();
        RetryStrategyOptimizer::merge_state_files(
            target.as_path(),
            &[shard_a.clone(), shard_b.clone()],
            &config,
        )
        .expect("merge");
        let merged = RetryStrategyOptimizer::load_state(target.as_path());
        assert_eq!(merged.total_observations, 12);
        assert_eq!(merged.auto_tune_updates, 3);
        assert!(merged.tuned_max_bonus >= 140);

        let _ = fs::remove_file(target);
        let _ = fs::remove_file(shard_a);
        let _ = fs::remove_file(shard_b);
    }
}
