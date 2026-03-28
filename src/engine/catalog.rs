use rustc_hash::FxHashMap;
use std::sync::OnceLock;

use crate::engine::edit_candidate::CandidateRiskTier;
use crate::policy::zone::PolicyZone;
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
    pub clang_invalidating: bool,
    pub macro_sensitive: bool,
    pub allowed_zones: &'static [PolicyZone],
    pub risk_tier: CandidateRiskTier,
}

impl PolicyCapabilities {
    pub fn allows_zone(&self, zone: PolicyZone) -> bool {
        self.allowed_zones.contains(&zone)
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
    pub priority_weight_bp: u16,
    pub risk_tier: CatalogConvergenceRiskTier,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PolicyBehavior {
    pub needs_exact_compdb: bool,
    pub keeps_nonlocal_batch: bool,
    pub advisory_on_retry: bool,
    pub hard_invariant: bool,
    pub bypasses_line_conflict: bool,
    pub execution_priority: u8,
}

#[derive(Clone, Copy, Debug)]
struct PolicyConvergenceTemplate {
    priority: u16,
    impact_radius: usize,
    priority_weight_bp: u16,
    risk_tier: CatalogConvergenceRiskTier,
}

impl Default for PolicyConvergenceTemplate {
    fn default() -> Self {
        Self {
            priority: 700,
            impact_radius: 0,
            priority_weight_bp: 240,
            risk_tier: CatalogConvergenceRiskTier::Balanced,
        }
    }
}

impl Default for PolicyBehavior {
    fn default() -> Self {
        Self {
            needs_exact_compdb: false,
            keeps_nonlocal_batch: false,
            advisory_on_retry: false,
            hard_invariant: false,
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
    known: FxHashMap<PolicyId, PolicyCatalogEntry>,
    default_capabilities: PolicyCapabilities,
    default_convergence: PolicyConvergenceTemplate,
    default_behavior: PolicyBehavior,
}

pub trait CatalogKey {
    fn to_policy_id(&self) -> PolicyId;
}
impl CatalogKey for &str {
    fn to_policy_id(&self) -> PolicyId { PolicyId::from_str_lossy(self) }
}
impl CatalogKey for &PolicyId {
    fn to_policy_id(&self) -> PolicyId { (*self).clone() }
}
impl CatalogKey for PolicyId {
    fn to_policy_id(&self) -> PolicyId { self.clone() }
}

impl PolicyCatalog {
    fn build() -> Self {
        let mut known: FxHashMap<PolicyId, PolicyCatalogEntry> = FxHashMap::default();

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
                clang_invalidating: false,
                macro_sensitive: true,
                allowed_zones: &ZONES_CODE_ONLY,
                risk_tier: CandidateRiskTier::Medium,
            },
            default_convergence: PolicyConvergenceTemplate::default(),
            default_behavior: PolicyBehavior::default(),
        }
    }

    pub fn capabilities(&self, key: impl CatalogKey) -> PolicyCapabilities {
        let id = key.to_policy_id();
        self.known
            .get(&id)
            .map(|entry| entry.capabilities)
            .unwrap_or(self.default_capabilities)
    }

    pub fn convergence(&self, key: impl CatalogKey) -> PolicyConvergenceDefaults {
        let id = key.to_policy_id();
        if let Some(entry) = self.known.get(&id) {
            return PolicyConvergenceDefaults {
                domain: entry.name.to_string(),
                priority: entry.convergence.priority,
                impact_radius: entry.convergence.impact_radius,
                priority_weight_bp: entry.convergence.priority_weight_bp,
                risk_tier: entry.convergence.risk_tier,
            };
        }
        PolicyConvergenceDefaults {
            domain: id.as_str().to_string(),
            priority: self.default_convergence.priority,
            impact_radius: self.default_convergence.impact_radius,
            priority_weight_bp: self.default_convergence.priority_weight_bp,
            risk_tier: self.default_convergence.risk_tier,
        }
    }

    pub fn behavior(&self, key: impl CatalogKey) -> PolicyBehavior {
        let id = key.to_policy_id();
        self.known
            .get(&id)
            .map(|entry| entry.behavior)
            .unwrap_or(self.default_behavior)
    }

    pub fn bypasses_line_conflict(&self, policy_name: &str) -> bool {
        self.behavior(policy_name).bypasses_line_conflict
    }

    pub fn is_semantic_rewrite(&self, key: impl CatalogKey) -> bool {
        self.capabilities(key).semantic_rewrite
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
                    clang_invalidating: false,
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
                clang_invalidating: false,
                macro_sensitive: true,
                allowed_zones: &ZONES_CODE_AND_PREPROC,
                risk_tier: CandidateRiskTier::Medium,
            },
            PolicyId::ClangFormat => PolicyCapabilities {
                whitespace_safe: false,
                structural_safe: true,
                semantic_rewrite: false,
                clang_invalidating: false,
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
                clang_invalidating: false,
                macro_sensitive: true,
                allowed_zones: &ZONES_CODE_ONLY,
                risk_tier: CandidateRiskTier::Medium,
            },
            PolicyId::LogicalKeywordOperators => PolicyCapabilities {
                whitespace_safe: false,
                structural_safe: true,
                semantic_rewrite: false,
                clang_invalidating: false,
                macro_sensitive: true,
                allowed_zones: &ZONES_CODE_ONLY,
                risk_tier: CandidateRiskTier::Medium,
            },
            PolicyId::NamingConventions
            | PolicyId::SnakeCase
            | PolicyId::OperatorOverloadSpacing => PolicyCapabilities {
                whitespace_safe: false,
                structural_safe: false,
                semantic_rewrite: true,
                clang_invalidating: false,
                macro_sensitive: true,
                allowed_zones: &ZONES_CODE_ONLY,
                risk_tier: CandidateRiskTier::High,
            },
            PolicyId::FunctionVoidParams => PolicyCapabilities {
                whitespace_safe: false,
                structural_safe: false,
                semantic_rewrite: true,
                clang_invalidating: true,
                macro_sensitive: true,
                allowed_zones: &ZONES_CODE_ONLY,
                risk_tier: CandidateRiskTier::High,
            },
            PolicyId::NumericLiteralSuffix => PolicyCapabilities {
                whitespace_safe: false,
                structural_safe: true,
                semantic_rewrite: false,
                clang_invalidating: false,
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
            priority_weight_bp: match policy_id {
                PolicyId::NamingConventions => 420,
                PolicyId::FunctionVoidParams => 300,
                _ => 240,
            },
            risk_tier: convergence_risk_tier,
        };

        let behavior = PolicyBehavior {
            needs_exact_compdb: matches!(
                policy_id,
                PolicyId::ClangFormat
                    | PolicyId::IncludeOrder
                    | PolicyId::ClassLayout
                    | PolicyId::CompactDeclarations
                    | PolicyId::DashCommentNormalizer
            ),
            keeps_nonlocal_batch: matches!(policy_id, PolicyId::ClangFormat),
            advisory_on_retry: matches!(policy_id, PolicyId::ClangFormat),
            hard_invariant: false,
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

pub struct PolicyCapabilityMatrix;

impl PolicyCapabilityMatrix {
    pub fn for_policy(policy_name: &str) -> PolicyCapabilities {
        policy_catalog().capabilities(policy_name)
    }
}

#[cfg(test)]
mod tests {
    use crate::engine::catalog::{
        policy_catalog, CatalogConvergenceRiskTier, PolicyCatalog,
    };
    use crate::engine::edit_candidate::CandidateRiskTier;
    use crate::policy::id::PolicyId;

    fn all_policy_ids() -> Vec<PolicyId> {
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
    fn contains_all_ids() {
        let catalog = PolicyCatalog::build();
        for id in all_policy_ids() {
            assert!(
                catalog.known.contains_key(&id),
                "missing catalog entry for {}",
                id.as_str()
            );
        }
    }

    #[test]
    fn flags_match_expected() {
        let catalog = policy_catalog();
        assert!(
            catalog
                .behavior("clang_format")
                .needs_exact_compdb
        );
        assert_eq!(
            catalog
                .behavior("naming_conventions")
                .execution_priority,
            10
        );
        assert_eq!(
            catalog.behavior("clang_format").execution_priority,
            90
        );
        assert!(
            catalog
                .behavior("clang_format")
                .keeps_nonlocal_batch
        );
        assert!(
            catalog
                .behavior("clang_format")
                .advisory_on_retry
        );
        assert!(
            !catalog
                .behavior("dash_comment_normalizer")
                .keeps_nonlocal_batch
        );
    }

    #[test]
    fn rewrite_flag_truth() {
        let catalog = policy_catalog();
        let semantic = catalog.capabilities("naming_conventions");
        assert!(semantic.semantic_rewrite);
        assert_eq!(semantic.risk_tier, CandidateRiskTier::High);

        let stable = catalog.capabilities("dash_comment_normalizer");
        assert!(!stable.semantic_rewrite);
        assert_eq!(stable.risk_tier, CandidateRiskTier::Low);
    }

    #[test]
    fn defaults_stable_anchors() {
        let catalog = policy_catalog();
        let clang = catalog.convergence("clang_format");
        assert_eq!(clang.priority, 1_000);
        assert_eq!(clang.impact_radius, 2);
        assert_eq!(clang.risk_tier, CatalogConvergenceRiskTier::Stabilizer);

        let naming = catalog.convergence("naming_conventions");
        assert_eq!(naming.priority_weight_bp, 420);
        assert_eq!(naming.risk_tier, CatalogConvergenceRiskTier::Rewrite);

        let unknown = catalog.convergence("custom_policy");
        assert_eq!(unknown.priority, 700);
        assert_eq!(unknown.risk_tier, CatalogConvergenceRiskTier::Balanced);
    }

}
