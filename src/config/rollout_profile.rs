#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccuracyRolloutProfile {
    Strict,
    Balanced,
    Adaptive,
}

impl AccuracyRolloutProfile {
    pub fn from_str(value: Option<&str>) -> Self {
        let Some(value) = value else {
            return Self::Balanced;
        };
        match value.trim().to_ascii_lowercase().as_str() {
            "strict" => Self::Strict,
            "adaptive" | "adoptive" => Self::Adaptive,
            "balanced" => Self::Balanced,
            _ => Self::Balanced,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Balanced => "balanced",
            Self::Adaptive => "adaptive",
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::rollout_profile::AccuracyRolloutProfile;

    #[test]
    fn parses_profile_variants() {
        assert_eq!(
            AccuracyRolloutProfile::from_str(Some("strict")),
            AccuracyRolloutProfile::Strict
        );
        assert_eq!(
            AccuracyRolloutProfile::from_str(Some("balanced")),
            AccuracyRolloutProfile::Balanced
        );
        assert_eq!(
            AccuracyRolloutProfile::from_str(Some("adaptive")),
            AccuracyRolloutProfile::Adaptive
        );
        assert_eq!(
            AccuracyRolloutProfile::from_str(Some("adoptive")),
            AccuracyRolloutProfile::Adaptive
        );
    }

    #[test]
    fn defaults_to_balanced() {
        assert_eq!(
            AccuracyRolloutProfile::from_str(Some("unknown")),
            AccuracyRolloutProfile::Balanced
        );
        assert_eq!(
            AccuracyRolloutProfile::from_str(None),
            AccuracyRolloutProfile::Balanced
        );
    }
}
