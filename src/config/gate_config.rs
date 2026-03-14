use std::path::PathBuf;

use crate::config::rollout_profile::AccuracyRolloutProfile;

#[derive(Clone, Debug)]
pub struct AccuracyGateConfig {
    pub enabled: bool,
    pub semantic_required: bool,
    pub fail_closed: bool,
    pub profile: AccuracyRolloutProfile,
    pub rollout_defer_fail_closed_until_stable: bool,
    pub rollout_stable_passes_required: usize,
    pub rollout_state_path: PathBuf,
    pub ci_require_benchmark: bool,
    pub min_precision: f64,
    pub min_recall: f64,
    pub min_samples: usize,
}

impl Default for AccuracyGateConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            semantic_required: false,
            fail_closed: false,
            profile: AccuracyRolloutProfile::Balanced,
            rollout_defer_fail_closed_until_stable: true,
            rollout_stable_passes_required: 5,
            rollout_state_path: PathBuf::from(
                "var/cache/rollout_state.bin",
            ),
            ci_require_benchmark: false,
            min_precision: 0.85,
            min_recall: 0.35,
            min_samples: 4,
        }
    }
}
