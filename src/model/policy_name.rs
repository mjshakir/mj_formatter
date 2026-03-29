use std::borrow::Borrow;
use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash, Deserialize, Serialize)]
#[serde(transparent)]
pub struct PolicyName(String);

impl PolicyName {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl Borrow<str> for PolicyName {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for PolicyName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<String> for PolicyName {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for PolicyName {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl From<&String> for PolicyName {
    fn from(value: &String) -> Self {
        Self(value.clone())
    }
}

impl From<PolicyName> for String {
    fn from(value: PolicyName) -> Self {
        value.0
    }
}

impl_str_partial_eq!(PolicyName);
