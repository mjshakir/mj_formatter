use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::config::rollout_profile::AccuracyRolloutProfile;
use crate::files::atomic_writer::AtomicWriter;
use crate::files::codec::StateCodec;

const ACCURACY_ROLLOUT_SCHEMA_VERSION: u32 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccuracyObservationSource {
    Benchmark,
    Gate,
}

impl AccuracyObservationSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Benchmark => "benchmark",
            Self::Gate => "gate",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "benchmark" => Some(Self::Benchmark),
            "gate" => Some(Self::Gate),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct PersistedAccuracyRolloutState {
    schema_version: u32,
    requested_profile: String,
    effective_profile: String,
    total_benchmark_runs: u64,
    benchmark_passes: u64,
    benchmark_failures: u64,
    consecutive_passes: u64,
    consecutive_failures: u64,
    fail_closed_armed: bool,
    promotions: u64,
    demotions: u64,
    last_precision: f64,
    last_recall: f64,
    last_match_ratio: f64,
    last_min_samples_met: bool,
    last_observation_source: String,
    last_observation_passed: bool,
    last_updated_unix_ms: u64,
}

pub struct AccuracyRolloutState {
    path: PathBuf,
    state: PersistedAccuracyRolloutState,
}

#[derive(Clone, Debug)]
pub struct AccuracyRolloutStatus {
    pub requested_profile: AccuracyRolloutProfile,
    pub effective_profile: AccuracyRolloutProfile,
    pub consecutive_passes: u64,
    pub consecutive_failures: u64,
    pub total_benchmark_runs: u64,
    pub fail_closed_armed: bool,
    pub promotions: u64,
    pub demotions: u64,
    pub benchmark_passes: u64,
    pub benchmark_failures: u64,
    pub last_precision: f64,
    pub last_recall: f64,
    pub last_match_ratio: f64,
    pub last_min_samples_met: bool,
    pub last_observation_source: AccuracyObservationSource,
    pub last_observation_passed: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct AccuracyObservation {
    pub passed: bool,
    pub precision: f64,
    pub recall: f64,
    pub match_ratio: f64,
    pub min_samples_met: bool,
    pub stable_passes_required: usize,
}

impl AccuracyRolloutState {
    pub fn open(path: &Path) -> Self {
        let state = match Self::load_state(path) {
            Ok(value) => value,
            Err(error) => {
                Self::quarantine_corrupted_state(path, error.to_string().as_str());
                warn!(
                    error = %error,
                    path = %path.display(),
                    "accuracy rollout state load failed; using defaults"
                );
                PersistedAccuracyRolloutState::default()
            }
        };
        Self {
            path: path.to_path_buf(),
            state,
        }
    }

    pub fn effective_profile(
        &self,
        requested_profile: AccuracyRolloutProfile,
    ) -> AccuracyRolloutProfile {
        if self.state.requested_profile != requested_profile.as_str() {
            return requested_profile;
        }
        Self::parse_profile(self.state.effective_profile.as_str()).unwrap_or(requested_profile)
    }

    pub fn effective_fail_closed(
        &self,
        profile: AccuracyRolloutProfile,
        requested_fail_closed: bool,
        defer_until_stable: bool,
    ) -> bool {
        if !requested_fail_closed {
            return false;
        }
        if !defer_until_stable {
            return true;
        }
        if self.state.requested_profile != profile.as_str() {
            return false;
        }
        let active_profile = self.effective_profile(profile);
        if matches!(active_profile, AccuracyRolloutProfile::Strict)
            && self.state.consecutive_passes > 0
        {
            return true;
        }
        self.state.fail_closed_armed
    }

    pub fn observe_benchmark(
        &mut self,
        profile: AccuracyRolloutProfile,
        observation: AccuracyObservation,
    ) -> Result<()> {
        self.observe_signal(profile, AccuracyObservationSource::Benchmark, observation)
    }

    pub fn observe_gate_signal(
        &mut self,
        profile: AccuracyRolloutProfile,
        observation: AccuracyObservation,
    ) -> Result<()> {
        self.observe_signal(profile, AccuracyObservationSource::Gate, observation)
    }

