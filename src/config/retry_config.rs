#[derive(Clone, Debug)]
pub struct RetryConfig {
    pub post_edit_check_enabled: bool,
    pub post_edit_fail_on_parser_unavailable: bool,
    pub post_edit_tree_error_ratio_tolerance: f64,
    pub post_edit_retry_enabled: bool,
    pub post_edit_retry_max_attempts: usize,
    pub post_edit_retry_confidence_step: f64,
    pub post_edit_retry_confidence_max: f64,
    pub post_edit_retry_aggressive_step_multiplier: f64,
    pub post_edit_retry_no_improve_limit: usize,
    pub post_edit_retry_max_blocked_policies: usize,
    pub retry_snapshot_cache_size: usize,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            post_edit_check_enabled: true,
            post_edit_fail_on_parser_unavailable: true,
            post_edit_tree_error_ratio_tolerance: 0.01,
            post_edit_retry_enabled: true,
            post_edit_retry_max_attempts: 6,
            post_edit_retry_confidence_step: 0.05,
            post_edit_retry_confidence_max: 1.0,
            post_edit_retry_aggressive_step_multiplier: 1.8,
            post_edit_retry_no_improve_limit: 2,
            post_edit_retry_max_blocked_policies: 6,
            retry_snapshot_cache_size: 128,
        }
    }
}
