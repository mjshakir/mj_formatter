use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct RetryStrategyOptimizerConfig {
    pub enabled: bool,
    pub path: PathBuf,
    pub ema_alpha: f64,
    pub context_weight: f64,
    pub min_samples: u64,
    pub max_bonus: i32,
    pub persist_every: u64,
    pub canary_only: bool,
    pub auto_tune_enabled: bool,
    pub auto_tune_ema_alpha: f64,
    pub auto_tune_target_retry_success_rate: f64,
    pub auto_tune_deadband: f64,
    pub auto_tune_step: i32,
    pub auto_tune_adjust_every: u64,
    pub auto_tune_min_samples: u64,
    pub auto_tune_min_bonus: i32,
    pub auto_tune_max_bonus_cap: i32,
}

impl Default for RetryStrategyOptimizerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: PathBuf::from("var/cache/retry_optimizer.json"),
            ema_alpha: 0.20,
            context_weight: 0.70,
            min_samples: 10,
            max_bonus: 180,
            persist_every: 16,
            canary_only: false,
            auto_tune_enabled: true,
            auto_tune_ema_alpha: 0.15,
            auto_tune_target_retry_success_rate: 0.80,
            auto_tune_deadband: 0.05,
            auto_tune_step: 10,
            auto_tune_adjust_every: 4,
            auto_tune_min_samples: 16,
            auto_tune_min_bonus: 40,
            auto_tune_max_bonus_cap: 240,
        }
    }
}
