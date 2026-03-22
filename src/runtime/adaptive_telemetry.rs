use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::engine::gate_decision::ConfidenceReasonCode;
use crate::engine::edit_candidate::PolicyDecisionOutcome;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename = "AdaptiveTelemetrySnapshot")]
pub struct AdaptiveSnapshot {
    pub threshold_evaluations: u64,
    pub threshold_applied: u64,
    pub threshold_canary: u64,
    pub threshold_suspended: u64,
    pub confidence_decisions: u64,
    pub confidence_apply: u64,
    pub confidence_apply_partial: u64,
    pub confidence_advisory_only: u64,
    pub confidence_block: u64,
    pub reason_low_consensus: u64,
    #[serde(alias = "reason_parser_consensus_strict")]
    pub reason_parser_strict: u64,
    #[serde(alias = "reason_parser_consensus_adaptive_hardened")]
    pub reason_parser_hardened: u64,
    #[serde(alias = "reason_parser_consensus_adaptive_relaxed")]
    pub reason_parser_relaxed: u64,
    #[serde(alias = "reason_context_coverage_low")]
    pub reason_coverage_low: u64,
    #[serde(alias = "reason_semantic_consensus_low")]
    pub reason_semantic_low: u64,
    pub reason_parser_disagreement: u64,
    pub reason_clang_diagnostics: u64,
    pub outcomes_first_pass: u64,
    pub outcomes_after_retry: u64,
    pub outcomes_reverted: u64,
    pub rollback_events: u64,
    pub last_threshold: f64,
    pub last_delta: f64,
    pub last_ema_failure_rate: f64,
    pub last_ema_revert_rate: f64,
    pub last_drift: f64,
    pub max_abs_drift: f64,
}

impl AdaptiveSnapshot {
    pub fn outcomes_total(&self) -> u64 {
        self.outcomes_first_pass
            .saturating_add(self.outcomes_after_retry)
            .saturating_add(self.outcomes_reverted)
    }
}

#[derive(Debug)]
struct AdaptiveTelemetryState {
    threshold_evaluations: AtomicU64,
    threshold_applied: AtomicU64,
    threshold_canary: AtomicU64,
    threshold_suspended: AtomicU64,
    confidence_decisions: AtomicU64,
    confidence_apply: AtomicU64,
    confidence_apply_partial: AtomicU64,
    confidence_advisory_only: AtomicU64,
    confidence_block: AtomicU64,
    reason_low_consensus: AtomicU64,
    reason_parser_strict: AtomicU64,
    reason_parser_hardened: AtomicU64,
    reason_parser_relaxed: AtomicU64,
    reason_coverage_low: AtomicU64,
    reason_semantic_low: AtomicU64,
    reason_parser_disagreement: AtomicU64,
    reason_clang_diagnostics: AtomicU64,
    outcomes_first_pass: AtomicU64,
    outcomes_after_retry: AtomicU64,
    outcomes_reverted: AtomicU64,
    rollback_events: AtomicU64,
    last_threshold_bits: AtomicU64,
    last_delta_bits: AtomicU64,
    last_ema_failure_bits: AtomicU64,
    last_ema_revert_bits: AtomicU64,
    last_drift_bits: AtomicU64,
    max_abs_drift_bits: AtomicU64,
}

impl Default for AdaptiveTelemetryState {
    fn default() -> Self {
        Self {
            threshold_evaluations: AtomicU64::new(0),
            threshold_applied: AtomicU64::new(0),
            threshold_canary: AtomicU64::new(0),
            threshold_suspended: AtomicU64::new(0),
            confidence_decisions: AtomicU64::new(0),
            confidence_apply: AtomicU64::new(0),
            confidence_apply_partial: AtomicU64::new(0),
            confidence_advisory_only: AtomicU64::new(0),
            confidence_block: AtomicU64::new(0),
            reason_low_consensus: AtomicU64::new(0),
            reason_parser_strict: AtomicU64::new(0),
            reason_parser_hardened: AtomicU64::new(0),
            reason_parser_relaxed: AtomicU64::new(0),
            reason_coverage_low: AtomicU64::new(0),
            reason_semantic_low: AtomicU64::new(0),
            reason_parser_disagreement: AtomicU64::new(0),
            reason_clang_diagnostics: AtomicU64::new(0),
            outcomes_first_pass: AtomicU64::new(0),
            outcomes_after_retry: AtomicU64::new(0),
            outcomes_reverted: AtomicU64::new(0),
            rollback_events: AtomicU64::new(0),
            last_threshold_bits: AtomicU64::new(0),
            last_delta_bits: AtomicU64::new(0),
            last_ema_failure_bits: AtomicU64::new(0),
            last_ema_revert_bits: AtomicU64::new(0),
            last_drift_bits: AtomicU64::new(0),
            max_abs_drift_bits: AtomicU64::new(0),
        }
    }
}

