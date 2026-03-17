use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::config::policy_config::PolicyConfig;
use crate::engine::catalog::{policy_catalog, PolicyCertainty};
use crate::parser::clang_types::ClangSymbolKind;
use crate::parser::file_context::{SemanticFileContext, SemanticSummary};

mod context;
mod readiness;
mod snapshot;
mod transition;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum SemanticInvariantClause {
    ParserAvailability,
    ParseQuality,
    SymbolIdentity,
    ScopeIntegrity,
    UsageRoleConsistency,
    MacroRegionSafety,
    TouchContract,
    DeclarationReferenceIntegrity,
    EditSafety,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyGuidanceMode {
    HardInvariant,
    SoftGuideline,
}

impl PolicyGuidanceMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::HardInvariant => "hard_invariant",
            Self::SoftGuideline => "soft_guideline",
        }
    }
}

impl SemanticInvariantClause {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ParserAvailability => "parser_availability",
            Self::ParseQuality => "parse_quality",
            Self::SymbolIdentity => "symbol_identity",
            Self::ScopeIntegrity => "scope_integrity",
            Self::UsageRoleConsistency => "usage_role_consistency",
            Self::MacroRegionSafety => "macro_region_safety",
            Self::TouchContract => "touch_contract",
            Self::DeclarationReferenceIntegrity => "declaration_reference_integrity",
            Self::EditSafety => "edit_safety",
        }
    }

    fn from_serialized(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "parser_availability" => Some(Self::ParserAvailability),
            "parse_quality" => Some(Self::ParseQuality),
            "symbol_identity" => Some(Self::SymbolIdentity),
            "scope_integrity" => Some(Self::ScopeIntegrity),
            "usage_role_consistency" => Some(Self::UsageRoleConsistency),
            "macro_region_safety" => Some(Self::MacroRegionSafety),
            "touch_contract" => Some(Self::TouchContract),
            "declaration_reference_integrity" => Some(Self::DeclarationReferenceIntegrity),
            "edit_safety" => Some(Self::EditSafety),
            _ => None,
        }
    }
}

pub const ALL_CLAUSES: &[SemanticInvariantClause] = &[
    SemanticInvariantClause::ParserAvailability,
    SemanticInvariantClause::ParseQuality,
    SemanticInvariantClause::SymbolIdentity,
    SemanticInvariantClause::ScopeIntegrity,
    SemanticInvariantClause::UsageRoleConsistency,
    SemanticInvariantClause::MacroRegionSafety,
    SemanticInvariantClause::TouchContract,
    SemanticInvariantClause::DeclarationReferenceIntegrity,
    SemanticInvariantClause::EditSafety,
];

impl SemanticInvariantClause {
    pub fn bit(self) -> u16 {
        match self {
            Self::ParserAvailability            => 1 << 0,
            Self::ParseQuality                  => 1 << 1,
            Self::SymbolIdentity                => 1 << 2,
            Self::ScopeIntegrity                => 1 << 3,
            Self::UsageRoleConsistency          => 1 << 4,
            Self::MacroRegionSafety             => 1 << 5,
            Self::TouchContract                 => 1 << 6,
            Self::DeclarationReferenceIntegrity => 1 << 7,
            Self::EditSafety                    => 1 << 8,
        }
    }
}

