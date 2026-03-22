use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::borrow::Borrow;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum RetryStrategyTag {
    Culprit,
    Top,
    Aggressive,
    HoldConfidence,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum KnownRetryStrategy {
    GuidedForceCulprit,
    GuidedForceCulpritHoldConfidence,
    GuidedForceTopHoldConfidence,
    RepeatSemanticCulpritTopHoldConfidence,
    RepeatSemanticCulpritTopAggressive,
    RepeatSemanticTop2HoldConfidence,
    ParserUnavailableAggressiveConfidence,
    ParserUnavailableBlockCulprit,
    ParserUnavailableBlockCulpritHoldConfidence,
    CulpritOnly,
    CulpritOnlyAggressiveConfidence,
    CulpritPlusTop,
    CulpritOnlyHoldConfidence,
    TopOnly,
    ConfidenceOnlyAggressive,
    TopOnlyHoldConfidence,
    Top2HoldConfidence,
    Top3HoldConfidence,
    CulpritPlusTopHoldConfidence,
    GuidedHoldConfidenceOnly,
}

impl KnownRetryStrategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::GuidedForceCulprit => "guided_force_culprit",
            Self::GuidedForceCulpritHoldConfidence => "guided_force_culprit_hold_confidence",
            Self::GuidedForceTopHoldConfidence => "guided_force_top_hold_confidence",
            Self::RepeatSemanticCulpritTopHoldConfidence => {
                "repeat_semantic_culprit_top_hold_confidence"
            }
            Self::RepeatSemanticCulpritTopAggressive => "repeat_semantic_culprit_top_aggressive",
            Self::RepeatSemanticTop2HoldConfidence => "repeat_semantic_top2_hold_confidence",
            Self::ParserUnavailableAggressiveConfidence => {
                "parser_unavailable_aggressive_confidence"
            }
            Self::ParserUnavailableBlockCulprit => "parser_unavailable_block_culprit",
            Self::ParserUnavailableBlockCulpritHoldConfidence => {
                "parser_unavailable_block_culprit_hold_confidence"
            }
            Self::CulpritOnly => "culprit_only",
            Self::CulpritOnlyAggressiveConfidence => "culprit_only_aggressive_confidence",
            Self::CulpritPlusTop => "culprit_plus_top",
            Self::CulpritOnlyHoldConfidence => "culprit_only_hold_confidence",
            Self::TopOnly => "top_only",
            Self::ConfidenceOnlyAggressive => "confidence_only_aggressive",
            Self::TopOnlyHoldConfidence => "top_only_hold_confidence",
            Self::Top2HoldConfidence => "top2_hold_confidence",
            Self::Top3HoldConfidence => "top3_hold_confidence",
            Self::CulpritPlusTopHoldConfidence => "culprit_plus_top_hold_confidence",
            Self::GuidedHoldConfidenceOnly => "guided_hold_confidence_only",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "guided_force_culprit" => Some(Self::GuidedForceCulprit),
            "guided_force_culprit_hold_confidence" => Some(Self::GuidedForceCulpritHoldConfidence),
            "guided_force_top_hold_confidence" => Some(Self::GuidedForceTopHoldConfidence),
            "repeat_semantic_culprit_top_hold_confidence" => {
                Some(Self::RepeatSemanticCulpritTopHoldConfidence)
            }
            "repeat_semantic_culprit_top_aggressive" => {
                Some(Self::RepeatSemanticCulpritTopAggressive)
            }
            "repeat_semantic_top2_hold_confidence" => Some(Self::RepeatSemanticTop2HoldConfidence),
            "parser_unavailable_aggressive_confidence" => {
                Some(Self::ParserUnavailableAggressiveConfidence)
            }
            "parser_unavailable_block_culprit" => Some(Self::ParserUnavailableBlockCulprit),
            "parser_unavailable_block_culprit_hold_confidence" => {
                Some(Self::ParserUnavailableBlockCulpritHoldConfidence)
            }
            "culprit_only" => Some(Self::CulpritOnly),
            "culprit_only_aggressive_confidence" => Some(Self::CulpritOnlyAggressiveConfidence),
            "culprit_plus_top" => Some(Self::CulpritPlusTop),
            "culprit_only_hold_confidence" => Some(Self::CulpritOnlyHoldConfidence),
            "top_only" => Some(Self::TopOnly),
            "confidence_only_aggressive" => Some(Self::ConfidenceOnlyAggressive),
            "top_only_hold_confidence" => Some(Self::TopOnlyHoldConfidence),
            "top2_hold_confidence" => Some(Self::Top2HoldConfidence),
            "top3_hold_confidence" => Some(Self::Top3HoldConfidence),
            "culprit_plus_top_hold_confidence" => Some(Self::CulpritPlusTopHoldConfidence),
            "guided_hold_confidence_only" => Some(Self::GuidedHoldConfidenceOnly),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum RetryStrategyName {
    Known(KnownRetryStrategy),
    Custom(String),
}

impl Default for RetryStrategyName {
    fn default() -> Self {
        Self::Custom(String::new())
    }
}

impl RetryStrategyName {
    pub fn from_str_lossy(value: &str) -> Self {
        if let Some(known) = KnownRetryStrategy::from_str(value) {
            return Self::Known(known);
        }
        Self::Custom(value.trim().to_ascii_lowercase())
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Known(known) => known.as_str(),
            Self::Custom(value) => value.as_str(),
        }
    }

    pub fn has_tag(&self, tag: RetryStrategyTag) -> bool {
        match tag {
            RetryStrategyTag::Culprit => self.as_str().contains("culprit"),
            RetryStrategyTag::Top => self.as_str().contains("top"),
            RetryStrategyTag::Aggressive => self.as_str().contains("aggressive"),
            RetryStrategyTag::HoldConfidence => self.as_str().contains("hold_confidence"),
        }
    }
}