pub struct AdaptiveTelemetry;

impl AdaptiveTelemetry {
    pub fn reset() {
        let state = Self::state();
        state.threshold_evaluations.store(0, Ordering::Relaxed);
        state.threshold_applied.store(0, Ordering::Relaxed);
        state.threshold_canary.store(0, Ordering::Relaxed);
        state.threshold_suspended.store(0, Ordering::Relaxed);
        state.confidence_decisions.store(0, Ordering::Relaxed);
        state.confidence_apply.store(0, Ordering::Relaxed);
        state.confidence_apply_partial.store(0, Ordering::Relaxed);
        state.confidence_advisory_only.store(0, Ordering::Relaxed);
        state.confidence_block.store(0, Ordering::Relaxed);
        state.reason_low_consensus.store(0, Ordering::Relaxed);
        state
            .reason_parser_strict
            .store(0, Ordering::Relaxed);
        state
            .reason_parser_hardened
            .store(0, Ordering::Relaxed);
        state
            .reason_parser_relaxed
            .store(0, Ordering::Relaxed);
        state
            .reason_coverage_low
            .store(0, Ordering::Relaxed);
        state
            .reason_semantic_low
            .store(0, Ordering::Relaxed);
        state.reason_parser_disagreement.store(0, Ordering::Relaxed);
        state.reason_clang_diagnostics.store(0, Ordering::Relaxed);
        state.outcomes_first_pass.store(0, Ordering::Relaxed);
        state.outcomes_after_retry.store(0, Ordering::Relaxed);
        state.outcomes_reverted.store(0, Ordering::Relaxed);
        state.rollback_events.store(0, Ordering::Relaxed);
        state.last_threshold_bits.store(0, Ordering::Relaxed);
        state.last_delta_bits.store(0, Ordering::Relaxed);
        state.last_ema_failure_bits.store(0, Ordering::Relaxed);
        state.last_ema_revert_bits.store(0, Ordering::Relaxed);
        state.last_drift_bits.store(0, Ordering::Relaxed);
        state.max_abs_drift_bits.store(0, Ordering::Relaxed);
    }

    pub fn record_confidence_gate(
        outcome: PolicyDecisionOutcome,
        reason_codes: &[ConfidenceReasonCode],
    ) {
        let state = Self::state();
        state.confidence_decisions.fetch_add(1, Ordering::Relaxed);
        match outcome {
            PolicyDecisionOutcome::Apply => {
                state.confidence_apply.fetch_add(1, Ordering::Relaxed);
            }
            PolicyDecisionOutcome::ApplyPartial => {
                state
                    .confidence_apply_partial
                    .fetch_add(1, Ordering::Relaxed);
            }
            PolicyDecisionOutcome::Block => {
                state.confidence_block.fetch_add(1, Ordering::Relaxed);
            }
        }
        for code in reason_codes {
            match code {
                ConfidenceReasonCode::LowConsensus => {
                    state.reason_low_consensus.fetch_add(1, Ordering::Relaxed);
                }
                ConfidenceReasonCode::ParserConsensusStrict => {
                    state
                        .reason_parser_strict
                        .fetch_add(1, Ordering::Relaxed);
                }
                ConfidenceReasonCode::ParserHardened => {
                    state
                        .reason_parser_hardened
                        .fetch_add(1, Ordering::Relaxed);
                }
                ConfidenceReasonCode::ParserRelaxed => {
                    state
                        .reason_parser_relaxed
                        .fetch_add(1, Ordering::Relaxed);
                }
                ConfidenceReasonCode::ContextCoverageLow => {
                    state
                        .reason_coverage_low
                        .fetch_add(1, Ordering::Relaxed);
                }
                ConfidenceReasonCode::SemanticConsensusLow => {
                    state
                        .reason_semantic_low
                        .fetch_add(1, Ordering::Relaxed);
                }
                ConfidenceReasonCode::ParserDisagreement => {
                    state
                        .reason_parser_disagreement
                        .fetch_add(1, Ordering::Relaxed);
                }
                ConfidenceReasonCode::ClangDiagnostics => {
                    state
                        .reason_clang_diagnostics
                        .fetch_add(1, Ordering::Relaxed);
                }
                _ => {}
            }
        }
    }

