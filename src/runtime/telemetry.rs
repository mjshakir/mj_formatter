use std::sync::OnceLock;
use std::time::Duration;

use dashmap::DashMap;
use serde::{Deserialize, Serialize};

use crate::engine::gate_decision::ConfidenceReasonCode;
use crate::engine::edit_candidate::PolicyDecisionOutcome;
use crate::model::policy_name::PolicyName;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PolicyTelemetryEntry {
    pub runs: u64,
    pub failures: u64,
    pub fatals: u64,
    pub blocked: u64,
    pub confidence_decisions: u64,
    pub confidence_apply: u64,
    pub confidence_apply_partial: u64,
    pub confidence_advisory_only: u64,
    pub confidence_block: u64,
    pub reason_low_consensus: u64,
    pub reason_parser_consensus_strict: u64,
    pub reason_parser_consensus_adaptive_hardened: u64,
    pub reason_parser_consensus_adaptive_relaxed: u64,
    pub reason_context_coverage_low: u64,
    pub reason_semantic_consensus_low: u64,
    pub reason_parser_disagreement: u64,
    pub reason_clang_diagnostics: u64,
    pub total_elapsed_ns: u64,
    pub max_elapsed_ns: u64,
    pub total_edits: u64,
    pub total_violations: u64,
}

impl PolicyTelemetryEntry {
    pub fn avg_elapsed_ms(&self) -> f64 {
        if self.runs == 0 {
            return 0.0;
        }
        (self.total_elapsed_ns as f64 / self.runs as f64) / 1_000_000.0
    }

    pub fn total_elapsed_ms(&self) -> f64 {
        self.total_elapsed_ns as f64 / 1_000_000.0
    }
}

#[derive(Clone, Debug)]
pub struct PolicyExecutionSample {
    pub policy: PolicyName,
    pub elapsed: Duration,
    pub edits: usize,
    pub violations: usize,
    pub failed: bool,
    pub fatal: bool,
    pub blocked: bool,
    pub confidence: Option<PolicyConfidenceSample>,
}

#[derive(Clone, Debug)]
pub struct PolicyConfidenceSample {
    pub outcome: PolicyDecisionOutcome,
    pub low_consensus: bool,
    pub parser_consensus_strict: bool,
    pub parser_consensus_adaptive_hardened: bool,
    pub parser_consensus_adaptive_relaxed: bool,
    pub context_coverage_low: bool,
    pub semantic_consensus_low: bool,
    pub parser_disagreement: bool,
    pub clang_diagnostics: bool,
}

impl PolicyConfidenceSample {
    pub fn from_reason_codes(
        outcome: PolicyDecisionOutcome,
        reason_codes: &[ConfidenceReasonCode],
    ) -> Self {
        let mut sample = Self {
            outcome,
            low_consensus: false,
            parser_consensus_strict: false,
            parser_consensus_adaptive_hardened: false,
            parser_consensus_adaptive_relaxed: false,
            context_coverage_low: false,
            semantic_consensus_low: false,
            parser_disagreement: false,
            clang_diagnostics: false,
        };

        for reason in reason_codes {
            match reason {
                ConfidenceReasonCode::LowConsensus => sample.low_consensus = true,
                ConfidenceReasonCode::ParserConsensusStrict => {
                    sample.parser_consensus_strict = true
                }
                ConfidenceReasonCode::ParserConsensusAdaptiveHardened => {
                    sample.parser_consensus_adaptive_hardened = true
                }
                ConfidenceReasonCode::ParserConsensusAdaptiveRelaxed => {
                    sample.parser_consensus_adaptive_relaxed = true
                }
                ConfidenceReasonCode::ContextCoverageLow => sample.context_coverage_low = true,
                ConfidenceReasonCode::SemanticConsensusLow => sample.semantic_consensus_low = true,
                ConfidenceReasonCode::ParserDisagreement => sample.parser_disagreement = true,
                ConfidenceReasonCode::ClangDiagnostics => sample.clang_diagnostics = true,
                _ => {}
            }
        }
        sample
    }
}

impl PolicyExecutionSample {
    pub fn success(policy: &str, elapsed: Duration, edits: usize, violations: usize) -> Self {
        Self {
            policy: policy.into(),
            elapsed,
            edits,
            violations,
            failed: false,
            fatal: false,
            blocked: false,
            confidence: None,
        }
    }

    pub fn blocked(policy: &str, elapsed: Duration) -> Self {
        Self {
            policy: policy.into(),
            elapsed,
            edits: 0,
            violations: 0,
            failed: false,
            fatal: false,
            blocked: true,
            confidence: None,
        }
    }

