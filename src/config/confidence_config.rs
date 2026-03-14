use crate::config::enums::Enforcement;

#[derive(Clone, Debug)]
pub struct ConfidenceConfig {
    pub enabled: bool,
    pub default_enforcement: Enforcement,
}

impl Default for ConfidenceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_enforcement: Enforcement::Hard,
        }
    }
}