    pub fn snapshot() -> AdaptiveSnapshot {
        let state = Self::state();
        AdaptiveSnapshot {
            threshold_evaluations: state.threshold_evaluations.load(Ordering::Relaxed),
            threshold_applied: state.threshold_applied.load(Ordering::Relaxed),
            threshold_canary: state.threshold_canary.load(Ordering::Relaxed),
            threshold_suspended: state.threshold_suspended.load(Ordering::Relaxed),
            confidence_decisions: state.confidence_decisions.load(Ordering::Relaxed),
            confidence_apply: state.confidence_apply.load(Ordering::Relaxed),
            confidence_apply_partial: state.confidence_apply_partial.load(Ordering::Relaxed),
            confidence_advisory_only: state.confidence_advisory_only.load(Ordering::Relaxed),
            confidence_block: state.confidence_block.load(Ordering::Relaxed),
            reason_low_consensus: state.reason_low_consensus.load(Ordering::Relaxed),
            reason_parser_strict: state
                .reason_parser_strict
                .load(Ordering::Relaxed),
            reason_parser_hardened: state
                .reason_parser_hardened
                .load(Ordering::Relaxed),
            reason_parser_relaxed: state
                .reason_parser_relaxed
                .load(Ordering::Relaxed),
            reason_coverage_low: state.reason_coverage_low.load(Ordering::Relaxed),
            reason_semantic_low: state
                .reason_semantic_low
                .load(Ordering::Relaxed),
            reason_parser_disagreement: state.reason_parser_disagreement.load(Ordering::Relaxed),
            reason_clang_diagnostics: state.reason_clang_diagnostics.load(Ordering::Relaxed),
            outcomes_first_pass: state.outcomes_first_pass.load(Ordering::Relaxed),
            outcomes_after_retry: state.outcomes_after_retry.load(Ordering::Relaxed),
            outcomes_reverted: state.outcomes_reverted.load(Ordering::Relaxed),
            rollback_events: state.rollback_events.load(Ordering::Relaxed),
            last_threshold: Self::bits_to_f64(state.last_threshold_bits.load(Ordering::Relaxed)),
            last_delta: Self::bits_to_f64(state.last_delta_bits.load(Ordering::Relaxed)),
            last_ema_failure_rate: Self::bits_to_f64(
                state.last_ema_failure_bits.load(Ordering::Relaxed),
            ),
            last_ema_revert_rate: Self::bits_to_f64(
                state.last_ema_revert_bits.load(Ordering::Relaxed),
            ),
            last_drift: Self::bits_to_f64(state.last_drift_bits.load(Ordering::Relaxed)),
            max_abs_drift: Self::bits_to_f64(state.max_abs_drift_bits.load(Ordering::Relaxed)),
        }
    }

