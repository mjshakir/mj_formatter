use crate::policy::id::PolicyId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Violation {
    pub policy: PolicyId,
    pub message: String,
    pub line: usize,
    pub column: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::Violation;
    use crate::policy::id::PolicyId;

    #[test]
    fn serde_roundtrip_legacy() {
        let violation = Violation {
            policy: PolicyId::from_str_lossy("semantic_contract"),
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
