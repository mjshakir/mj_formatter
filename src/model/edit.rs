use crate::model::policy_name::PolicyName;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Edit {
    pub policy: PolicyName,
    pub line: usize,
    pub before: String,
    pub after: String,
}

#[cfg(test)]
mod tests {
    use super::Edit;

    #[test]
    fn serde_roundtrip_preserves_legacy_policy_string() {
        let edit = Edit {
            policy: "naming_conventions".into(),
            line: 12,
            before: "Value".to_string(),
            after: "value".to_string(),
        };

        let value = serde_json::to_value(&edit).expect("serialize edit");
        assert_eq!(value["policy"], "naming_conventions");

        let restored: Edit = serde_json::from_value(value).expect("deserialize edit");
        assert_eq!(restored.policy, "naming_conventions");
    }
}