    fn observe_signal(
        &mut self,
        profile: AccuracyRolloutProfile,
        source: AccuracyObservationSource,
        observation: AccuracyObservation,
    ) -> Result<()> {
        if self.state.requested_profile != profile.as_str() {
            self.state.requested_profile = profile.as_str().to_string();
            self.state.effective_profile = profile.as_str().to_string();
            self.state.consecutive_passes = 0;
            self.state.consecutive_failures = 0;
            self.state.fail_closed_armed = false;
        }
        self.state.schema_version = ACCURACY_ROLLOUT_SCHEMA_VERSION;
        self.state.total_benchmark_runs = self.state.total_benchmark_runs.saturating_add(1);
        self.state.last_precision = observation.precision.clamp(0.0, 1.0);
        self.state.last_recall = observation.recall.clamp(0.0, 1.0);
        self.state.last_match_ratio = observation.match_ratio.clamp(0.0, 1.0);
        self.state.last_min_samples_met = observation.min_samples_met;
        self.state.last_observation_source = source.as_str().to_string();
        self.state.last_observation_passed = observation.passed;
        self.state.last_updated_unix_ms = current_unix_ms();
        if !observation.min_samples_met {
            self.state.fail_closed_armed = false;
            return self.persist();
        }
        if observation.passed {
            self.state.benchmark_passes = self.state.benchmark_passes.saturating_add(1);
            self.state.consecutive_passes = self.state.consecutive_passes.saturating_add(1);
            self.state.consecutive_failures = 0;
        } else {
            self.state.benchmark_failures = self.state.benchmark_failures.saturating_add(1);
            self.state.consecutive_failures = self.state.consecutive_failures.saturating_add(1);
            self.state.consecutive_passes = 0;
            self.state.fail_closed_armed = false;
        }
        let active_profile = self.effective_profile(profile);
        let transitioned_profile = if observation.passed {
            self.promote_profile_if_eligible(
                active_profile,
                observation.precision,
                observation.recall,
                observation.match_ratio,
            )
        } else {
            self.demote_profile_if_unstable(active_profile)
        };
        self.state.effective_profile = transitioned_profile.as_str().to_string();
        let stable_target =
            Self::stable_pass_target(transitioned_profile, observation.stable_passes_required);
        self.state.fail_closed_armed = self.state.consecutive_passes >= stable_target as u64
            && self.state.requested_profile == profile.as_str();
        if !self.state.fail_closed_armed {
            self.state.fail_closed_armed = false;
        }
        self.persist()
    }

    pub fn status(&self, profile: AccuracyRolloutProfile) -> AccuracyRolloutStatus {
        let effective_profile = self.effective_profile(profile);
        AccuracyRolloutStatus {
            requested_profile: profile,
            effective_profile,
            consecutive_passes: self.state.consecutive_passes,
            consecutive_failures: self.state.consecutive_failures,
            total_benchmark_runs: self.state.total_benchmark_runs,
            fail_closed_armed: self.state.fail_closed_armed
                && self.state.requested_profile == profile.as_str(),
            promotions: self.state.promotions,
            demotions: self.state.demotions,
            benchmark_passes: self.state.benchmark_passes,
            benchmark_failures: self.state.benchmark_failures,
            last_precision: self.state.last_precision.clamp(0.0, 1.0),
            last_recall: self.state.last_recall.clamp(0.0, 1.0),
            last_match_ratio: self.state.last_match_ratio.clamp(0.0, 1.0),
            last_min_samples_met: self.state.last_min_samples_met,
            last_observation_source: AccuracyObservationSource::from_str(
                self.state.last_observation_source.as_str(),
            )
            .unwrap_or(AccuracyObservationSource::Benchmark),
            last_observation_passed: self.state.last_observation_passed,
        }
    }

    fn promote_profile_if_eligible(
        &mut self,
        active_profile: AccuracyRolloutProfile,
        precision: f64,
        recall: f64,
        match_ratio: f64,
    ) -> AccuracyRolloutProfile {
        let Some(target_profile) = Self::promote_target(active_profile) else {
            return active_profile;
        };
        if self.state.consecutive_passes < Self::promotion_streak_required(active_profile) {
            return active_profile;
        }
        if !Self::meets_profile_thresholds(target_profile, precision, recall, match_ratio) {
            return active_profile;
        }
        self.state.promotions = self.state.promotions.saturating_add(1);
        self.state.consecutive_passes = 0;
        self.state.consecutive_failures = 0;
        self.state.fail_closed_armed = false;
        target_profile
    }

