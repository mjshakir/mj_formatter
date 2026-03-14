use crate::model::edit::Edit;
use crate::model::violation::Violation;

#[derive(Clone, Debug, Default)]
pub struct PolicyResult {
    pub text: String,
    pub violations: Vec<Violation>,
    pub edits: Vec<Edit>,
    pub warnings: Vec<String>,
}