    pub fn failed(policy: &str, elapsed: Duration, fatal: bool) -> Self {
        Self {
            policy: policy.into(),
            elapsed,
            edits: 0,
            violations: 0,
            failed: true,
            fatal,
            blocked: false,
            confidence: None,
        }
    }

    pub fn with_confidence(mut self, confidence: PolicyConfidenceSample) -> Self {
        self.confidence = Some(confidence);
        self
    }
}

fn state() -> &'static DashMap<PolicyName, PolicyTelemetryEntry> {
    static STATE: OnceLock<DashMap<PolicyName, PolicyTelemetryEntry>> = OnceLock::new();
    STATE.get_or_init(DashMap::new)
}

pub struct PolicyTelemetry;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PolicyTelemetrySnapshotEntry {
    pub policy: PolicyName,
    pub entry: PolicyTelemetryEntry,
}

impl PolicyTelemetry {
    pub fn reset() {
        state().clear();
    }

    pub fn record_batch(samples: &[PolicyExecutionSample]) {
        if samples.is_empty() {
            return;
        }
        let map = state();
        for sample in samples {
            let mut entry = map.entry(sample.policy.clone()).or_default();
            entry.runs = entry.runs.saturating_add(1);
            entry.total_elapsed_ns = entry
                .total_elapsed_ns
                .saturating_add(sample.elapsed.as_nanos() as u64);
            entry.max_elapsed_ns = entry.max_elapsed_ns.max(sample.elapsed.as_nanos() as u64);
            entry.total_edits = entry.total_edits.saturating_add(sample.edits as u64);
            entry.total_violations = entry
                .total_violations
                .saturating_add(sample.violations as u64);
            if sample.failed {
                entry.failures = entry.failures.saturating_add(1);
            }
            if sample.fatal {
                entry.fatals = entry.fatals.saturating_add(1);
            }
            if sample.blocked {
                entry.blocked = entry.blocked.saturating_add(1);
            }
            if let Some(confidence) = &sample.confidence {
                entry.confidence_decisions = entry.confidence_decisions.saturating_add(1);
                match confidence.outcome {
                    PolicyDecisionOutcome::Apply => {
                        entry.confidence_apply = entry.confidence_apply.saturating_add(1)
                    }
                    PolicyDecisionOutcome::ApplyPartial => {
                        entry.confidence_apply_partial =
                            entry.confidence_apply_partial.saturating_add(1)
                    }
                    PolicyDecisionOutcome::Block => {
                        entry.confidence_block = entry.confidence_block.saturating_add(1)
                    }
                }
                if confidence.low_consensus {
                    entry.reason_low_consensus = entry.reason_low_consensus.saturating_add(1);
                }
                if confidence.parser_consensus_strict {
                    entry.reason_parser_consensus_strict =
                        entry.reason_parser_consensus_strict.saturating_add(1);
                }
                if confidence.parser_consensus_adaptive_hardened {
                    entry.reason_parser_consensus_adaptive_hardened = entry
                        .reason_parser_consensus_adaptive_hardened
                        .saturating_add(1);
                }
                if confidence.parser_consensus_adaptive_relaxed {
                    entry.reason_parser_consensus_adaptive_relaxed = entry
                        .reason_parser_consensus_adaptive_relaxed
                        .saturating_add(1);
                }
                if confidence.context_coverage_low {
                    entry.reason_context_coverage_low =
                        entry.reason_context_coverage_low.saturating_add(1);
                }
                if confidence.semantic_consensus_low {
                    entry.reason_semantic_consensus_low =
                        entry.reason_semantic_consensus_low.saturating_add(1);
                }
                if confidence.parser_disagreement {
                    entry.reason_parser_disagreement =
                        entry.reason_parser_disagreement.saturating_add(1);
                }
                if confidence.clang_diagnostics {
                    entry.reason_clang_diagnostics =
                        entry.reason_clang_diagnostics.saturating_add(1);
                }
            }
        }
    }

    pub fn snapshot_sorted() -> Vec<PolicyTelemetrySnapshotEntry> {
        let map = state();
        let mut items: Vec<_> = map
            .iter()
            .map(|entry| PolicyTelemetrySnapshotEntry {
                policy: entry.key().clone(),
                entry: entry.value().clone(),
            })
            .collect();
        items.sort_by(|left, right| {
            right
                .entry
                .total_elapsed_ns
                .cmp(&left.entry.total_elapsed_ns)
                .then(right.entry.failures.cmp(&left.entry.failures))
                .then(left.policy.cmp(&right.policy))
        });
        items
    }