    fn demote_profile_if_unstable(
        &mut self,
        active_profile: AccuracyRolloutProfile,
    ) -> AccuracyRolloutProfile {
        let Some(target_profile) = Self::demote_target(active_profile) else {
            return active_profile;
        };
        if self.state.consecutive_failures < Self::demotion_streak_required(active_profile) {
            return active_profile;
        }
        self.state.demotions = self.state.demotions.saturating_add(1);
        self.state.consecutive_passes = 0;
        self.state.consecutive_failures = 0;
        self.state.fail_closed_armed = false;
        target_profile
    }

    fn parse_profile(value: &str) -> Option<AccuracyRolloutProfile> {
        match value.trim().to_ascii_lowercase().as_str() {
            "strict" => Some(AccuracyRolloutProfile::Strict),
            "balanced" => Some(AccuracyRolloutProfile::Balanced),
            "adaptive" => Some(AccuracyRolloutProfile::Adaptive),
            _ => None,
        }
    }

    fn stable_pass_target(profile: AccuracyRolloutProfile, requested: usize) -> usize {
        let base = requested.max(1);
        match profile {
            AccuracyRolloutProfile::Strict => base.min(2),
            AccuracyRolloutProfile::Balanced => base.max(2),
            AccuracyRolloutProfile::Adaptive => base.saturating_add(1).max(3),
        }
    }

    fn promotion_streak_required(profile: AccuracyRolloutProfile) -> u64 {
        match profile {
            AccuracyRolloutProfile::Adaptive => 3,
            AccuracyRolloutProfile::Balanced => 5,
            AccuracyRolloutProfile::Strict => u64::MAX,
        }
    }

    fn demotion_streak_required(profile: AccuracyRolloutProfile) -> u64 {
        match profile {
            AccuracyRolloutProfile::Strict => 2,
            AccuracyRolloutProfile::Balanced => 3,
            AccuracyRolloutProfile::Adaptive => u64::MAX,
        }
    }

    fn promote_target(profile: AccuracyRolloutProfile) -> Option<AccuracyRolloutProfile> {
        match profile {
            AccuracyRolloutProfile::Adaptive => Some(AccuracyRolloutProfile::Balanced),
            AccuracyRolloutProfile::Balanced => Some(AccuracyRolloutProfile::Strict),
            AccuracyRolloutProfile::Strict => None,
        }
    }

    fn demote_target(profile: AccuracyRolloutProfile) -> Option<AccuracyRolloutProfile> {
        match profile {
            AccuracyRolloutProfile::Strict => Some(AccuracyRolloutProfile::Balanced),
            AccuracyRolloutProfile::Balanced => Some(AccuracyRolloutProfile::Adaptive),
            AccuracyRolloutProfile::Adaptive => None,
        }
    }

    fn meets_profile_thresholds(
        profile: AccuracyRolloutProfile,
        precision: f64,
        recall: f64,
        match_ratio: f64,
    ) -> bool {
        let precision = precision.clamp(0.0, 1.0);
        let recall = recall.clamp(0.0, 1.0);
        let match_ratio = match_ratio.clamp(0.0, 1.0);
        match profile {
            AccuracyRolloutProfile::Strict => {
                precision >= 0.98 && recall >= 0.94 && match_ratio >= 0.95
            }
            AccuracyRolloutProfile::Balanced => {
                precision >= 0.94 && recall >= 0.78 && match_ratio >= 0.85
            }
            AccuracyRolloutProfile::Adaptive => {
                precision >= 0.90 && recall >= 0.60 && match_ratio >= 0.72
            }
        }
    }

