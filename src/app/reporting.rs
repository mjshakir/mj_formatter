use crate::app::runner::{AccuracyGateRolloutSignal, App};
use crate::engine::accuracy_gate::AccuracyGateReason;
use crate::model::file_result::FileResult;
use crate::runtime::rollout_state::AccuracyObservationSource;

impl App {
    pub(crate) fn collect_accuracy_gate_rollout_signal(
        results: &[FileResult],
    ) -> Option<AccuracyGateRolloutSignal> {
        let mut considered = 0usize;
        let mut failing = 0usize;
        let mut semantic_required_unmet = 0usize;
        let mut precision_sum = 0.0f64;
        let mut recall_sum = 0.0f64;
        let mut pass_count = 0usize;

        for result in results {
            if let Some(decision) = result.outcome.accuracy_gate.as_ref() {
                considered = considered.saturating_add(1);
                failing = failing.saturating_add(1);
                precision_sum += decision.precision.clamp(0.0, 1.0);
                recall_sum += decision.recall.clamp(0.0, 1.0);
                if decision
                    .reasons
                    .iter()
                    .any(|reason| matches!(reason, AccuracyGateReason::SemanticRequiredUnmet))
                {
                    semantic_required_unmet = semantic_required_unmet.saturating_add(1);
                }
                continue;
            }
            if result.error.is_none() {
                considered = considered.saturating_add(1);
                pass_count = pass_count.saturating_add(1);
                precision_sum += 1.0;
                recall_sum += 1.0;
            }
        }

        if considered == 0 {
            return None;
        }

        Some(AccuracyGateRolloutSignal {
            considered_files: considered,
            failing_files: failing,
            semantic_required_unmet_files: semantic_required_unmet,
            precision: (precision_sum / considered as f64).clamp(0.0, 1.0),
            recall: (recall_sum / considered as f64).clamp(0.0, 1.0),
            match_ratio: (pass_count as f64 / considered as f64).clamp(0.0, 1.0),
        })
    }

    pub(crate) fn print_accuracy_rollout_status(
        status: &crate::runtime::rollout_state::AccuracyRolloutStatus,
        effective_fail_closed: bool,
    ) {
        let source = match status.last_observation_source {
            AccuracyObservationSource::Benchmark => "benchmark",
            AccuracyObservationSource::Gate => "gate",
        };
        println!(
            "accuracy_rollout: requested={} effective={} effective_fail_closed={} armed={} pass_streak={} fail_streak={} total_runs={} promotions={} demotions={} source={} source_passed={} source_samples_met={} source_precision={:.3} source_recall={:.3} source_match={:.3} pass_total={} fail_total={}",
            status.requested_profile.as_str(),
            status.effective_profile.as_str(),
            effective_fail_closed,
            status.fail_closed_armed,
            status.consecutive_passes,
            status.consecutive_failures,
            status.total_benchmark_runs,
            status.promotions,
            status.demotions,
            source,
            status.last_observation_passed,
            status.last_min_samples_met,
            status.last_precision,
            status.last_recall,
            status.last_match_ratio,
            status.benchmark_passes,
            status.benchmark_failures
        );
    }
}
