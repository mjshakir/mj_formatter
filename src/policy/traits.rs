use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::policy::id::PolicyId;

pub trait Policy: Send + Sync {
    fn id(&self) -> PolicyId {
        PolicyId::from_str_lossy(self.name())
    }

    fn name(&self) -> &str;

    fn apply(&self, context: &PolicyContext<'_>) -> PolicyResult;
}
