use std::collections::HashMap;
use std::sync::OnceLock;

use crate::engine::edit_candidate::CandidateRiskTier;
use crate::engine::zone::PolicyZone;
use crate::policy::id::PolicyId;

const ZONES_CODE_ONLY: [PolicyZone; 1] = [PolicyZone::Code];
const ZONES_CODE_AND_PREPROC: [PolicyZone; 2] = [PolicyZone::Code, PolicyZone::Preprocessor];
const ZONES_ALL: [PolicyZone; 3] = [
    PolicyZone::Code,
    PolicyZone::Preprocessor,
    PolicyZone::Comments,
];
const ZONES_COMMENTS_AND_PREPROC: [PolicyZone; 2] =
    [PolicyZone::Comments, PolicyZone::Preprocessor];

#[derive(Clone, Copy, Debug)]
pub struct PolicyCapabilities {
    pub whitespace_safe: bool,
    pub structural_safe: bool,
    pub semantic_rewrite: bool,
    pub macro_sensitive: bool,
    pub allowed_zones: &'static [PolicyZone],
    pub risk_tier: CandidateRiskTier,
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct PolicyCertainty {
    pub overall: f64,
    pub structural: f64,
    pub semantic: f64,
    pub coverage: f64,
    pub richness: f64,
    pub semantic_variance: f64,
    pub structural_variance: f64,
    pub coverage_variance: f64,
    pub richness_variance: f64,
    pub edit_success: f64,
    pub edit_success_variance: f64,
    pub stable_model_prob: f64,
    pub transitional_model_prob: f64,
    pub noisy_model_prob: f64,
    pub observation_count: u32,
    pub raw_observation: Option<[f64; 5]>,
}

const Z_ALPHA: f64 = 1.645;

impl PolicyCertainty {
    pub fn semantic_lower_ci(&self) -> f64 {
        (self.semantic - Z_ALPHA * self.semantic_variance.sqrt()).clamp(0.0, 1.0)
    }

    pub fn richness_lower_ci(&self) -> f64 {
        (self.richness - Z_ALPHA * self.richness_variance.sqrt()).clamp(0.0, 1.0)
    }

    pub fn edit_success_lower_ci(&self) -> f64 {
        (self.edit_success - Z_ALPHA * self.edit_success_variance.sqrt()).clamp(0.0, 1.0)
    }

    pub fn model_probs(&self) -> [f64; 3] {
        [self.stable_model_prob, self.transitional_model_prob, self.noisy_model_prob]
    }

    pub fn trust_for_semantic_rewrite(&self) -> f64 {
        crate::engine::fuzzy_inference::fuzzy_trust_semantic_rewrite(self)
    }

    pub fn trust_for_structural(&self) -> f64 {
        crate::engine::fuzzy_inference::fuzzy_trust_structural(self)
    }

    pub fn trust_for_general(&self) -> f64 {
        crate::engine::fuzzy_inference::fuzzy_trust_general(self)
    }
}

impl PolicyCapabilities {
    pub fn allows_zone(&self, zone: PolicyZone) -> bool {
        self.allowed_zones.contains(&zone)
    }

    pub fn effective_certainty(&self, certainty: &PolicyCertainty) -> f64 {
        if self.semantic_rewrite {
            certainty.semantic
        } else if self.structural_safe {
            certainty.structural
        } else {
            certainty.overall
        }
    }