    pub fn merge_snapshot(snapshot: &AdaptiveSnapshot) {
        let state = Self::state();
        state
            .threshold_evaluations
            .fetch_add(snapshot.threshold_evaluations, Ordering::Relaxed);
        state
            .threshold_applied
            .fetch_add(snapshot.threshold_applied, Ordering::Relaxed);
        state
            .threshold_canary
            .fetch_add(snapshot.threshold_canary, Ordering::Relaxed);
        state
            .threshold_suspended
            .fetch_add(snapshot.threshold_suspended, Ordering::Relaxed);
        state
            .confidence_decisions
            .fetch_add(snapshot.confidence_decisions, Ordering::Relaxed);
        state
            .confidence_apply
            .fetch_add(snapshot.confidence_apply, Ordering::Relaxed);
        state
            .confidence_apply_partial
            .fetch_add(snapshot.confidence_apply_partial, Ordering::Relaxed);
        state
            .confidence_advisory_only
            .fetch_add(snapshot.confidence_advisory_only, Ordering::Relaxed);
        state
            .confidence_block
            .fetch_add(snapshot.confidence_block, Ordering::Relaxed);
        state
            .reason_low_consensus
            .fetch_add(snapshot.reason_low_consensus, Ordering::Relaxed);
        state
            .reason_parser_strict
            .fetch_add(snapshot.reason_parser_strict, Ordering::Relaxed);
        state.reason_parser_hardened.fetch_add(
            snapshot.reason_parser_hardened,
            Ordering::Relaxed,
        );
        state.reason_parser_relaxed.fetch_add(
            snapshot.reason_parser_relaxed,
            Ordering::Relaxed,
        );
        state
            .reason_coverage_low
            .fetch_add(snapshot.reason_coverage_low, Ordering::Relaxed);
        state
            .reason_semantic_low
            .fetch_add(snapshot.reason_semantic_low, Ordering::Relaxed);
        state
            .reason_parser_disagreement
            .fetch_add(snapshot.reason_parser_disagreement, Ordering::Relaxed);
        state
            .reason_clang_diagnostics
            .fetch_add(snapshot.reason_clang_diagnostics, Ordering::Relaxed);
        state
            .outcomes_first_pass
            .fetch_add(snapshot.outcomes_first_pass, Ordering::Relaxed);
        state
            .outcomes_after_retry
            .fetch_add(snapshot.outcomes_after_retry, Ordering::Relaxed);
        state
            .outcomes_reverted
            .fetch_add(snapshot.outcomes_reverted, Ordering::Relaxed);
        state
            .rollback_events
            .fetch_add(snapshot.rollback_events, Ordering::Relaxed);

        if snapshot.threshold_evaluations > 0 || snapshot.outcomes_total() > 0 {
            state.last_threshold_bits.store(
                Self::f64_to_bits(snapshot.last_threshold),
                Ordering::Relaxed,
            );
            state
                .last_delta_bits
                .store(Self::f64_to_bits(snapshot.last_delta), Ordering::Relaxed);
            state.last_ema_failure_bits.store(
                Self::f64_to_bits(snapshot.last_ema_failure_rate),
                Ordering::Relaxed,
            );
            state.last_ema_revert_bits.store(
                Self::f64_to_bits(snapshot.last_ema_revert_rate),
                Ordering::Relaxed,
            );
            state
                .last_drift_bits
                .store(Self::f64_to_bits(snapshot.last_drift), Ordering::Relaxed);
        }
        let mut current = state.max_abs_drift_bits.load(Ordering::Relaxed);
        let candidate = snapshot.max_abs_drift.abs();
        let candidate_bits = Self::f64_to_bits(candidate);
        loop {
            if Self::bits_to_f64(current) >= candidate {
                break;
            }
            match state.max_abs_drift_bits.compare_exchange_weak(
                current,
                candidate_bits,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
    }

    fn state() -> &'static AdaptiveTelemetryState {
        static STATE: OnceLock<AdaptiveTelemetryState> = OnceLock::new();
        STATE.get_or_init(AdaptiveTelemetryState::default)
    }

    fn f64_to_bits(value: f64) -> u64 {
        if value.is_finite() {
            value.to_bits()
        } else {
            0.0f64.to_bits()
        }
    }

    fn bits_to_f64(bits: u64) -> f64 {
        f64::from_bits(bits)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use crate::engine::gate_decision::ConfidenceReasonCode;
    use crate::engine::edit_candidate::PolicyDecisionOutcome;

    use super::AdaptiveTelemetry;

    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
        match GUARD.get_or_init(|| Mutex::new(())).lock() {
            Ok(value) => value,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    #[test]
    fn merges_across_workers() {
        let _guard = test_guard();
        AdaptiveTelemetry::reset();
        AdaptiveTelemetry::merge_snapshot(&super::AdaptiveSnapshot {
            threshold_evaluations: 2,
            threshold_applied: 1,
            threshold_canary: 1,
            threshold_suspended: 0,
            confidence_decisions: 3,
            confidence_apply: 1,
            confidence_apply_partial: 1,
            confidence_advisory_only: 0,
            confidence_block: 1,
            reason_low_consensus: 2,
            reason_parser_strict: 1,
            reason_parser_hardened: 1,
            reason_parser_relaxed: 0,
            reason_coverage_low: 1,
            reason_semantic_low: 0,
            reason_parser_disagreement: 1,
            reason_clang_diagnostics: 1,
            outcomes_first_pass: 3,
            outcomes_after_retry: 2,
            outcomes_reverted: 1,
            rollback_events: 1,
            last_threshold: 0.73,
            last_delta: 0.02,
            last_ema_failure_rate: 0.15,
            last_ema_revert_rate: 0.05,
            last_drift: -0.03,
            max_abs_drift: 0.09,
        });
        AdaptiveTelemetry::merge_snapshot(&super::AdaptiveSnapshot {
            threshold_evaluations: 1,
            threshold_applied: 1,
            threshold_canary: 0,
            threshold_suspended: 0,
            confidence_decisions: 2,
            confidence_apply: 1,
            confidence_apply_partial: 0,
            confidence_advisory_only: 0,
            confidence_block: 0,
            reason_low_consensus: 1,
            reason_parser_strict: 0,
            reason_parser_hardened: 0,
            reason_parser_relaxed: 1,
            reason_coverage_low: 0,
            reason_semantic_low: 1,
            reason_parser_disagreement: 0,
            reason_clang_diagnostics: 0,
            outcomes_first_pass: 1,
            outcomes_after_retry: 0,
            outcomes_reverted: 1,
            rollback_events: 2,
            last_threshold: 0.74,
            last_delta: 0.03,
            last_ema_failure_rate: 0.20,
            last_ema_revert_rate: 0.10,
            last_drift: 0.04,
            max_abs_drift: 0.12,
        });
        let snapshot = AdaptiveTelemetry::snapshot();
        assert!(snapshot.threshold_evaluations >= 3);
        assert!(snapshot.threshold_applied >= 2);
        assert!(snapshot.threshold_canary >= 1);
        assert!(snapshot.confidence_decisions >= 5);
        assert!(snapshot.confidence_apply >= 2);
        assert!(snapshot.confidence_apply_partial >= 1);
        assert_eq!(snapshot.confidence_advisory_only, 0);
        assert!(snapshot.confidence_block >= 1);
        assert!(snapshot.reason_low_consensus >= 3);
        assert!(snapshot.reason_parser_strict >= 1);
        assert!(snapshot.reason_parser_hardened >= 1);
        assert!(snapshot.reason_parser_relaxed >= 1);
        assert!(snapshot.reason_coverage_low >= 1);
        assert!(snapshot.reason_semantic_low >= 1);
        assert!(snapshot.reason_parser_disagreement >= 1);
        assert!(snapshot.reason_clang_diagnostics >= 1);
        assert!(snapshot.outcomes_first_pass >= 4);
        assert!(snapshot.outcomes_after_retry >= 2);
        assert!(snapshot.outcomes_reverted >= 2);
        assert!(snapshot.rollback_events >= 3);
        assert!(snapshot.max_abs_drift >= 0.12);
    }

    #[test]
    fn tracks_gate_outcomes() {
        let _guard = test_guard();
        AdaptiveTelemetry::reset();
        AdaptiveTelemetry::record_confidence_gate(
            PolicyDecisionOutcome::ApplyPartial,
            &[
                ConfidenceReasonCode::LowConsensus,
                ConfidenceReasonCode::ParserHardened,
                ConfidenceReasonCode::ContextCoverageLow,
                ConfidenceReasonCode::SemanticConsensusLow,
            ],
        );
        AdaptiveTelemetry::record_confidence_gate(
            PolicyDecisionOutcome::Block,
            &[
                ConfidenceReasonCode::ParserRelaxed,
                ConfidenceReasonCode::ParserDisagreement,
                ConfidenceReasonCode::ClangDiagnostics,
            ],
        );

        let snapshot = AdaptiveTelemetry::snapshot();
        assert_eq!(snapshot.confidence_decisions, 2);
        assert_eq!(snapshot.confidence_apply, 0);
        assert_eq!(snapshot.confidence_apply_partial, 1);
        assert_eq!(snapshot.confidence_advisory_only, 0);
        assert_eq!(snapshot.confidence_block, 1);
        assert_eq!(snapshot.reason_low_consensus, 1);
        assert_eq!(snapshot.reason_parser_strict, 0);
        assert_eq!(snapshot.reason_parser_hardened, 1);
        assert_eq!(snapshot.reason_parser_relaxed, 1);
        assert_eq!(snapshot.reason_coverage_low, 1);
        assert_eq!(snapshot.reason_semantic_low, 1);
        assert_eq!(snapshot.reason_parser_disagreement, 1);
        assert_eq!(snapshot.reason_clang_diagnostics, 1);
    }
}
