use crate::model::policy_name::PolicyName;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Violation {
    pub policy: PolicyName,
    pub message: String,
    pub line: usize,
    pub column: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::Violation;

    #[test]
    fn serde_roundtrip_preserves_legacy_policy_string() {
        let violation = Violation {
            policy: "semantic_contract".into(),
            message: "regressed".to_string(),
            line: 4,
            column: Some(2),
        };

        let value = serde_json::to_value(&violation).expect("serialize violation");
        assert_eq!(value["policy"], "semantic_contract");

        let restored: Violation = serde_json::from_value(value).expect("deserialize violation");
        assert_eq!(restored.policy, "semantic_contract");
    }
}
