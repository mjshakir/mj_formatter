use crate::model::edit::Edit;
use crate::model::violation::Violation;

#[derive(Clone, Debug, Default)]
pub struct PolicyResult {
    pub text: String,
    pub changed: bool,
    pub violations: Vec<Violation>,
    pub edits: Vec<Edit>,
    pub warnings: Vec<String>,
}

impl PolicyResult {
    #[inline]
    pub fn unchanged() -> Self {
        Self::default()
    }

    #[inline]
    pub fn unchanged_with_warning(warning: String) -> Self {
        Self {
            warnings: vec![warning],
            ..Self::default()
        }
    }

    #[inline]
    pub fn unchanged_with_warnings(warnings: Vec<String>) -> Self {
        Self {
            warnings,
            ..Self::default()
        }
    }

    pub fn rename_coverage_signal(&self) -> Option<f64> {
        const PREFIX: &str = "internal:rename_coverage_signal:";
        self.warnings.iter().find_map(|w| {
            w.strip_prefix(PREFIX).and_then(|v| v.parse::<f64>().ok())
        })
    }
}
