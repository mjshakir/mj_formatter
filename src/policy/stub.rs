use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::policy::traits::Policy;

pub struct StubPolicy {
    name: String,
    reason: String,
}

impl StubPolicy {
    pub fn new(name: String, reason: String) -> Self {
        Self { name, reason }
    }
}

impl Policy for StubPolicy {
    fn name(&self) -> &str {
        &self.name
    }

    fn apply(&self, context: &PolicyContext<'_>) -> PolicyResult {
        PolicyResult {
            text: context.text.to_string(),
            violations: Vec::new(),
            edits: Vec::new(),
            warnings: vec![format!("{}: skipped ({})", self.name, self.reason)],
        }
    }
}