    fn load_state(path: &Path) -> Result<PersistedAccuracyRolloutState> {
        if !path.exists() {
            return Ok(PersistedAccuracyRolloutState {
                schema_version: ACCURACY_ROLLOUT_SCHEMA_VERSION,
                requested_profile: AccuracyRolloutProfile::Balanced.as_str().to_string(),
                effective_profile: AccuracyRolloutProfile::Balanced.as_str().to_string(),
                ..PersistedAccuracyRolloutState::default()
            });
        }
        let mut state = StateCodec::read_decode_binary::<PersistedAccuracyRolloutState>(path)?;
        if state.schema_version > ACCURACY_ROLLOUT_SCHEMA_VERSION {
            state = PersistedAccuracyRolloutState {
                schema_version: ACCURACY_ROLLOUT_SCHEMA_VERSION,
                requested_profile: AccuracyRolloutProfile::Balanced.as_str().to_string(),
                effective_profile: AccuracyRolloutProfile::Balanced.as_str().to_string(),
                ..PersistedAccuracyRolloutState::default()
            };
        } else {
            state.schema_version = ACCURACY_ROLLOUT_SCHEMA_VERSION;
        }
        if state.requested_profile.is_empty() {
            state.requested_profile = AccuracyRolloutProfile::Balanced.as_str().to_string();
        }
        if state.effective_profile.is_empty() {
            state.effective_profile = state.requested_profile.clone();
        }
        if state.last_observation_source.is_empty() {
            state.last_observation_source =
                AccuracyObservationSource::Benchmark.as_str().to_string();
        }
        Ok(state)
    }

    fn persist(&self) -> Result<()> {
        let bytes = StateCodec::encode_binary(&self.state)?;
        AtomicWriter::write_bytes(self.path.as_path(), bytes.as_slice())?;
        Ok(())
    }

    fn quarantine_corrupted_state(path: &Path, reason: &str) {
        if !path.exists() {
            return;
        }
        let file_name = path
            .file_name()
            .and_then(|item| item.to_str())
            .unwrap_or("accuracy_rollout_state.bin");
        let quarantined = path.with_file_name(format!("{file_name}.corrupt.{}", current_unix_ms()));
        if let Err(rename_error) = fs::rename(path, quarantined.as_path()) {
            warn!(
                path = %path.display(),
                quarantined = %quarantined.display(),
                reason = %reason,
                error = %rename_error,
                "failed to quarantine accuracy rollout state"
            );
        } else {
            warn!(
                path = %path.display(),
                quarantined = %quarantined.display(),
                reason = %reason,
                "quarantined accuracy rollout state"
            );
        }
    }
}

fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::config::rollout_profile::AccuracyRolloutProfile;
    use crate::runtime::rollout_state::{
        AccuracyObservation, AccuracyObservationSource, AccuracyRolloutState,
    };

