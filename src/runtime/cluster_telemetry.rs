use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::OnceLock;

use arc_swap::ArcSwapOption;
use dashmap::DashMap;

use crate::engine::edit_candidate::PolicyDecisionOutcome;
use crate::model::exec_trace::PolicyExecutionTrace;
use crate::model::policy_name::PolicyName;
use crate::graph::state::PolicyClusterLearningStats;
use serde::{Deserialize, Serialize};

const DECISION_EMA_ALPHA: f64 = 0.22;
const OUTCOME_EMA_ALPHA: f64 = 0.26;
const MIN_ADAPTIVE_DECISIONS: u64 = 6;
const WILSON_Z: f64 = 1.959_963_984_540_054;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClusterOutcome {
    Accepted,
    Regressed,
    Reverted,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ClusterEnforcementBias {
    Relax,
    #[default]
    Neutral,
    Harden,
}

#[derive(Clone, Copy, Debug)]
pub struct ClusterAdaptiveControls {
    pub enforcement_bias: ClusterEnforcementBias,
    pub max_impact_radius_cap: Option<usize>,
}

impl Default for ClusterAdaptiveControls {
    fn default() -> Self {
        Self {
            enforcement_bias: ClusterEnforcementBias::Neutral,
            max_impact_radius_cap: None,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PolicyClusterSnapshotEntry {
    pub policy: PolicyName,
    pub cluster: u64,
    pub stats: PolicyClusterLearningStats,
}

#[derive(Clone, Copy, Debug)]
struct ClusterAdaptiveState {
    decision_ema: f64,
    outcome_ema: f64,
    decision_events: u64,
    outcome_events: u64,
}

impl Default for ClusterAdaptiveState {
    fn default() -> Self {
        Self {
            decision_ema: 0.50,
            outcome_ema: 0.50,
            decision_events: 0,
            outcome_events: 0,
        }
    }
}

#[derive(Clone, Default)]
struct ClusterCombinedEntry {
    stats: PolicyClusterLearningStats,
    adaptive: ClusterAdaptiveState,
}

struct PolicyClusterTelemetryState {
    entries: DashMap<(String, u64), ClusterCombinedEntry>,
    read_model: ArcSwapOption<FxHashMap<(String, u64), ClusterCombinedEntry>>,
}

pub struct PolicyClusterTelemetry;
pub struct ClusterGuard;

impl Drop for ClusterGuard {
    fn drop(&mut self) {
        PolicyClusterTelemetry::clear_read_model();
    }
}

impl PolicyClusterTelemetry {
    pub fn reset() {
        let s = Self::global();
        s.entries.clear();
        s.read_model.store(None);
    }

    pub fn begin_read_model() -> ClusterGuard {
        let s = Self::global();
        let model = Self::build_read_model_snapshot(&s.entries);
        s.read_model.store(Some(std::sync::Arc::new(model)));
        ClusterGuard
    }

    pub fn clear_read_model() {
        Self::global().read_model.store(None);
    }

    pub fn record_decision(policy: &str, cluster: u64, outcome: PolicyDecisionOutcome) {
        let s = Self::global();
        let key = (policy.to_string(), cluster);
        let mut combined = s.entries.entry(key).or_default();
        combined.stats.decisions = combined.stats.decisions.saturating_add(1);
        match outcome {
            PolicyDecisionOutcome::Apply => combined.stats.apply = combined.stats.apply.saturating_add(1),
            PolicyDecisionOutcome::ApplyPartial => {
                combined.stats.apply_partial = combined.stats.apply_partial.saturating_add(1);
            }
            PolicyDecisionOutcome::Block => {
                combined.stats.block = combined.stats.block.saturating_add(1);
            }
        }
        let decision_sample = Self::decision_sample_from_outcome(outcome);
        Self::apply_ema_sample(&mut combined.adaptive, true, decision_sample);
    }

    pub fn record_outcome(traces: &[PolicyExecutionTrace], outcome: ClusterOutcome) {
        if traces.is_empty() {
            return;
        }
        let s = Self::global();
        let mut seen: FxHashSet<(String, u64)> = FxHashSet::default();
        for trace in traces {
            let key = (trace.policy.to_string(), trace.context_cluster);
            if !seen.insert(key.clone()) {
                continue;
            }
            let mut combined = s.entries.entry(key).or_default();
            match outcome {
                ClusterOutcome::Accepted => combined.stats.accepted = combined.stats.accepted.saturating_add(1),
                ClusterOutcome::Regressed => combined.stats.regressed = combined.stats.regressed.saturating_add(1),
                ClusterOutcome::Reverted => combined.stats.reverted = combined.stats.reverted.saturating_add(1),
            }
            let outcome_sample = Self::outcome_sample_from_outcome(outcome);
            Self::apply_ema_sample(&mut combined.adaptive, false, outcome_sample);
        }
    }

    pub fn adaptive_controls(policy: &str, cluster: u64, kalman: &crate::engine::certainty_filter::CertaintyFilterState) -> ClusterAdaptiveControls {
        let s = Self::global();
        let key = (policy.to_string(), cluster);
        let model = s.read_model.load();
        if let Some(model) = model.as_deref() {
            let Some(combined) = model.get(&key) else {
                return ClusterAdaptiveControls::default();
            };
            return Self::controls_for(&combined.stats, &combined.adaptive, kalman);
        }
        let Some(combined) = s.entries.get(&key) else {
            return ClusterAdaptiveControls::default();
        };
        Self::controls_for(&combined.stats, &combined.adaptive, kalman)
    }

    pub fn snapshot_entries() -> Vec<PolicyClusterSnapshotEntry> {
        let s = Self::global();
        let mut entries: Vec<_> = s
            .entries
            .iter()
            .map(|entry| {
                let (policy, cluster) = entry.key();
                let combined = entry.value();
                let mut resolved = combined.stats.clone();
                Self::write_adaptive_hints(&mut resolved, &combined.adaptive);
                PolicyClusterSnapshotEntry {
                    policy: policy.clone().into(),
                    cluster: *cluster,
                    stats: resolved,
                }
            })
            .collect();
        entries.sort_by(|left, right| {
            left.policy
                .cmp(&right.policy)
                .then(left.cluster.cmp(&right.cluster))
        });
        entries
    }

    pub fn merge_entries(entries: &[PolicyClusterSnapshotEntry]) {
        if entries.is_empty() {
            return;
        }
        let s = Self::global();
        for entry in entries {
            if entry.policy.as_str().trim().is_empty() {
                continue;
            }
            let key = (entry.policy.to_string(), entry.cluster);
            let mut combined = s.entries.entry(key).or_default();
            combined.stats.decisions = combined.stats.decisions.saturating_add(entry.stats.decisions);
            combined.stats.apply = combined.stats.apply.saturating_add(entry.stats.apply);
            combined.stats.apply_partial = combined.stats.apply_partial.saturating_add(entry.stats.apply_partial);
            combined.stats.advisory_only = combined.stats.advisory_only.saturating_add(entry.stats.advisory_only);
            combined.stats.block = combined.stats.block.saturating_add(entry.stats.block);
            combined.stats.accepted = combined.stats.accepted.saturating_add(entry.stats.accepted);
            combined.stats.regressed = combined.stats.regressed.saturating_add(entry.stats.regressed);
            combined.stats.reverted = combined.stats.reverted.saturating_add(entry.stats.reverted);
            Self::merge_adaptive_state(&mut combined.adaptive, &entry.stats);
        }
    }

    pub fn load_entries(entries: &[PolicyClusterSnapshotEntry]) {
        Self::reset();
        Self::merge_entries(entries);
    }

    #[cfg(test)]
    pub fn snapshot_entry(policy: &str, cluster: u64) -> Option<PolicyClusterLearningStats> {
        let s = Self::global();
        let key = (policy.to_string(), cluster);
        s.entries.get(&key).map(|combined| {
            let mut resolved = combined.stats.clone();
            Self::write_adaptive_hints(&mut resolved, &combined.adaptive);
            resolved
        })
    }

    fn global() -> &'static PolicyClusterTelemetryState {
        static STATE: OnceLock<PolicyClusterTelemetryState> = OnceLock::new();
        STATE.get_or_init(|| PolicyClusterTelemetryState {
            entries: DashMap::new(),
            read_model: ArcSwapOption::empty(),
        })
    }

    fn build_read_model_snapshot(
        entries: &DashMap<(String, u64), ClusterCombinedEntry>,
    ) -> FxHashMap<(String, u64), ClusterCombinedEntry> {
        entries
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect()
    }

    fn decision_sample_from_outcome(outcome: PolicyDecisionOutcome) -> f64 {
        match outcome {
            PolicyDecisionOutcome::Apply => 1.0,
            PolicyDecisionOutcome::ApplyPartial => 0.72,
            PolicyDecisionOutcome::Block => 0.05,
        }
    }

    fn outcome_sample_from_outcome(outcome: ClusterOutcome) -> f64 {
        match outcome {
            ClusterOutcome::Accepted => 1.0,
            ClusterOutcome::Regressed => 0.25,
            ClusterOutcome::Reverted => 0.0,
        }
    }

    fn apply_ema_sample(state: &mut ClusterAdaptiveState, decision_axis: bool, sample: f64) {
        let bounded = sample.clamp(0.0, 1.0);
        if decision_axis {
            state.decision_ema = Self::ema_step(state.decision_ema, DECISION_EMA_ALPHA, bounded);
            state.decision_events = state.decision_events.saturating_add(1);
        } else {
            state.outcome_ema = Self::ema_step(state.outcome_ema, OUTCOME_EMA_ALPHA, bounded);
            state.outcome_events = state.outcome_events.saturating_add(1);
        }
    }

    fn apply_stats_batch(adaptive: &mut ClusterAdaptiveState, stats: &PolicyClusterLearningStats) {
        let decision_total = stats.decision_total();
        if decision_total > 0 {
            let decision_mean = ((stats.apply as f64)
                + (stats.apply_partial as f64 * 0.72)
                + (stats.advisory_only as f64 * 0.28)
                + (stats.block as f64 * 0.05))
                / decision_total as f64;
            adaptive.decision_ema = Self::ema_batch(
                adaptive.decision_ema,
                DECISION_EMA_ALPHA,
                decision_mean.clamp(0.0, 1.0),
                decision_total,
            );
            adaptive.decision_events = adaptive.decision_events.saturating_add(decision_total);
        }

        let outcome_total = stats.outcome_total();
        if outcome_total > 0 {
            let outcome_mean =
                ((stats.accepted as f64) + (stats.regressed as f64 * 0.25)) / outcome_total as f64;
            adaptive.outcome_ema = Self::ema_batch(
                adaptive.outcome_ema,
                OUTCOME_EMA_ALPHA,
                outcome_mean.clamp(0.0, 1.0),
                outcome_total,
            );
            adaptive.outcome_events = adaptive.outcome_events.saturating_add(outcome_total);
        }
    }

    fn merge_adaptive_state(
        adaptive: &mut ClusterAdaptiveState,
        stats: &PolicyClusterLearningStats,
    ) {
        if stats.has_adaptive_hints() {
            let incoming = Self::adaptive_from_hints(stats);
            Self::blend_adaptive(adaptive, &incoming);
        } else {
            Self::apply_stats_batch(adaptive, stats);
        }
    }

    fn adaptive_from_hints(stats: &PolicyClusterLearningStats) -> ClusterAdaptiveState {
        ClusterAdaptiveState {
            decision_ema: Self::bp_to_ema(stats.decision_ema_bp),
            outcome_ema: Self::bp_to_ema(stats.outcome_ema_bp),
            decision_events: stats.decision_events,
            outcome_events: stats.outcome_events,
        }
    }

    fn write_adaptive_hints(
        stats: &mut PolicyClusterLearningStats,
        adaptive: &ClusterAdaptiveState,
    ) {
        stats.decision_ema_bp = Self::ema_to_bp(adaptive.decision_ema);
        stats.outcome_ema_bp = Self::ema_to_bp(adaptive.outcome_ema);
        stats.decision_events = adaptive.decision_events;
        stats.outcome_events = adaptive.outcome_events;
    }

    fn blend_adaptive(target: &mut ClusterAdaptiveState, incoming: &ClusterAdaptiveState) {
        let prior_decision_events = target.decision_events;
        let prior_outcome_events = target.outcome_events;
        target.decision_events = target
            .decision_events
            .saturating_add(incoming.decision_events);
        target.outcome_events = target
            .outcome_events
            .saturating_add(incoming.outcome_events);
        target.decision_ema = Self::weighted_mean(
            target.decision_ema,
            prior_decision_events,
            incoming.decision_ema,
            incoming.decision_events,
        );
        target.outcome_ema = Self::weighted_mean(
            target.outcome_ema,
            prior_outcome_events,
            incoming.outcome_ema,
            incoming.outcome_events,
        );
    }

    fn weighted_mean(lhs: f64, lhs_weight: u64, rhs: f64, rhs_weight: u64) -> f64 {
        if lhs_weight == 0 {
            return rhs.clamp(0.0, 1.0);
        }
        if rhs_weight == 0 {
            return lhs.clamp(0.0, 1.0);
        }
        let lhs_weight_f = lhs_weight as f64;
        let rhs_weight_f = rhs_weight as f64;
        ((lhs * lhs_weight_f + rhs * rhs_weight_f) / (lhs_weight_f + rhs_weight_f)).clamp(0.0, 1.0)
    }

    fn ema_to_bp(value: f64) -> u16 {
        (value.clamp(0.0, 1.0) * 10_000.0).round() as u16
    }

    fn bp_to_ema(value: u16) -> f64 {
        (value as f64 / 10_000.0).clamp(0.0, 1.0)
    }

    fn controls_for(
        stats: &PolicyClusterLearningStats,
        adaptive: &ClusterAdaptiveState,
        kalman: &crate::engine::certainty_filter::CertaintyFilterState,
    ) -> ClusterAdaptiveControls {
        let decision_total = stats.decision_total();
        let outcome_total = stats.outcome_total();
        if decision_total < MIN_ADAPTIVE_DECISIONS {
            return ClusterAdaptiveControls::default();
        }

        let decision_ema = adaptive.decision_ema.clamp(0.0, 1.0);
        let outcome_ema = adaptive.outcome_ema.clamp(0.0, 1.0);
        let combined_ema = (decision_ema * 0.45) + (outcome_ema * 0.55);
        let revert_rate = if outcome_total == 0 {
            0.0
        } else {
            stats.reverted as f64 / outcome_total as f64
        };

        let (wilson_lower, wilson_upper) = Self::wilson_bounds(
            stats.accepted,
            stats.regressed.saturating_add(stats.reverted),
        );
        let (beta_lower, beta_upper) = Self::beta_bounds(
            stats.accepted,
            stats.regressed.saturating_add(stats.reverted),
        );

        let reliability_lower = wilson_lower.min(beta_lower);
        let reliability_upper = wilson_upper.max(beta_upper);
        let uncertainty = (reliability_upper - reliability_lower).clamp(0.0, 1.0);
        let stability_score = ((combined_ema * 0.55) + (reliability_lower * 0.45)).clamp(0.0, 1.0);

        ClusterAdaptiveControls {
            enforcement_bias: if stability_score >= kalman.cluster_relax_stability() && uncertainty < kalman.cluster_relax_uncertainty() {
                ClusterEnforcementBias::Relax
            } else if stability_score <= kalman.cluster_harden_stability() || uncertainty > kalman.cluster_harden_uncertainty() || revert_rate >= kalman.cluster_harden_revert_rate() {
                ClusterEnforcementBias::Harden
            } else {
                ClusterEnforcementBias::Neutral
            },
            max_impact_radius_cap: if stability_score < kalman.cluster_cap1_stability() || reliability_lower < kalman.cluster_cap1_reliability() {
                Some(1)
            } else if stability_score < kalman.cluster_cap3_stability() || uncertainty > kalman.cluster_cap3_uncertainty() {
                Some(3)
            } else {
                None
            },
        }
    }

    fn ema_step(current: f64, alpha: f64, sample: f64) -> f64 {
        let bounded_alpha = alpha.clamp(0.000_1, 1.0);
        (current + bounded_alpha * (sample - current)).clamp(0.0, 1.0)
    }

    fn ema_batch(current: f64, alpha: f64, sample_mean: f64, count: u64) -> f64 {
        if count == 0 {
            return current.clamp(0.0, 1.0);
        }
        let bounded_alpha = alpha.clamp(0.000_1, 1.0);
        let decay = (1.0 - bounded_alpha).powf(count as f64);
        ((current * decay) + (sample_mean * (1.0 - decay))).clamp(0.0, 1.0)
    }

    fn wilson_bounds(successes: u64, failures: u64) -> (f64, f64) {
        let total = successes.saturating_add(failures);
        if total == 0 {
            return (0.5, 0.5);
        }
        let n = total as f64;
        let phat = successes as f64 / n;
        let z2 = WILSON_Z * WILSON_Z;
        let denom = 1.0 + z2 / n;
        let center = (phat + z2 / (2.0 * n)) / denom;
        let margin = ((phat * (1.0 - phat) / n) + (z2 / (4.0 * n * n))).sqrt() * WILSON_Z / denom;
        (
            (center - margin).clamp(0.0, 1.0),
            (center + margin).clamp(0.0, 1.0),
        )
    }

    fn beta_bounds(successes: u64, failures: u64) -> (f64, f64) {
        let alpha = successes as f64 + 1.0;
        let beta = failures as f64 + 1.0;
        let mean = alpha / (alpha + beta);
        let variance = (alpha * beta) / (((alpha + beta).powi(2)) * (alpha + beta + 1.0));
        let std_dev = variance.sqrt();
        let margin = WILSON_Z * std_dev;
        (
            (mean - margin).clamp(0.0, 1.0),
            (mean + margin).clamp(0.0, 1.0),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use crate::engine::certainty_filter::CertaintyFilterState;
    use crate::engine::edit_candidate::PolicyDecisionOutcome;
    use crate::model::exec_trace::PolicyExecutionTrace;
    use crate::runtime::cluster_telemetry::{
        ClusterEnforcementBias, ClusterOutcome, PolicyClusterTelemetry,
    };

    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
        match GUARD.get_or_init(|| Mutex::new(())).lock() {
            Ok(value) => value,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    #[test]
    fn dedup_same_cluster() {
        let _guard = test_guard();
        PolicyClusterTelemetry::reset();
        let policy = format!("snake_case_{:?}", std::thread::current().id());
        let cluster = 9u64;
        let traces = vec![
            PolicyExecutionTrace {
                policy: policy.clone().into(),
                context_cluster: cluster,
                ..PolicyExecutionTrace::default()
            },
            PolicyExecutionTrace {
                policy: policy.clone().into(),
                context_cluster: cluster,
                ..PolicyExecutionTrace::default()
            },
        ];
        PolicyClusterTelemetry::record_outcome(traces.as_slice(), ClusterOutcome::Accepted);
        let entry =
            PolicyClusterTelemetry::snapshot_entry(policy.as_str(), cluster).unwrap_or_default();
        assert_eq!(entry.accepted, 1);
    }

    #[test]
    fn controls_relax_harden() {
        let _guard = test_guard();
        PolicyClusterTelemetry::reset();

        let stable_policy = format!("stable_policy_{:?}", std::thread::current().id());
        let stable_cluster = 111u64;
        for _ in 0..24 {
            PolicyClusterTelemetry::record_decision(
                stable_policy.as_str(),
                stable_cluster,
                PolicyDecisionOutcome::Apply,
            );
            PolicyClusterTelemetry::record_outcome(
                &[PolicyExecutionTrace {
                    policy: stable_policy.clone().into(),
                    context_cluster: stable_cluster,
                    ..PolicyExecutionTrace::default()
                }],
                ClusterOutcome::Accepted,
            );
        }
        let stable_controls =
            PolicyClusterTelemetry::adaptive_controls(stable_policy.as_str(), stable_cluster, &CertaintyFilterState::new());
        assert!(stable_controls.enforcement_bias != ClusterEnforcementBias::Harden);

        let unstable_policy = format!("unstable_policy_{:?}", std::thread::current().id());
        let unstable_cluster = 222u64;
        for _ in 0..24 {
            PolicyClusterTelemetry::record_decision(
                unstable_policy.as_str(),
                unstable_cluster,
                PolicyDecisionOutcome::Block,
            );
        }
        for _ in 0..10 {
            PolicyClusterTelemetry::record_outcome(
                &[PolicyExecutionTrace {
                    policy: unstable_policy.clone().into(),
                    context_cluster: unstable_cluster,
                    ..PolicyExecutionTrace::default()
                }],
                ClusterOutcome::Regressed,
            );
        }
        for _ in 0..10 {
            PolicyClusterTelemetry::record_outcome(
                &[PolicyExecutionTrace {
                    policy: unstable_policy.clone().into(),
                    context_cluster: unstable_cluster,
                    ..PolicyExecutionTrace::default()
                }],
                ClusterOutcome::Reverted,
            );
        }
        let unstable_controls =
            PolicyClusterTelemetry::adaptive_controls(unstable_policy.as_str(), unstable_cluster, &CertaintyFilterState::new());
        assert_eq!(
            unstable_controls.enforcement_bias,
            ClusterEnforcementBias::Harden
        );
        assert_eq!(unstable_controls.max_impact_radius_cap, Some(1));
    }

    #[test]
    fn hints_survive_reload() {
        let _guard = test_guard();
        PolicyClusterTelemetry::reset();

        let policy = format!("persisted_cluster_{:?}", std::thread::current().id());
        let cluster = 555u64;
        for _ in 0..18 {
            PolicyClusterTelemetry::record_decision(
                policy.as_str(),
                cluster,
                PolicyDecisionOutcome::Apply,
            );
            PolicyClusterTelemetry::record_outcome(
                &[PolicyExecutionTrace {
                    policy: policy.clone().into(),
                    context_cluster: cluster,
                    ..PolicyExecutionTrace::default()
                }],
                ClusterOutcome::Accepted,
            );
        }
        for _ in 0..3 {
            PolicyClusterTelemetry::record_decision(
                policy.as_str(),
                cluster,
                PolicyDecisionOutcome::Block,
            );
            PolicyClusterTelemetry::record_outcome(
                &[PolicyExecutionTrace {
                    policy: policy.clone().into(),
                    context_cluster: cluster,
                    ..PolicyExecutionTrace::default()
                }],
                ClusterOutcome::Regressed,
            );
        }

        let before = PolicyClusterTelemetry::adaptive_controls(policy.as_str(), cluster, &CertaintyFilterState::new());
        let snapshot = PolicyClusterTelemetry::snapshot_entries();
        PolicyClusterTelemetry::reset();
        PolicyClusterTelemetry::load_entries(snapshot.as_slice());
        let after = PolicyClusterTelemetry::adaptive_controls(policy.as_str(), cluster, &CertaintyFilterState::new());

        assert_eq!(before.enforcement_bias, after.enforcement_bias);
        assert_eq!(before.max_impact_radius_cap, after.max_impact_radius_cap);
    }

    #[test]
    fn read_model_freezes() {
        let _guard = test_guard();
        PolicyClusterTelemetry::reset();

        let policy = format!("deterministic_cluster_{:?}", std::thread::current().id());
        let cluster = 901u64;
        for _ in 0..20 {
            PolicyClusterTelemetry::record_decision(
                policy.as_str(),
                cluster,
                PolicyDecisionOutcome::Apply,
            );
            PolicyClusterTelemetry::record_outcome(
                &[PolicyExecutionTrace {
                    policy: policy.clone().into(),
                    context_cluster: cluster,
                    ..PolicyExecutionTrace::default()
                }],
                ClusterOutcome::Accepted,
            );
        }
        let baseline = PolicyClusterTelemetry::adaptive_controls(policy.as_str(), cluster, &CertaintyFilterState::new());
        let guard = PolicyClusterTelemetry::begin_read_model();

        for _ in 0..16 {
            PolicyClusterTelemetry::record_decision(
                policy.as_str(),
                cluster,
                PolicyDecisionOutcome::Block,
            );
            PolicyClusterTelemetry::record_outcome(
                &[PolicyExecutionTrace {
                    policy: policy.clone().into(),
                    context_cluster: cluster,
                    ..PolicyExecutionTrace::default()
                }],
                ClusterOutcome::Reverted,
            );
        }
        let frozen = PolicyClusterTelemetry::adaptive_controls(policy.as_str(), cluster, &CertaintyFilterState::new());
        assert_eq!(baseline.enforcement_bias, frozen.enforcement_bias);
        assert_eq!(baseline.max_impact_radius_cap, frozen.max_impact_radius_cap);

        drop(guard);
        let updated = PolicyClusterTelemetry::adaptive_controls(policy.as_str(), cluster, &CertaintyFilterState::new());
        assert_ne!(updated.enforcement_bias, baseline.enforcement_bias);
    }
}