    pub fn policy_trust(&self, certainty: &PolicyCertainty) -> f64 {
        if self.semantic_rewrite {
            certainty.trust_for_semantic_rewrite()
        } else if self.structural_safe {
            certainty.trust_for_structural()
        } else {
            certainty.trust_for_general()
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogConvergenceRiskTier {
    Stabilizer,
    Balanced,
    Rewrite,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicyConvergenceDefaults {
    pub domain: String,
    pub priority: u16,
    pub impact_radius: usize,
    pub semantic_priority_weight_bp: u16,
    pub risk_tier: CatalogConvergenceRiskTier,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PolicyBehavior {
    pub requires_exact_compdb_for_wide_structural: bool,
    pub keeps_non_local_conflict_batch: bool,
    pub advisory_on_semantic_retry: bool,
    pub hard_invariant_by_default: bool,
    pub bypasses_line_conflict: bool,
    pub execution_priority: u8,
}

#[derive(Clone, Copy, Debug)]
struct PolicyConvergenceTemplate {
    priority: u16,
    impact_radius: usize,
    semantic_priority_weight_bp: u16,
    risk_tier: CatalogConvergenceRiskTier,
}

impl Default for PolicyConvergenceTemplate {
    fn default() -> Self {
        Self {
            priority: 700,
            impact_radius: 0,
            semantic_priority_weight_bp: 240,
            risk_tier: CatalogConvergenceRiskTier::Balanced,
        }
    }
}

impl Default for PolicyBehavior {
    fn default() -> Self {
        Self {
            requires_exact_compdb_for_wide_structural: false,
            keeps_non_local_conflict_batch: false,
            advisory_on_semantic_retry: false,
            hard_invariant_by_default: false,
            bypasses_line_conflict: false,
            execution_priority: 80,
        }
    }
}

#[derive(Clone, Debug)]
struct PolicyCatalogEntry {
    name: &'static str,
    capabilities: PolicyCapabilities,
    convergence: PolicyConvergenceTemplate,
    behavior: PolicyBehavior,
}

#[derive(Clone, Debug)]
pub struct PolicyCatalog {
    known: HashMap<PolicyId, PolicyCatalogEntry>,
    default_capabilities: PolicyCapabilities,
    default_convergence: PolicyConvergenceTemplate,
    default_behavior: PolicyBehavior,
}

impl PolicyCatalog {
    fn build() -> Self {
        let mut known = HashMap::<PolicyId, PolicyCatalogEntry>::new();

        for id in [
            PolicyId::DashCommentNormalizer,
            PolicyId::SectionTitleNormalizer,
            PolicyId::CompactDeclarations,
            PolicyId::ClassLayout,
            PolicyId::LuaMacroSpacing,
            PolicyId::NamespaceEndComments,
            PolicyId::PragmaOnceSpacing,
            PolicyId::IncludeGuards,
            PolicyId::IncludeOrder,
            PolicyId::LogicalKeywordOperators,
            PolicyId::FunctionVoidParams,
            PolicyId::OperatorOverloadSpacing,
            PolicyId::ClangFormat,
            PolicyId::NamingConventions,
            PolicyId::SnakeCase,
            PolicyId::NumericLiteralSuffix,
        ] {
            known.insert(id.clone(), Self::entry_for_known_policy(id));
        }

        Self {
            known,
            default_capabilities: PolicyCapabilities {
                whitespace_safe: false,
                structural_safe: true,
                semantic_rewrite: false,
                macro_sensitive: true,
                allowed_zones: &ZONES_CODE_ONLY,
                risk_tier: CandidateRiskTier::Medium,
            },
            default_convergence: PolicyConvergenceTemplate::default(),
            default_behavior: PolicyBehavior::default(),
        }
    }

    pub fn capabilities_for_name(&self, policy_name: &str) -> PolicyCapabilities {
        let id = PolicyId::from_str_lossy(policy_name);
        self.capabilities_for_id(&id)
    }

    pub fn capabilities_for_id(&self, policy_id: &PolicyId) -> PolicyCapabilities {
        self.known
            .get(policy_id)
            .map(|entry| entry.capabilities)
            .unwrap_or(self.default_capabilities)
    }

    pub fn convergence_defaults_for_name(&self, policy_name: &str) -> PolicyConvergenceDefaults {
        let policy_id = PolicyId::from_str_lossy(policy_name);
        self.convergence_defaults_for_id(&policy_id)
    }

    pub fn convergence_defaults_for_id(&self, policy_id: &PolicyId) -> PolicyConvergenceDefaults {
        if let Some(entry) = self.known.get(policy_id) {
            return PolicyConvergenceDefaults {
                domain: entry.name.to_string(),
                priority: entry.convergence.priority,
                impact_radius: entry.convergence.impact_radius,
                semantic_priority_weight_bp: entry.convergence.semantic_priority_weight_bp,
                risk_tier: entry.convergence.risk_tier,
            };
        }
        PolicyConvergenceDefaults {
            domain: policy_id.as_str().to_string(),
            priority: self.default_convergence.priority,
            impact_radius: self.default_convergence.impact_radius,
            semantic_priority_weight_bp: self.default_convergence.semantic_priority_weight_bp,
            risk_tier: self.default_convergence.risk_tier,
        }
    }

    pub fn behavior_for_name(&self, policy_name: &str) -> PolicyBehavior {
        let policy_id = PolicyId::from_str_lossy(policy_name);
        self.behavior_for_id(&policy_id)
    }

    pub fn behavior_for_id(&self, policy_id: &PolicyId) -> PolicyBehavior {
        self.known
            .get(policy_id)
            .map(|entry| entry.behavior)
            .unwrap_or(self.default_behavior)
    }

    pub fn bypasses_line_conflict(&self, policy_name: &str) -> bool {
        self.behavior_for_name(policy_name).bypasses_line_conflict
    }

    pub fn is_semantic_rewrite_policy_name(&self, policy_name: &str) -> bool {
        self.capabilities_for_name(policy_name).semantic_rewrite
    }

    fn entry_for_known_policy(policy_id: PolicyId) -> PolicyCatalogEntry {
        let name = match policy_id {
            PolicyId::DashCommentNormalizer => "dash_comment_normalizer",
            PolicyId::SectionTitleNormalizer => "section_title_normalizer",
            PolicyId::CompactDeclarations => "compact_declarations",
            PolicyId::ClassLayout => "class_layout",
            PolicyId::LuaMacroSpacing => "lua_macro_spacing",
            PolicyId::NamespaceEndComments => "namespace_end_comments",
            PolicyId::PragmaOnceSpacing => "pragma_once_spacing",
            PolicyId::IncludeGuards => "include_guards",
            PolicyId::IncludeOrder => "include_order",
            PolicyId::LogicalKeywordOperators => "logical_keyword_operators",
            PolicyId::FunctionVoidParams => "function_void_params",
            PolicyId::OperatorOverloadSpacing => "operator_overload_spacing",
            PolicyId::ClangFormat => "clang_format",
            PolicyId::NamingConventions => "naming_conventions",
            PolicyId::SnakeCase => "snake_case",
            PolicyId::NumericLiteralSuffix => "numeric_literal_suffix",
            PolicyId::Unknown(_) => unreachable!("known policy entry cannot be unknown"),
        };

        let capabilities = match policy_id {
            PolicyId::DashCommentNormalizer | PolicyId::SectionTitleNormalizer => {
                PolicyCapabilities {
                    whitespace_safe: true,
                    structural_safe: true,
                    semantic_rewrite: false,
                    macro_sensitive: false,
                    allowed_zones: &ZONES_COMMENTS_AND_PREPROC,
                    risk_tier: CandidateRiskTier::Low,
                }
            }
            PolicyId::PragmaOnceSpacing
            | PolicyId::IncludeOrder
            | PolicyId::IncludeGuards
            | PolicyId::LuaMacroSpacing => PolicyCapabilities {
                whitespace_safe: false,
                structural_safe: true,
                semantic_rewrite: false,
                macro_sensitive: true,
                allowed_zones: &ZONES_CODE_AND_PREPROC,
                risk_tier: CandidateRiskTier::Medium,
            },
            PolicyId::ClangFormat => PolicyCapabilities {
                whitespace_safe: false,
                structural_safe: true,
                semantic_rewrite: false,
                macro_sensitive: true,
                allowed_zones: &ZONES_ALL,
                risk_tier: CandidateRiskTier::Medium,
            },
            PolicyId::ClassLayout
            | PolicyId::NamespaceEndComments
            | PolicyId::CompactDeclarations => PolicyCapabilities {
                whitespace_safe: false,
                structural_safe: true,
                semantic_rewrite: false,
                macro_sensitive: true,
                allowed_zones: &ZONES_CODE_ONLY,
                risk_tier: CandidateRiskTier::Medium,
            },
            PolicyId::LogicalKeywordOperators => PolicyCapabilities {
                whitespace_safe: false,
                structural_safe: true,
                semantic_rewrite: false,
                macro_sensitive: true,
                allowed_zones: &ZONES_CODE_ONLY,
                risk_tier: CandidateRiskTier::Medium,
            },
            PolicyId::NamingConventions
            | PolicyId::SnakeCase
            | PolicyId::FunctionVoidParams
            | PolicyId::OperatorOverloadSpacing => PolicyCapabilities {
                whitespace_safe: false,
                structural_safe: false,
                semantic_rewrite: true,
                macro_sensitive: true,
                allowed_zones: &ZONES_CODE_ONLY,
                risk_tier: CandidateRiskTier::High,
            },
            PolicyId::NumericLiteralSuffix => PolicyCapabilities {
                whitespace_safe: false,
                structural_safe: true,
                semantic_rewrite: false,
                macro_sensitive: true,
                allowed_zones: &ZONES_CODE_ONLY,
                risk_tier: CandidateRiskTier::Medium,
            },
            PolicyId::Unknown(_) => unreachable!("known policy entry cannot be unknown"),
        };

        let convergence_risk_tier = match policy_id {
            PolicyId::ClangFormat
            | PolicyId::IncludeOrder
            | PolicyId::SectionTitleNormalizer
            | PolicyId::PragmaOnceSpacing
            | PolicyId::NamespaceEndComments
            | PolicyId::DashCommentNormalizer => CatalogConvergenceRiskTier::Stabilizer,
            PolicyId::NamingConventions
            | PolicyId::SnakeCase
            | PolicyId::FunctionVoidParams
            | PolicyId::LogicalKeywordOperators => CatalogConvergenceRiskTier::Rewrite,
            _ => CatalogConvergenceRiskTier::Balanced,
        };

        let convergence = PolicyConvergenceTemplate {
            priority: match policy_id {
                PolicyId::ClangFormat => 1_000,
                PolicyId::SectionTitleNormalizer => 930,
                PolicyId::IncludeOrder => 900,
                PolicyId::PragmaOnceSpacing => 860,
                _ => 700,
            },
            impact_radius: match policy_id {
                PolicyId::ClangFormat | PolicyId::IncludeOrder => 2,
                PolicyId::SectionTitleNormalizer => 1,
                _ => 0,
            },
            semantic_priority_weight_bp: match policy_id {
                PolicyId::NamingConventions => 420,
                PolicyId::FunctionVoidParams => 300,
                _ => 240,
            },
            risk_tier: convergence_risk_tier,
        };

        let behavior = PolicyBehavior {
            requires_exact_compdb_for_wide_structural: matches!(
                policy_id,
                PolicyId::ClangFormat
                    | PolicyId::IncludeOrder
                    | PolicyId::ClassLayout
                    | PolicyId::CompactDeclarations
                    | PolicyId::DashCommentNormalizer
            ),
            keeps_non_local_conflict_batch: matches!(policy_id, PolicyId::ClangFormat),
            advisory_on_semantic_retry: matches!(policy_id, PolicyId::ClangFormat),
            hard_invariant_by_default: false,
            bypasses_line_conflict: matches!(
                policy_id,
                PolicyId::NumericLiteralSuffix | PolicyId::ClangFormat
            ),
            execution_priority: match policy_id {
                PolicyId::NamingConventions => 10,
                PolicyId::SnakeCase
                | PolicyId::FunctionVoidParams
                | PolicyId::LogicalKeywordOperators
                | PolicyId::OperatorOverloadSpacing => 20,
                PolicyId::PragmaOnceSpacing
                | PolicyId::IncludeGuards
                | PolicyId::IncludeOrder
                | PolicyId::LuaMacroSpacing => 40,
                PolicyId::CompactDeclarations
                | PolicyId::ClassLayout
                | PolicyId::NamespaceEndComments => 60,
                PolicyId::DashCommentNormalizer | PolicyId::SectionTitleNormalizer => 70,
                PolicyId::NumericLiteralSuffix => 80,
                PolicyId::ClangFormat => 90,
                PolicyId::Unknown(_) => 80,
            },
        };

        PolicyCatalogEntry {
            name,
            capabilities,
            convergence,
            behavior,
        }
    }
}

pub fn policy_catalog() -> &'static PolicyCatalog {
    static POLICY_CATALOG: OnceLock<PolicyCatalog> = OnceLock::new();
    POLICY_CATALOG.get_or_init(PolicyCatalog::build)
}

#[cfg(test)]
mod tests {
    use crate::engine::catalog::{
        policy_catalog, CatalogConvergenceRiskTier, PolicyCatalog,
    };
    use crate::engine::edit_candidate::CandidateRiskTier;
    use crate::policy::id::PolicyId;

    fn all_known_policy_ids() -> Vec<PolicyId> {
        vec![
            PolicyId::DashCommentNormalizer,
            PolicyId::SectionTitleNormalizer,
            PolicyId::CompactDeclarations,
            PolicyId::ClassLayout,
            PolicyId::LuaMacroSpacing,
            PolicyId::NamespaceEndComments,
            PolicyId::PragmaOnceSpacing,
            PolicyId::IncludeGuards,
            PolicyId::IncludeOrder,
            PolicyId::LogicalKeywordOperators,
            PolicyId::FunctionVoidParams,
            PolicyId::OperatorOverloadSpacing,
            PolicyId::ClangFormat,
            PolicyId::NamingConventions,
            PolicyId::SnakeCase,
            PolicyId::NumericLiteralSuffix,
        ]
    }

    #[test]
    fn catalog_contains_all_known_policy_ids() {
        let catalog = PolicyCatalog::build();
        for id in all_known_policy_ids() {
            assert!(
                catalog.known.contains_key(&id),
                "missing catalog entry for {}",
                id.as_str()
            );
        }
    }

    #[test]
    fn catalog_behavior_flags_match_expected_policies() {
        let catalog = policy_catalog();
        assert!(
            catalog
                .behavior_for_name("clang_format")
                .requires_exact_compdb_for_wide_structural
        );
        assert_eq!(
            catalog
                .behavior_for_name("naming_conventions")
                .execution_priority,
            10
        );
        assert_eq!(
            catalog.behavior_for_name("clang_format").execution_priority,
            90
        );
        assert!(
            catalog
                .behavior_for_name("clang_format")
                .keeps_non_local_conflict_batch
        );
        assert!(
            catalog
                .behavior_for_name("clang_format")
                .advisory_on_semantic_retry
        );
        assert!(
            !catalog
                .behavior_for_name("dash_comment_normalizer")
                .keeps_non_local_conflict_batch
        );
    }

    #[test]
    fn semantic_rewrite_flag_is_source_of_truth_for_policy_capability() {
        let catalog = policy_catalog();
        let semantic = catalog.capabilities_for_name("naming_conventions");
        assert!(semantic.semantic_rewrite);
        assert_eq!(semantic.risk_tier, CandidateRiskTier::High);

        let stable = catalog.capabilities_for_name("dash_comment_normalizer");
        assert!(!stable.semantic_rewrite);
        assert_eq!(stable.risk_tier, CandidateRiskTier::Low);
    }

    #[test]
    fn convergence_defaults_are_stable_for_anchor_policies() {
        let catalog = policy_catalog();
        let clang = catalog.convergence_defaults_for_name("clang_format");
        assert_eq!(clang.priority, 1_000);
        assert_eq!(clang.impact_radius, 2);
        assert_eq!(clang.risk_tier, CatalogConvergenceRiskTier::Stabilizer);

        let naming = catalog.convergence_defaults_for_name("naming_conventions");
        assert_eq!(naming.semantic_priority_weight_bp, 420);
        assert_eq!(naming.risk_tier, CatalogConvergenceRiskTier::Rewrite);

        let unknown = catalog.convergence_defaults_for_name("custom_policy");
        assert_eq!(unknown.priority, 700);
        assert_eq!(unknown.risk_tier, CatalogConvergenceRiskTier::Balanced);
    }

    #[test]
    fn trust_zero_coverage_reduces_but_not_kills() {
        use super::PolicyCertainty;
        let certainty = PolicyCertainty {
            semantic: 0.90,
            coverage: 0.0,
            semantic_variance: 0.002,
            coverage_variance: 0.002,
            stable_model_prob: 0.70,
            edit_success: 0.80,
            edit_success_variance: 0.002,
            ..Default::default()
        };
        let trust = certainty.trust_for_semantic_rewrite();
        assert!(trust > 0.25 && trust < 0.65,
            "zero coverage should reduce trust but not kill it, got {trust}");
    }

    #[test]
    fn trust_high_confidence_yields_high() {
        use super::PolicyCertainty;
        let certainty = PolicyCertainty {
            overall: 0.85,
            semantic: 0.90,
            coverage: 0.85,
            structural: 0.92,
            richness: 0.60,
            semantic_variance: 0.002,
            coverage_variance: 0.002,
            structural_variance: 0.002,
            richness_variance: 0.002,
            stable_model_prob: 0.80,
            transitional_model_prob: 0.10,
            noisy_model_prob: 0.10,
            edit_success: 0.90,
            edit_success_variance: 0.002,
            ..Default::default()
        };
        assert!(certainty.trust_for_semantic_rewrite() > 0.60);
        assert!(certainty.trust_for_structural() > 0.60);
    }

    #[test]
    fn trust_stable_model_boosts() {
        use super::PolicyCertainty;
        let base = PolicyCertainty {
            semantic: 0.70,
            coverage: 0.70,
            structural: 0.70,
            semantic_variance: 0.01,
            coverage_variance: 0.01,
            structural_variance: 0.01,
            stable_model_prob: 0.30,
            edit_success: 0.50,
            edit_success_variance: 0.01,
            ..Default::default()
        };
        let boosted = PolicyCertainty {
            stable_model_prob: 0.90,
            ..base
        };
        assert!(boosted.trust_for_semantic_rewrite() > base.trust_for_semantic_rewrite());
    }

    #[test]
    fn trust_edit_success_boosts() {
        use super::PolicyCertainty;
        let base = PolicyCertainty {
            semantic: 0.70,
            coverage: 0.70,
            structural: 0.70,
            semantic_variance: 0.01,
            coverage_variance: 0.01,
            structural_variance: 0.01,
            stable_model_prob: 0.50,
            edit_success: 0.30,
            edit_success_variance: 0.01,
            ..Default::default()
        };
        let boosted = PolicyCertainty {
            edit_success: 0.95,
            edit_success_variance: 0.001,
            ..base
        };
        assert!(boosted.trust_for_general() > base.trust_for_general());
    }

    #[test]
    fn trust_both_bad_compounds_reduction() {
        use super::PolicyCertainty;
        let good = PolicyCertainty {
            semantic: 0.80,
            coverage: 0.80,
            structural: 0.80,
            semantic_variance: 0.002,
            coverage_variance: 0.002,
            structural_variance: 0.002,
            stable_model_prob: 0.90,
            edit_success: 0.95,
            edit_success_variance: 0.001,
            ..Default::default()
        };
        let bad = PolicyCertainty {
            stable_model_prob: 0.10,
            edit_success: 0.10,
            edit_success_variance: 0.001,
            ..good
        };
        let ratio = bad.trust_for_semantic_rewrite() / good.trust_for_semantic_rewrite();
        assert!(ratio < 0.80, "both bad should compound reduction, ratio={ratio}");
    }
}

// ── Capability matrix (thin dispatch helper) ──────────────────────────────────

pub struct PolicyCapabilityMatrix;

impl PolicyCapabilityMatrix {
    pub fn for_policy(policy_name: &str) -> PolicyCapabilities {
        policy_catalog().capabilities_for_name(policy_name)
    }
}

#[cfg(test)]
mod capability_tests {
    use super::{PolicyCapabilityMatrix, PolicyCertainty};

    #[test]
    fn semantic_rewrite_low_trust_near_zero() {
        let capability = PolicyCapabilityMatrix::for_policy("naming_conventions");
        let certainty = PolicyCertainty {
            overall: 0.85,
            structural: 0.90,
            semantic: 0.30,
            ..Default::default()
        };
        let trust = capability.policy_trust(&certainty);
        assert!(trust < 0.30, "low semantic → low trust: {}", trust);
    }

    #[test]
    fn whitespace_policy_high_trust_under_low_semantic() {
        let capability = PolicyCapabilityMatrix::for_policy("dash_comment_normalizer");
        let certainty = PolicyCertainty {
            overall: 0.40,
            structural: 0.45,
            semantic: 0.10,
            structural_variance: 0.001,
            ..Default::default()
        };
        let trust = capability.policy_trust(&certainty);
        assert!(trust > 0.20, "whitespace policy uses structural, not semantic: {}", trust);
    }
}
