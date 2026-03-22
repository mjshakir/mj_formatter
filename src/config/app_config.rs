use std::collections::HashMap;
use std::path::PathBuf;

use crate::config::enums::BackupMode;
use crate::config::enums::ClangArgsMode;
use crate::config::policy_config::PolicyConfig;
use crate::config::types::AccuracyBenchmarkConfig;
use crate::config::types::AccuracyGateConfig;
use crate::config::types::ConfidenceConfig;
use crate::config::types::ProjectGraphConfig;
use crate::config::types::RetryConfig;
use crate::config::types::RetryOptimizerConfig;

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub root: PathBuf,
    pub include_patterns: Vec<String>,
    pub exclude_patterns: Vec<String>,
    pub jobs: usize,
    pub processes: usize,
    pub check: bool,
    pub verbose: bool,
    pub backup: bool,
    pub backup_mode: BackupMode,
    pub backup_suffix: String,
    pub backup_dir: PathBuf,
    pub run_journal_dir: PathBuf,
    pub report_path: PathBuf,
    pub cache_enabled: bool,
    pub cache_path: PathBuf,
    pub cache_l1_size: usize,
    pub tracker_path: PathBuf,
    pub style_name: String,
    pub policy_settings: HashMap<String, PolicyConfig>,
    pub policy_order: Vec<String>,
    pub cpp_standard: String,
    pub clang_binary: String,
    pub clang_args: Vec<String>,
    pub clang_compdb_path: Option<PathBuf>,
    pub clang_args_mode: ClangArgsMode,
    pub semantic_require_compdb: bool,
    pub semantic_no_inferred: bool,
    pub worker_timeout_secs: u64,
    pub worker_kill_secs: u64,
    pub worker_max_restarts: usize,
    pub clang_format_binary: String,
    pub conflict_enabled: bool,
    pub conflict_touch_threshold: usize,
    pub confidence: ConfidenceConfig,
    pub retry_strategy_optimizer: RetryOptimizerConfig,
    pub retry: RetryConfig,
    pub accuracy_gate: AccuracyGateConfig,
    pub accuracy_benchmark: AccuracyBenchmarkConfig,
    pub project_graph: ProjectGraphConfig,
    pub convergence_learn_on_check: bool,
    pub observation_only: bool,
}

impl AppConfig {
    pub fn enabled_policy_names(&self) -> Vec<String> {
        let mut resolved: Vec<String> = Vec::new();

        if !self.policy_order.is_empty() {
            for item in &self.policy_order {
                if let Some(policy) = self.policy_settings.get(item) {
                    if policy.enabled {
                        resolved.push(item.clone());
                    }
                }
            }
        }

        if resolved.is_empty() {
            let mut names: Vec<String> = self
                .policy_settings
                .iter()
                .filter_map(|(name, policy)| policy.enabled.then_some(name.clone()))
                .collect();
            names.sort();
            return names;
        }

        resolved
    }
}
