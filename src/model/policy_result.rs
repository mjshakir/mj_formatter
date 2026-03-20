use crate::model::edit::Edit;
use crate::model::violation::Violation;

#[derive(Clone, Debug, Default)]
pub struct PolicyResult {
    pub text: String,
    pub violations: Vec<Violation>,
    pub edits: Vec<Edit>,
    pub warnings: Vec<String>,
}

impl PolicyResult {
    pub fn rename_coverage_signal(&self) -> Option<f64> {
        const PREFIX: &str = "internal:rename_coverage_signal:";
        self.warnings.iter().find_map(|w| {
            w.strip_prefix(PREFIX).and_then(|v| v.parse::<f64>().ok())
        })
    }
}
