use crate::config::app_config::AppConfig;
use crate::engine::catalog::policy_catalog;
use crate::policy::factory::PolicyFactory;
use crate::policy::id::PolicyId;
use crate::policy::Policy;

pub struct PolicyRegistry;

impl PolicyRegistry {
    pub fn build_enabled(config: &AppConfig) -> Vec<Box<dyn Policy>> {
        let factory = PolicyFactory::new(config);
        let mut ordered: Vec<(usize, Box<dyn Policy>)> = Vec::new();

        for (index, name) in config.enabled_policy_names().into_iter().enumerate() {
            if let Some(settings) = config.policy_settings.get(&name) {
                ordered.push((index, factory.create(&name, settings)));
            }
        }
        ordered.sort_by(|(left_index, left_policy), (right_index, right_policy)| {
            Self::execution_priority(left_policy.id())
                .cmp(&Self::execution_priority(right_policy.id()))
                .then(left_index.cmp(right_index))
        });
        ordered.into_iter().map(|(_, policy)| policy).collect()
    }

    fn execution_priority(policy_id: PolicyId) -> u8 {
        policy_catalog()
            .behavior(&policy_id)
            .execution_priority
    }
}

#[cfg(test)]
mod tests {
    use super::PolicyRegistry;
    use crate::policy::id::PolicyId;

    #[test]
    fn priority_matches_buckets() {
        assert_eq!(
            PolicyRegistry::execution_priority(PolicyId::NamingConventions),
            10
        );
        assert_eq!(
            PolicyRegistry::execution_priority(PolicyId::LogicalKeywordOperators),
            20
        );
        assert_eq!(
            PolicyRegistry::execution_priority(PolicyId::IncludeOrder),
            40
        );
        assert_eq!(
            PolicyRegistry::execution_priority(PolicyId::ClassLayout),
            60
        );
        assert_eq!(
            PolicyRegistry::execution_priority(PolicyId::DashCommentNormalizer),
            70
        );
        assert_eq!(
            PolicyRegistry::execution_priority(PolicyId::ClangFormat),
            90
        );
    }

    #[test]
    fn priority_defaults_neutral() {
        assert_eq!(
            PolicyRegistry::execution_priority(PolicyId::Unknown("custom".to_string())),
            80
        );
    }
}
