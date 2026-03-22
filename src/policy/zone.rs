use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum PolicyZone {
    #[default]
    Code,
    Preprocessor,
    Comments,
}

impl PolicyZone {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Preprocessor => "preprocessor",
            Self::Comments => "comments",
        }
    }

    fn from_serialized(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "code" => Some(Self::Code),
            "preprocessor" => Some(Self::Preprocessor),
            "comments" => Some(Self::Comments),
            _ => None,
        }
    }
}

impl Serialize for PolicyZone {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PolicyZone {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_serialized(value.as_str())
            .ok_or_else(|| serde::de::Error::custom(format!("unknown policy zone '{value}'")))
    }
}
