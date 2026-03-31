use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SymbolId(String);

impl SymbolId {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    #[cfg(test)]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Display for SymbolId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl From<String> for SymbolId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for SymbolId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::SymbolId;

    #[test]
    fn string_roundtrip() {
        let id = SymbolId::new("usr:c:@F@foo");
        assert_eq!(id.as_str(), "usr:c:@F@foo");
        assert!(!id.is_empty());
        assert_eq!(id.to_string(), "usr:c:@F@foo");
    }
}