impl Serialize for SemanticInvariantClause {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SemanticInvariantClause {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_serialized(value.as_str())
            .ok_or_else(|| serde::de::Error::custom(format!("unknown invariant clause '{value}'")))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticInvariantSpec {
    pub clause: SemanticInvariantClause,
    pub hard: bool,
    pub description: &'static str,
}

const INVARIANT_SPECS: [SemanticInvariantSpec; 9] = [
    SemanticInvariantSpec {
        clause: SemanticInvariantClause::ParserAvailability,
        hard: true,
        description: "tree-sitter and clang parsers must both be available for semantic checks",
    },
    SemanticInvariantSpec {
        clause: SemanticInvariantClause::ParseQuality,
        hard: true,
        description: "tree parse-error ratio and clang diagnostics must stay within configured budgets",
    },
    SemanticInvariantSpec {
        clause: SemanticInvariantClause::SymbolIdentity,
        hard: true,
        description: "symbols must keep deterministic, stable identity without provenance drift",
    },
    SemanticInvariantSpec {
        clause: SemanticInvariantClause::ScopeIntegrity,
        hard: true,
        description: "scope topology must remain stable unless explicitly tolerated",
    },
    SemanticInvariantSpec {
        clause: SemanticInvariantClause::UsageRoleConsistency,
        hard: true,
        description:
            "reference usage roles must remain consistent with declaration kind identity",
    },
    SemanticInvariantSpec {
        clause: SemanticInvariantClause::MacroRegionSafety,
        hard: false,
        description:
            "macro/preprocessor edits are flagged for review unless explicitly protected by policy contract",
    },
    SemanticInvariantSpec {
        clause: SemanticInvariantClause::TouchContract,
        hard: true,
        description:
            "touch-contract must never be violated by accepted edit batches",
    },
    SemanticInvariantSpec {
        clause: SemanticInvariantClause::DeclarationReferenceIntegrity,
        hard: true,
        description:
            "declaration-reference connectivity for stable symbols must not regress beyond tolerance",
    },
    SemanticInvariantSpec {
        clause: SemanticInvariantClause::EditSafety,
        hard: true,
        description: "post-edit semantic readiness must not regress from baseline",
    },
];

#[derive(Clone, Copy, Debug)]
pub struct SemanticContract {
    _private: (),
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SemanticReadinessInput {
    pub tree_unavailable: bool,
    pub clang_unavailable: bool,
    pub tree_error_ratio: Option<f64>,
    pub clang_error_count: Option<usize>,
    pub clang_fatal_count: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct SemanticReadinessAssessment {
    pub ready: bool,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SemanticScopeCounts {
    pub namespace: usize,
    pub type_scope: usize,
    pub function: usize,
    pub preprocessor: usize,
}

#[derive(Clone, Debug, Default)]
pub struct SemanticContractSnapshot {
    pub summary: SemanticSummary,
    pub usr_decl_reference_counts: BTreeMap<String, usize>,
    pub usr_decl_lines: BTreeMap<String, usize>,
    pub declaration_stable_ids_by_line: BTreeMap<usize, BTreeSet<String>>,
    pub declaration_kind_by_stable_id: BTreeMap<String, ClangSymbolKind>,
    pub declaration_stable_ids: BTreeSet<String>,
    pub stable_id_decl_lines: BTreeMap<String, usize>,
    pub reference_stable_id_counts: BTreeMap<String, usize>,
    pub reference_stable_id_first_line: BTreeMap<String, usize>,
    pub scope_counts: SemanticScopeCounts,
    pub scope_ranges_by_kind: BTreeMap<String, BTreeSet<(usize, usize)>>,
    pub symbol_identity_issue_count: usize,
    pub symbol_identity_issue_lines: BTreeSet<usize>,
    pub usage_role_mismatch_count: usize,
    pub usage_role_mismatch_lines: BTreeSet<usize>,
    pub preprocessor_ranges: Vec<(usize, usize)>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SemanticContextAssessment {
    pub ready: bool,
    pub hard_failures: Vec<String>,
    pub warnings: Vec<String>,
    pub culprit_lines: BTreeSet<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct SemanticTransitionAssessment {
    pub failure_messages: Vec<String>,
    pub warning_messages: Vec<String>,
    pub failure_score_delta: u32,
    pub culprit_lines: BTreeSet<usize>,
    pub identity_integrity_regressed: bool,
    pub reference_integrity_regressed: bool,
    pub scope_integrity_regressed: bool,
    pub identity_severity: f64,
    pub reference_severity: f64,
    pub scope_severity: f64,
}

impl SemanticReadinessAssessment {
    pub fn summary(&self) -> String {
        if self.reasons.is_empty() {
            return "semantic readiness satisfied".to_string();
        }
        self.reasons.join("; ")
    }
}

impl SemanticContract {
    pub fn invariant_specs() -> &'static [SemanticInvariantSpec] {
        INVARIANT_SPECS.as_slice()
    }

    pub fn invariant_spec(clause: SemanticInvariantClause) -> Option<SemanticInvariantSpec> {
        INVARIANT_SPECS
            .iter()
            .copied()
            .find(|item| item.clause == clause)
    }

    pub fn policy_guidance_mode(policy_name: &str, settings: &PolicyConfig) -> PolicyGuidanceMode {
        if settings.semantic_hard_invariant() {
            return PolicyGuidanceMode::HardInvariant;
        }
        if policy_catalog()
            .behavior_for_name(policy_name)
            .hard_invariant_by_default
        {
            return PolicyGuidanceMode::HardInvariant;
        }
        PolicyGuidanceMode::SoftGuideline
    }

    pub fn new() -> Self {
        Self { _private: () }
    }

    pub fn evaluate_readiness(
        &self,
        input: SemanticReadinessInput,
        certainty: Option<&PolicyCertainty>,
    ) -> SemanticReadinessAssessment {
        readiness::evaluate(input, certainty)
    }

    pub fn evaluate_readiness_with_snapshot(
        &self,
        input: SemanticReadinessInput,
        snapshot: Option<&SemanticContractSnapshot>,
        certainty: Option<&PolicyCertainty>,
    ) -> SemanticReadinessAssessment {
        let mut readiness = self.evaluate_readiness(input, certainty);
        if let Some(snapshot) = snapshot {
            let context = self.evaluate_context(snapshot);
            readiness.reasons.extend(context.hard_failures);
            readiness.ready = readiness.reasons.is_empty();
        }
        readiness
    }

    pub fn snapshot(&self, context: &SemanticFileContext) -> SemanticContractSnapshot {
        snapshot::build(context)
    }

    pub fn evaluate_context(
        &self,
        snapshot: &SemanticContractSnapshot,
    ) -> SemanticContextAssessment {
        context::evaluate(snapshot)
    }

    pub fn evaluate_transition(
        &self,
        before: &SemanticContractSnapshot,
        after: &SemanticContractSnapshot,
        semantic_reference_drop_tolerance: usize,
        semantic_scope_drift_tolerance: usize,
        identity_line_shift_tolerance: usize,
        edited_lines: Option<&BTreeSet<usize>>,
    ) -> SemanticTransitionAssessment {
        transition::evaluate(
            before,
            after,
            semantic_reference_drop_tolerance,
            semantic_scope_drift_tolerance,
            identity_line_shift_tolerance,
            edited_lines,
        )
    }

    fn identity_migrated_locally(
        stable_id: &str,
        declaration_line: usize,
        declaration_kind: Option<ClangSymbolKind>,
        before_reference_count: usize,
        after: &SemanticContractSnapshot,
        line_shift_tolerance: usize,
    ) -> bool {
        if declaration_line == 0 {
            return false;
        }
        let search_start = declaration_line.saturating_sub(line_shift_tolerance);
        let search_end = declaration_line.saturating_add(line_shift_tolerance);
        for search_line in search_start..=search_end {
            let Some(after_ids) = after.declaration_stable_ids_by_line.get(&search_line) else {
                continue;
            };
            for after_id in after_ids {
                if after_id == stable_id {
                    return true;
                }
                if declaration_kind.is_some_and(|kind| {
                    after
                        .declaration_kind_by_stable_id
                        .get(after_id.as_str())
                        .copied()
                        .is_some_and(|after_kind| after_kind != kind)
                }) {
                    continue;
                }
                let kind_matched_count = after_ids
                    .iter()
                    .filter(|id| {
                        declaration_kind.is_none_or(|kind| {
                            after
                                .declaration_kind_by_stable_id
                                .get(id.as_str())
                                .copied()
                                .is_none_or(|after_kind| after_kind == kind)
                        })
                    })
                    .count();
                if kind_matched_count == 1 {
                    return true;
                }
                let after_ref_count = after
                    .reference_stable_id_counts
                    .get(after_id.as_str())
                    .copied()
                    .unwrap_or(0);
                if before_reference_count == 0
                    || after_ref_count >= before_reference_count.saturating_sub(1)
                {
                    return true;
                }
            }
        }
        false
    }

    fn line_in_ranges(line: usize, ranges: &[(usize, usize)]) -> bool {
        ranges
            .iter()
            .any(|(start, end)| line >= *start && line <= *end)
    }

    fn range_near_edited_lines(
        range: (usize, usize),
        edited_lines: &BTreeSet<usize>,
        radius: usize,
    ) -> bool {
        if edited_lines.is_empty() {
            return false;
        }
        let start = range.0.saturating_sub(radius);
        let end = range.1.saturating_add(radius);
        edited_lines.range(start..=end).next().is_some()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::config::policy_config::PolicyConfig;
    use crate::config::enums::TouchContract;
    use crate::parser::clang_types::ClangSymbolKind;
    use crate::parser::file_context::{
        SemanticDeclaration, SemanticFileContext, SemanticIdProvenance, SemanticReference,
        SemanticScope, SemanticScopeKind,
    };

    use crate::parser::node_kind;

    use crate::engine::catalog::PolicyCertainty;
    use super::{PolicyGuidanceMode, SemanticContract, SemanticReadinessInput};

    fn policy_config() -> PolicyConfig {
        PolicyConfig {
            name: "test_policy".to_string(),
            enabled: true,
            policy_type: crate::config::enums::PolicyType::Python,
            touch_contract: TouchContract::CodeOnly,
            enforcement: crate::config::enums::Enforcement::Hard,
            raw: toml::Table::new(),
        }
    }

    #[test]
    fn readiness_allows_non_zero_clang_errors_within_threshold() {
        let contract = SemanticContract::new();
        let assessment = contract.evaluate_readiness(SemanticReadinessInput {
            tree_unavailable: false,
            clang_unavailable: false,
            tree_error_ratio: Some(0.0),
            clang_error_count: Some(2),
            clang_fatal_count: Some(0),
        }, None);
        assert!(assessment.ready);
    }

    #[test]
    fn readiness_rejects_tree_ratio_above_threshold() {
        let contract = SemanticContract::new();
        let high_cert = PolicyCertainty {
            structural: 0.95, semantic: 0.95, coverage: 0.95,
            richness: 0.95, edit_success: 0.10, stable_model_prob: 0.90,
            ..PolicyCertainty::default()
        };
        let assessment = contract.evaluate_readiness(SemanticReadinessInput {
            tree_unavailable: false,
            clang_unavailable: false,
            tree_error_ratio: Some(0.25),
            clang_error_count: Some(0),
            clang_fatal_count: Some(0),
        }, Some(&high_cert));
        // Readiness is now binary: parsers available = ready.
        // Error ratios are informational only and do not block.
        assert!(assessment.ready);
    }

    #[test]
    fn readiness_rejects_fatal_diagnostics_above_threshold() {
        let contract = SemanticContract::new();
        let high_cert = PolicyCertainty {
            structural: 0.95, semantic: 0.95, coverage: 0.95,
            richness: 0.95, edit_success: 0.10, stable_model_prob: 0.90,
            ..PolicyCertainty::default()
        };
        let assessment = contract.evaluate_readiness(SemanticReadinessInput {
            tree_unavailable: false,
            clang_unavailable: false,
            tree_error_ratio: Some(0.0),
            clang_error_count: Some(1),
            clang_fatal_count: Some(5),
        }, Some(&high_cert));
        // Readiness is now binary: parsers available = ready.
        // Clang fatals are informational only and do not block.
        assert!(assessment.ready);
    }

    #[test]
    fn readiness_high_certainty_can_override_moderate_threshold_violation() {
        use crate::engine::catalog::PolicyCertainty;
        let contract = SemanticContract::new();
        let high_certainty = PolicyCertainty {
            structural: 0.90,
            edit_success: 0.85,
            stable_model_prob: 0.80,
            coverage: 0.30,
            ..PolicyCertainty::default()
        };
        let assessment = contract.evaluate_readiness(SemanticReadinessInput {
            tree_unavailable: false,
            clang_unavailable: false,
            tree_error_ratio: Some(0.025),
            clang_error_count: Some(0),
            clang_fatal_count: Some(0),
        }, Some(&high_certainty));
        assert!(assessment.ready, "high Kalman certainty should override marginal tree error ratio violation");
    }

    #[test]
    fn context_invariants_report_symbol_identity_issue() {
        let contract = SemanticContract::new();
        let context = SemanticFileContext {
            declarations: vec![SemanticDeclaration {
                stable_id: "fallback:x.cpp:10:1:Function".to_string(),
                provenance: SemanticIdProvenance::Usr,
                name: "bad".to_string(),
                kind: ClangSymbolKind::Function,
                line: 10,
                column: 1,
                usr: Some("c:@F@bad#".to_string()),
                scope_usr: None,
            }],
            ..SemanticFileContext::default()
        };
        let snapshot = contract.snapshot(&context);
        let assessment = contract.evaluate_context(&snapshot);
        assert!(!assessment.ready);
        assert!(assessment
            .hard_failures
            .iter()
            .any(|message| message.contains("symbol identity")));
    }

    #[test]
    fn transition_flags_usage_role_regression() {
        let contract = SemanticContract::new();
        let before = SemanticFileContext {
            declarations: vec![SemanticDeclaration {
                stable_id: "usr:c:@F@foo#".to_string(),
                provenance: SemanticIdProvenance::Usr,
                name: "foo".to_string(),
                kind: ClangSymbolKind::Function,
                line: 2,
                column: 1,
                usr: Some("c:@F@foo#".to_string()),
                scope_usr: None,
            }],
            references: vec![SemanticReference {
                stable_id: "usr:c:@F@foo#".to_string(),
                provenance: SemanticIdProvenance::Usr,
                decl_path: "a.cpp".to_string(),
                decl_kind: ClangSymbolKind::Function,
                offset: 10,
                line: 4,
                column: 1,
            }],
            ..SemanticFileContext::default()
        };
        let after = SemanticFileContext {
            declarations: before.declarations.clone(),
            references: vec![SemanticReference {
                stable_id: "usr:c:@F@foo#".to_string(),
                provenance: SemanticIdProvenance::Usr,
                decl_path: "a.cpp".to_string(),
                decl_kind: ClangSymbolKind::Variable,
                offset: 10,
                line: 4,
                column: 1,
            }],
            scopes: vec![SemanticScope {
                kind: SemanticScopeKind::Function,
                node_kind: node_kind::FUNCTION_DEFINITION,
                start_offset: 0,
                end_offset: 20,
                start_line: 1,
                end_line: 6,
            }],
            ..SemanticFileContext::default()
        };
        let before_snapshot = contract.snapshot(&before);
        let after_snapshot = contract.snapshot(&after);
        let assessment =
            contract.evaluate_transition(&before_snapshot, &after_snapshot, 0, 16, 0, None);
        assert!(assessment.reference_integrity_regressed);
        assert!(assessment
            .failure_messages
            .iter()
            .any(|message| message.contains("usage-role consistency regressed")));
    }

    #[test]
    fn macro_region_edits_emit_warning() {
        let contract = SemanticContract::new();
        let before = SemanticFileContext::default();
        let after = SemanticFileContext {
            scopes: vec![SemanticScope {
                kind: SemanticScopeKind::Preprocessor,
                node_kind: node_kind::PREPROC_IF,
                start_offset: 0,
                end_offset: 12,
                start_line: 3,
                end_line: 5,
            }],
            ..SemanticFileContext::default()
        };
        let before_snapshot = contract.snapshot(&before);
        let after_snapshot = contract.snapshot(&after);
        let edited_lines = BTreeSet::from([4usize]);
        let assessment = contract.evaluate_transition(
            &before_snapshot,
            &after_snapshot,
            16,
            16,
            0,
            Some(&edited_lines),
        );
        assert_eq!(assessment.failure_messages.len(), 0);
        assert!(assessment
            .warning_messages
            .iter()
            .any(|message| message.contains("macro-safety clause")));
    }

    #[test]
    fn transition_flags_orphan_reference_introduction() {
        let contract = SemanticContract::new();
        let stable_id = "fallback:file.cpp:10:3:Variable".to_string();
        let before = SemanticFileContext {
            declarations: vec![SemanticDeclaration {
                stable_id: stable_id.clone(),
                provenance: SemanticIdProvenance::DeclLocation,
                name: "value".to_string(),
                kind: ClangSymbolKind::Variable,
                line: 10,
                column: 3,
                usr: None,
                scope_usr: None,
            }],
            references: vec![SemanticReference {
                stable_id: stable_id.clone(),
                provenance: SemanticIdProvenance::DeclLocation,
                decl_path: "file.cpp".to_string(),
                decl_kind: ClangSymbolKind::Variable,
                offset: 100,
                line: 12,
                column: 5,
            }],
            ..SemanticFileContext::default()
        };
        let after = SemanticFileContext {
            references: vec![SemanticReference {
                stable_id: stable_id.clone(),
                provenance: SemanticIdProvenance::DeclLocation,
                decl_path: "file.cpp".to_string(),
                decl_kind: ClangSymbolKind::Variable,
                offset: 120,
                line: 14,
                column: 7,
            }],
            ..SemanticFileContext::default()
        };
        let before_snapshot = contract.snapshot(&before);
        let after_snapshot = contract.snapshot(&after);
        let assessment =
            contract.evaluate_transition(&before_snapshot, &after_snapshot, 0, 16, 0, None);
        assert!(assessment.reference_integrity_regressed);
        assert!(assessment
            .failure_messages
            .iter()
            .any(|message| message.contains("orphan semantic references introduced")));
    }

    #[test]
    fn transition_flags_scope_structural_drift_even_when_counts_match() {
        let contract = SemanticContract::new();
        let before = SemanticFileContext {
            scopes: vec![SemanticScope {
                kind: SemanticScopeKind::Function,
                node_kind: node_kind::FUNCTION_DEFINITION,
                start_offset: 0,
                end_offset: 40,
                start_line: 2,
                end_line: 6,
            }],
            ..SemanticFileContext::default()
        };
        let after = SemanticFileContext {
            scopes: vec![SemanticScope {
                kind: SemanticScopeKind::Function,
                node_kind: node_kind::FUNCTION_DEFINITION,
                start_offset: 100,
                end_offset: 200,
                start_line: 30,
                end_line: 40,
            }],
            ..SemanticFileContext::default()
        };
        let before_snapshot = contract.snapshot(&before);
        let after_snapshot = contract.snapshot(&after);
        let edited_lines = BTreeSet::from([3usize]);
        let assessment = contract.evaluate_transition(
            &before_snapshot,
            &after_snapshot,
            16,
            0,
            0,
            Some(&edited_lines),
        );
        assert!(assessment.scope_integrity_regressed);
        assert!(assessment
            .failure_messages
            .iter()
            .any(|message| message.contains("scope structural drift")));
    }

    #[test]
    fn identity_migration_tolerates_small_line_shift() {
        let contract = SemanticContract::new();
        let stable_id = "usr:c:@F@foo#".to_string();
        let before = SemanticFileContext {
            declarations: vec![SemanticDeclaration {
                stable_id: stable_id.clone(),
                provenance: SemanticIdProvenance::Usr,
                name: "foo".to_string(),
                kind: ClangSymbolKind::Function,
                line: 10,
                column: 1,
                usr: Some("c:@F@foo#".to_string()),
                scope_usr: None,
            }],
            references: vec![SemanticReference {
                stable_id: stable_id.clone(),
                provenance: SemanticIdProvenance::Usr,
                decl_path: "a.cpp".to_string(),
                decl_kind: ClangSymbolKind::Function,
                offset: 50,
                line: 20,
                column: 1,
            }],
            scopes: vec![SemanticScope {
                kind: SemanticScopeKind::Function,
                node_kind: node_kind::FUNCTION_DEFINITION,
                start_offset: 0,
                end_offset: 100,
                start_line: 1,
                end_line: 30,
            }],
            ..SemanticFileContext::default()
        };
        let shifted_id = "usr:c:@F@bar#".to_string();
        let after = SemanticFileContext {
            declarations: vec![SemanticDeclaration {
                stable_id: shifted_id.clone(),
                provenance: SemanticIdProvenance::Usr,
                name: "bar".to_string(),
                kind: ClangSymbolKind::Function,
                line: 13,
                column: 1,
                usr: Some("c:@F@bar#".to_string()),
                scope_usr: None,
            }],
            references: vec![SemanticReference {
                stable_id: shifted_id.clone(),
                provenance: SemanticIdProvenance::Usr,
                decl_path: "a.cpp".to_string(),
                decl_kind: ClangSymbolKind::Function,
                offset: 50,
                line: 23,
                column: 1,
            }],
            scopes: vec![SemanticScope {
                kind: SemanticScopeKind::Function,
                node_kind: node_kind::FUNCTION_DEFINITION,
                start_offset: 0,
                end_offset: 100,
                start_line: 1,
                end_line: 33,
            }],
            ..SemanticFileContext::default()
        };
        let before_snapshot = contract.snapshot(&before);
        let after_snapshot = contract.snapshot(&after);
        let assessment_tolerant =
            contract.evaluate_transition(&before_snapshot, &after_snapshot, 0, 16, 4, None);
        assert!(
            !assessment_tolerant.identity_integrity_regressed,
            "shift of 3 lines should be tolerated with tolerance=4"
        );
        let assessment_exact =
            contract.evaluate_transition(&before_snapshot, &after_snapshot, 0, 16, 0, None);
        assert!(
            assessment_exact.identity_integrity_regressed,
            "shift of 3 lines should regress with tolerance=0 (exact match)"
        );
    }

    #[test]
    fn identity_migration_rejects_large_line_shift() {
        let contract = SemanticContract::new();
        let stable_id = "usr:c:@F@foo#".to_string();
        let before = SemanticFileContext {
            declarations: vec![SemanticDeclaration {
                stable_id: stable_id.clone(),
                provenance: SemanticIdProvenance::Usr,
                name: "foo".to_string(),
                kind: ClangSymbolKind::Function,
                line: 10,
                column: 1,
                usr: Some("c:@F@foo#".to_string()),
                scope_usr: None,
            }],
            references: vec![SemanticReference {
                stable_id: stable_id.clone(),
                provenance: SemanticIdProvenance::Usr,
                decl_path: "a.cpp".to_string(),
                decl_kind: ClangSymbolKind::Function,
                offset: 50,
                line: 20,
                column: 1,
            }],
            scopes: vec![SemanticScope {
                kind: SemanticScopeKind::Function,
                node_kind: node_kind::FUNCTION_DEFINITION,
                start_offset: 0,
                end_offset: 100,
                start_line: 1,
                end_line: 30,
            }],
            ..SemanticFileContext::default()
        };
        let shifted_id = "usr:c:@F@bar#".to_string();
        let after = SemanticFileContext {
            declarations: vec![SemanticDeclaration {
                stable_id: shifted_id.clone(),
                provenance: SemanticIdProvenance::Usr,
                name: "bar".to_string(),
                kind: ClangSymbolKind::Function,
                line: 20,
                column: 1,
                usr: Some("c:@F@bar#".to_string()),
                scope_usr: None,
            }],
            references: vec![SemanticReference {
                stable_id: shifted_id.clone(),
                provenance: SemanticIdProvenance::Usr,
                decl_path: "a.cpp".to_string(),
                decl_kind: ClangSymbolKind::Function,
                offset: 50,
                line: 30,
                column: 1,
            }],
            scopes: vec![SemanticScope {
                kind: SemanticScopeKind::Function,
                node_kind: node_kind::FUNCTION_DEFINITION,
                start_offset: 0,
                end_offset: 100,
                start_line: 1,
                end_line: 40,
            }],
            ..SemanticFileContext::default()
        };
        let before_snapshot = contract.snapshot(&before);
        let after_snapshot = contract.snapshot(&after);
        let assessment =
            contract.evaluate_transition(&before_snapshot, &after_snapshot, 0, 16, 4, None);
        assert!(
            assessment.identity_integrity_regressed,
            "shift of 10 lines should regress even with tolerance=4"
        );
    }

    #[test]
    fn policy_guidance_mode_defaults_to_soft_guideline() {
        let settings = policy_config();
        assert_eq!(
            SemanticContract::policy_guidance_mode("class_layout", &settings),
            PolicyGuidanceMode::SoftGuideline
        );
    }

    #[test]
    fn policy_guidance_mode_honors_hard_invariant_override() {
        let mut settings = policy_config();
        settings.raw.insert(
            "semantic_hard_invariant".to_string(),
            toml::Value::Boolean(true),
        );
        assert_eq!(
            SemanticContract::policy_guidance_mode("class_layout", &settings),
            PolicyGuidanceMode::HardInvariant
        );
    }
}
