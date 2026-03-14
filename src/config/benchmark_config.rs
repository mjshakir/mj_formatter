use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct AccuracyBenchmarkConfig {
    pub enabled: bool,
    pub input_dir: PathBuf,
    pub expected_dir: PathBuf,
    pub min_precision: f64,
    pub min_recall: f64,
    pub min_match_ratio: f64,
    pub min_samples: usize,
    pub fail_closed: bool,
}

impl Default for AccuracyBenchmarkConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            input_dir: PathBuf::from("behavior_test/input"),
            expected_dir: PathBuf::from("behavior_test/expected"),
            min_precision: 0.90,
            min_recall: 0.90,
            min_match_ratio: 0.90,
            min_samples: 8,
            fail_closed: false,
        }
    }
}