    #[cfg(test)]
    pub fn snapshot_map() -> std::collections::HashMap<String, PolicyTelemetryEntry> {
        state()
            .iter()
            .map(|entry| (entry.key().to_string(), entry.value().clone()))
            .collect()
    }

    pub fn merge_entries(entries: &[PolicyTelemetrySnapshotEntry]) {
        if entries.is_empty() {
            return;
        }
        let map = state();
        for item in entries {
            let mut entry = map.entry(item.policy.clone()).or_default();
            entry.runs = entry.runs.saturating_add(item.entry.runs);
            entry.failures = entry.failures.saturating_add(item.entry.failures);
            entry.fatals = entry.fatals.saturating_add(item.entry.fatals);
            entry.blocked = entry.blocked.saturating_add(item.entry.blocked);
            entry.confidence_decisions = entry
                .confidence_decisions
                .saturating_add(item.entry.confidence_decisions);
            entry.confidence_apply = entry
                .confidence_apply
                .saturating_add(item.entry.confidence_apply);
            entry.confidence_apply_partial = entry
                .confidence_apply_partial
                .saturating_add(item.entry.confidence_apply_partial);
            entry.confidence_advisory_only = entry
                .confidence_advisory_only
                .saturating_add(item.entry.confidence_advisory_only);
            entry.confidence_block = entry
                .confidence_block
                .saturating_add(item.entry.confidence_block);
            entry.reason_low_consensus = entry
                .reason_low_consensus
                .saturating_add(item.entry.reason_low_consensus);
            entry.reason_parser_consensus_strict = entry
                .reason_parser_consensus_strict
                .saturating_add(item.entry.reason_parser_consensus_strict);
            entry.reason_parser_consensus_adaptive_hardened = entry
                .reason_parser_consensus_adaptive_hardened
                .saturating_add(item.entry.reason_parser_consensus_adaptive_hardened);
            entry.reason_parser_consensus_adaptive_relaxed = entry
                .reason_parser_consensus_adaptive_relaxed
                .saturating_add(item.entry.reason_parser_consensus_adaptive_relaxed);
            entry.reason_context_coverage_low = entry
                .reason_context_coverage_low
                .saturating_add(item.entry.reason_context_coverage_low);
            entry.reason_semantic_consensus_low = entry
                .reason_semantic_consensus_low
                .saturating_add(item.entry.reason_semantic_consensus_low);
            entry.reason_parser_disagreement = entry
                .reason_parser_disagreement
                .saturating_add(item.entry.reason_parser_disagreement);
            entry.reason_clang_diagnostics = entry
                .reason_clang_diagnostics
                .saturating_add(item.entry.reason_clang_diagnostics);
            entry.total_elapsed_ns = entry
                .total_elapsed_ns
                .saturating_add(item.entry.total_elapsed_ns);
            entry.max_elapsed_ns = entry.max_elapsed_ns.max(item.entry.max_elapsed_ns);
            entry.total_edits = entry.total_edits.saturating_add(item.entry.total_edits);
            entry.total_violations = entry
                .total_violations
                .saturating_add(item.entry.total_violations);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    use crate::engine::gate_decision::ConfidenceReasonCode;
    use crate::engine::edit_candidate::PolicyDecisionOutcome;
    use crate::runtime::telemetry::{
        PolicyConfidenceSample, PolicyExecutionSample, PolicyTelemetry,
        PolicyTelemetrySnapshotEntry,
    };

    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
        match GUARD.get_or_init(|| Mutex::new(())).lock() {
            Ok(value) => value,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    #[test]
    fn records_and_sorts_policy_telemetry() {
        let _guard = test_guard();
        let suffix = format!("{:?}", std::thread::current().id());
        let policy_a = format!("policy_a_records_and_sorts_{suffix}");
        let policy_b = format!("policy_b_records_and_sorts_{suffix}");
        PolicyTelemetry::reset();
        PolicyTelemetry::record_batch(&vec![
            PolicyExecutionSample::success(policy_a.as_str(), Duration::from_millis(8), 3, 1),
            PolicyExecutionSample::success(policy_a.as_str(), Duration::from_millis(2), 1, 0)
                .with_confidence(PolicyConfidenceSample::from_reason_codes(
                    PolicyDecisionOutcome::ApplyPartial,
                    &[
                        ConfidenceReasonCode::ParserDisagreement,
                        ConfidenceReasonCode::ClangDiagnostics,
                    ],
                )),
            PolicyExecutionSample::failed(policy_b.as_str(), Duration::from_millis(1), true),
        ]);

        let snapshot = PolicyTelemetry::snapshot_map();
        let entry_a = snapshot
            .get(policy_a.as_str())
            .expect("policy_a telemetry entry");
        assert_eq!(entry_a.runs, 2);
        assert_eq!(entry_a.total_edits, 4);
        assert_eq!(entry_a.confidence_decisions, 1);
        assert_eq!(entry_a.confidence_apply_partial, 1);
        assert_eq!(entry_a.reason_parser_disagreement, 1);
        assert_eq!(entry_a.reason_clang_diagnostics, 1);
        let entry_b = snapshot
            .get(policy_b.as_str())
            .expect("policy_b telemetry entry");
        assert_eq!(entry_b.failures, 1);
        assert_eq!(entry_b.fatals, 1);
    }

    #[test]
    fn merges_entries() {
        let _guard = test_guard();
        let suffix = format!("{:?}", std::thread::current().id());
        let policy_a = format!("policy_a_merges_entries_{suffix}");
        PolicyTelemetry::reset();
        PolicyTelemetry::merge_entries(&[
            PolicyTelemetrySnapshotEntry {
                policy: policy_a.clone().into(),
                entry: crate::runtime::telemetry::PolicyTelemetryEntry {
                    runs: 2,
                    failures: 1,
                    fatals: 0,
                    blocked: 1,
                    confidence_decisions: 2,
                    confidence_apply: 1,
                    confidence_apply_partial: 1,
                    confidence_advisory_only: 0,
                    confidence_block: 0,
                    reason_low_consensus: 1,
                    reason_parser_consensus_strict: 1,
                    reason_parser_consensus_adaptive_hardened: 0,
                    reason_parser_consensus_adaptive_relaxed: 0,
                    reason_context_coverage_low: 0,
                    reason_semantic_consensus_low: 0,
                    reason_parser_disagreement: 1,
                    reason_clang_diagnostics: 0,
                    total_elapsed_ns: 20,
                    max_elapsed_ns: 15,
                    total_edits: 3,
                    total_violations: 2,
                },
            },
            PolicyTelemetrySnapshotEntry {
                policy: policy_a.clone().into(),
                entry: crate::runtime::telemetry::PolicyTelemetryEntry {
                    runs: 3,
                    failures: 0,
                    fatals: 1,
                    blocked: 0,
                    confidence_decisions: 1,
                    confidence_apply: 0,
                    confidence_apply_partial: 0,
                    confidence_advisory_only: 1,
                    confidence_block: 0,
                    reason_low_consensus: 0,
                    reason_parser_consensus_strict: 0,
                    reason_parser_consensus_adaptive_hardened: 0,
                    reason_parser_consensus_adaptive_relaxed: 1,
                    reason_context_coverage_low: 1,
                    reason_semantic_consensus_low: 0,
                    reason_parser_disagreement: 0,
                    reason_clang_diagnostics: 1,
                    total_elapsed_ns: 10,
                    max_elapsed_ns: 5,
                    total_edits: 4,
                    total_violations: 1,
                },
            },
        ]);
        let snapshot = PolicyTelemetry::snapshot_map();
        let entry = snapshot
            .get(policy_a.as_str())
            .expect("merged telemetry entry");
        assert_eq!(entry.runs, 5);
        assert_eq!(entry.failures, 1);
        assert_eq!(entry.fatals, 1);
        assert_eq!(entry.blocked, 1);
        assert_eq!(entry.confidence_decisions, 3);
        assert_eq!(entry.confidence_apply, 1);
        assert_eq!(entry.confidence_apply_partial, 1);
        assert_eq!(entry.confidence_advisory_only, 1);
        assert_eq!(entry.reason_low_consensus, 1);
        assert_eq!(entry.reason_parser_consensus_strict, 1);
        assert_eq!(entry.reason_parser_consensus_adaptive_relaxed, 1);
        assert_eq!(entry.reason_context_coverage_low, 1);
        assert_eq!(entry.reason_parser_disagreement, 1);
        assert_eq!(entry.reason_clang_diagnostics, 1);
        assert_eq!(entry.total_elapsed_ns, 30);
        assert_eq!(entry.max_elapsed_ns, 15);
        assert_eq!(entry.total_edits, 7);
        assert_eq!(entry.total_violations, 3);
    }
}