    fn temp_state_path() -> std::path::PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("fmt_rollout_{stamp}.bin"))
    }

    #[test]
    fn arms_fail_closed_after_stable_pass_streak() {
        let path = temp_state_path();
        let mut state = AccuracyRolloutState::open(path.as_path());
        assert!(!state.effective_fail_closed(AccuracyRolloutProfile::Balanced, true, true));
        state
            .observe_benchmark(
                AccuracyRolloutProfile::Balanced,
                AccuracyObservation {
                    passed: true,
                    precision: 0.95,
                    recall: 0.90,
                    match_ratio: 0.90,
                    min_samples_met: true,
                    stable_passes_required: 2,
                },
            )
            .expect("persist");
        assert!(!state.effective_fail_closed(AccuracyRolloutProfile::Balanced, true, true));
        state
            .observe_benchmark(
                AccuracyRolloutProfile::Balanced,
                AccuracyObservation {
                    passed: true,
                    precision: 0.95,
                    recall: 0.90,
                    match_ratio: 0.90,
                    min_samples_met: true,
                    stable_passes_required: 2,
                },
            )
            .expect("persist");
        assert!(state.effective_fail_closed(AccuracyRolloutProfile::Balanced, true, true));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn fail_resets_streak_and_disarms() {
        let path = temp_state_path();
        let mut state = AccuracyRolloutState::open(path.as_path());
        state
            .observe_benchmark(
                AccuracyRolloutProfile::Strict,
                AccuracyObservation {
                    passed: true,
                    precision: 0.98,
                    recall: 0.97,
                    match_ratio: 0.96,
                    min_samples_met: true,
                    stable_passes_required: 1,
                },
            )
            .expect("persist");
        assert!(state.effective_fail_closed(AccuracyRolloutProfile::Strict, true, true));
        state
            .observe_benchmark(
                AccuracyRolloutProfile::Strict,
                AccuracyObservation {
                    passed: false,
                    precision: 0.60,
                    recall: 0.50,
                    match_ratio: 0.55,
                    min_samples_met: true,
                    stable_passes_required: 1,
                },
            )
            .expect("persist");
        assert!(!state.effective_fail_closed(AccuracyRolloutProfile::Strict, true, true));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn promotes_profile_when_metrics_stay_stable() {
        let path = temp_state_path();
        let mut state = AccuracyRolloutState::open(path.as_path());
        for _ in 0..3 {
            state
                .observe_benchmark(
                    AccuracyRolloutProfile::Adaptive,
                    AccuracyObservation {
                        passed: true,
                        precision: 0.96,
                        recall: 0.84,
                        match_ratio: 0.88,
                        min_samples_met: true,
                        stable_passes_required: 4,
                    },
                )
                .expect("persist");
        }
        let status = state.status(AccuracyRolloutProfile::Adaptive);
        assert_eq!(status.effective_profile, AccuracyRolloutProfile::Balanced);
        assert!(status.promotions >= 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn demotes_profile_after_repeated_failures() {
        let path = temp_state_path();
        let mut state = AccuracyRolloutState::open(path.as_path());
        state
            .observe_benchmark(
                AccuracyRolloutProfile::Strict,
                AccuracyObservation {
                    passed: true,
                    precision: 0.99,
                    recall: 0.95,
                    match_ratio: 0.96,
                    min_samples_met: true,
                    stable_passes_required: 1,
                },
            )
            .expect("persist");
        state
            .observe_benchmark(
                AccuracyRolloutProfile::Strict,
                AccuracyObservation {
                    passed: false,
                    precision: 0.60,
                    recall: 0.50,
                    match_ratio: 0.55,
                    min_samples_met: true,
                    stable_passes_required: 1,
                },
            )
            .expect("persist");
        state
            .observe_benchmark(
                AccuracyRolloutProfile::Strict,
                AccuracyObservation {
                    passed: false,
                    precision: 0.55,
                    recall: 0.45,
                    match_ratio: 0.50,
                    min_samples_met: true,
                    stable_passes_required: 1,
                },
            )
            .expect("persist");
        let status = state.status(AccuracyRolloutProfile::Strict);
        assert_eq!(status.effective_profile, AccuracyRolloutProfile::Balanced);
        assert!(status.demotions >= 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn low_sample_runs_do_not_force_demotion() {
        let path = temp_state_path();
        let mut state = AccuracyRolloutState::open(path.as_path());
        state
            .observe_benchmark(
                AccuracyRolloutProfile::Balanced,
                AccuracyObservation {
                    passed: true,
                    precision: 0.95,
                    recall: 0.90,
                    match_ratio: 0.91,
                    min_samples_met: true,
                    stable_passes_required: 2,
                },
            )
            .expect("persist");
        let before = state.status(AccuracyRolloutProfile::Balanced);
        state
            .observe_gate_signal(
                AccuracyRolloutProfile::Balanced,
                AccuracyObservation {
                    passed: false,
                    precision: 0.10,
                    recall: 0.10,
                    match_ratio: 0.10,
                    min_samples_met: false,
                    stable_passes_required: 2,
                },
            )
            .expect("persist");
        let after = state.status(AccuracyRolloutProfile::Balanced);
        assert_eq!(before.effective_profile, after.effective_profile);
        assert_eq!(before.consecutive_failures, after.consecutive_failures);
        assert!(!after.last_min_samples_met);
        assert_eq!(
            after.last_observation_source,
            AccuracyObservationSource::Gate
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn gate_signal_is_persisted_in_status() {
        let path = temp_state_path();
        let mut state = AccuracyRolloutState::open(path.as_path());
        state
            .observe_gate_signal(
                AccuracyRolloutProfile::Adaptive,
                AccuracyObservation {
                    passed: true,
                    precision: 0.93,
                    recall: 0.80,
                    match_ratio: 0.88,
                    min_samples_met: true,
                    stable_passes_required: 3,
                },
            )
            .expect("persist");
        let status = state.status(AccuracyRolloutProfile::Adaptive);
        assert_eq!(
            status.last_observation_source,
            AccuracyObservationSource::Gate
        );
        assert!(status.last_observation_passed);
        assert!(status.last_precision >= 0.93 - f64::EPSILON);
        assert!(status.last_recall >= 0.80 - f64::EPSILON);
        assert!(status.last_match_ratio >= 0.88 - f64::EPSILON);
        let _ = std::fs::remove_file(path);
    }
}
