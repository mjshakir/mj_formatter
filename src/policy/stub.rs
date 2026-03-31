use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::policy::Policy;

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

    fn apply(&self, _context: &PolicyContext<'_>) -> PolicyResult {
        PolicyResult::unchanged_with_warning(format!("{}: skipped ({})", self.name, self.reason))
    }
}