impl Borrow<str> for RetryStrategyName {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl std::ops::Deref for RetryStrategyName {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl Serialize for RetryStrategyName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RetryStrategyName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from_str_lossy(value.as_str()))
    }
}

impl From<&str> for RetryStrategyName {
    fn from(value: &str) -> Self {
        Self::from_str_lossy(value)
    }
}

impl From<String> for RetryStrategyName {
    fn from(value: String) -> Self {
        Self::from_str_lossy(value.as_str())
    }
}

impl std::fmt::Display for RetryStrategyName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PartialEq<&str> for RetryStrategyName {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<RetryStrategyName> for &str {
    fn eq(&self, other: &RetryStrategyName) -> bool {
        *self == other.as_str()
    }
}

#[cfg(test)]
mod tests {
    use crate::model::retry_strategy::{KnownRetryStrategy, RetryStrategyName, RetryStrategyTag};

    #[test]
    fn strategy_roundtrips_string() {
        let value = RetryStrategyName::from_str_lossy("parser_unavailable_block_culprit");
        assert_eq!(
            value,
            RetryStrategyName::Known(KnownRetryStrategy::ParserUnavailableBlockCulprit)
        );
        assert_eq!(value.as_str(), "parser_unavailable_block_culprit");
    }

    #[test]
    fn custom_strategy_preserved() {
        let value = RetryStrategyName::from_str_lossy("custom_strategy_x");
        assert_eq!(value.as_str(), "custom_strategy_x");
        assert!(matches!(value, RetryStrategyName::Custom(_)));
    }

    #[test]
    fn strategy_tags_detected() {
        let value = RetryStrategyName::from_str_lossy("culprit_only_hold_confidence");
        assert!(value.has_tag(RetryStrategyTag::Culprit));
        assert!(value.has_tag(RetryStrategyTag::HoldConfidence));
        assert!(!value.has_tag(RetryStrategyTag::Aggressive));
    }
}
