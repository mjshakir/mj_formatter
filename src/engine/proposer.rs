use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};

use crate::engine::convergence::ConvergencePolicySignal;
use crate::engine::catalog::{PolicyCapabilities, PolicyCertainty};
use crate::engine::edit_candidate::PolicyEditCandidate;
use crate::engine::zone::PolicyZone;
use crate::engine::semantic_contract::{SemanticContract, SemanticInvariantClause};
use crate::model::policy_result::PolicyResult;
use crate::model::project_query::ProjectContextQuery;
use crate::parser::file_context::SemanticScopeKind;
use crate::parser::semantic_region::SemanticRegionKind;

#[derive(Clone, Debug, Default)]
pub struct GuardianAssessment {
    pub allowed: Vec<PolicyEditCandidate>,
    pub blocked_zone_lines: BTreeSet<usize>,
    pub blocked_hard_constraint_lines: BTreeSet<usize>,
}

#[derive(Default)]
pub struct ProposerController;

impl ProposerController {
    pub fn new() -> Self {
        Self
    }

    #[allow(clippy::too_many_arguments)]
    pub fn propose(
        &self,
        policy_name: &str,
        result: &PolicyResult,
        project_query: &ProjectContextQuery<'_>,
        comment_lines: Option<&BTreeSet<usize>>,
        convergence_signal: &ConvergencePolicySignal,
        capability: &PolicyCapabilities,
        confidence: f64,
        certainty: &PolicyCertainty,
    ) -> Vec<PolicyEditCandidate> {
        let mut candidates = Vec::<PolicyEditCandidate>::new();
        for edit in &result.edits {
            if edit.line == 0 || edit.before == edit.after {
                continue;
            }
            let zone = Self::zone_for_line(edit.line, project_query, comment_lines);
            let mut hard_constraints_touched = BTreeSet::<SemanticInvariantClause>::new();
            if project_query.is_macro_region(edit.line, 1) {
                hard_constraints_touched.insert(SemanticInvariantClause::MacroRegionSafety);
            }
            if !project_query.is_safe_edit(edit.line, 1) {
                hard_constraints_touched.insert(SemanticInvariantClause::EditSafety);
            }
            if !project_query.is_available() {
                hard_constraints_touched.insert(SemanticInvariantClause::ParserAvailability);
            }
            if !capability.allows_zone(zone) {
                hard_constraints_touched.insert(SemanticInvariantClause::TouchContract);
            }
            let trust_deficit_penalty = if capability.semantic_rewrite {
                let trust = capability.policy_trust(certainty);
                crate::engine::fuzzy_inference::fuzzy_trust_deficit_penalty(trust)
            } else {
                0.0
            };
            let mut symbol_footprint = convergence_signal
                .semantic_symbol_ids_by_line
                .get(&edit.line)
                .cloned()
                .unwrap_or_default();
            if let Some(symbol) = project_query.symbol_at(edit.line, 1, &[]) {
                if project_query
                    .declaration_by_stable_id(symbol.stable_id.as_str())
                    .is_some()
                {
                    symbol_footprint.push(Self::text_fingerprint(symbol.stable_id.as_str()));
                }
                // No hard constraint for unresolved symbols: without exact compile_commands.json,
                // headers commonly have symbols that can't be resolved to declarations in the
                // current parse context. Blocking based on non-information prevents valid edits.
                for reference in project_query.references_of(symbol.stable_id.as_str()) {
                    symbol_footprint.push(Self::line_column_fingerprint(
                        reference.line,
                        reference.column,
                    ));
                }
            }
            symbol_footprint.sort_unstable();
            symbol_footprint.dedup();
            let range_footprint = convergence_signal
                .semantic_impact_ranges_by_line
                .get(&edit.line)
                .cloned()
                .unwrap_or_else(|| vec![(edit.line, edit.line)]);
            let trust_for_penalty = certainty.trust_for_general();
            let confidence_penalty = crate::engine::fuzzy_inference::fuzzy_constraint_penalty(
                hard_constraints_touched.len(), trust_for_penalty,
            );
            let mut resolved_confidence =
                (confidence - confidence_penalty - trust_deficit_penalty).clamp(0.0, 1.0);
            let mut impact_radius = convergence_signal.impact_radius.max(1);
            let project_signal = project_query
                .symbol_at(edit.line, 1, &[])
                .and_then(|symbol| {
                    project_query.project_signal_for_stable_id(symbol.stable_id.as_str())
                })
                .or_else(|| project_query.project_signal_for_line(edit.line, 1));
            let richness_multiplier =
                crate::engine::fuzzy_inference::fuzzy_richness_radius_multiplier(certainty.trust_for_general());
            let richness_radius =
                (certainty.richness_lower_ci() * richness_multiplier).round().clamp(1.0, 3.0) as usize;
            impact_radius = impact_radius.max(richness_radius);
            if let Some(signal) = project_signal {
                let consensus_weakness = crate::engine::fuzzy_inference::TrapezoidalMF::new(0.0, 0.0, 0.65, 0.85)
                    .membership(signal.consensus_score);
                let deficit = certainty.semantic_variance.sqrt().clamp(0.0, 0.5);
                let trust = capability.policy_trust(certainty);
                let penalty = crate::engine::fuzzy_inference::fuzzy_constraint_penalty(
                    ((consensus_weakness * deficit * 10.0).round() as usize).min(5),
                    trust,
                );
                resolved_confidence = (resolved_confidence - penalty).clamp(0.0, 1.0);
            }
            candidates.push(PolicyEditCandidate {
                policy: policy_name.into(),
                line: edit.line,
                confidence: resolved_confidence,
                risk_tier: capability.risk_tier,
                impact_radius,
                symbol_footprint,
                range_footprint,
                hard_constraints_touched,
                zone,
                after_fingerprint: Self::text_fingerprint(edit.after.as_str()),
                style_gain: Self::style_gain(edit.before.as_str(), edit.after.as_str()),
            });
        }
        candidates
    }

    pub fn assess(
        candidates: &[PolicyEditCandidate],
        capability: &PolicyCapabilities,
        certainty: Option<&crate::engine::catalog::PolicyCertainty>,
    ) -> GuardianAssessment {
        let zone_relaxed =
            crate::engine::fuzzy_inference::fuzzy_guardian_zone_relax(certainty);
        let mut allowed = Vec::<PolicyEditCandidate>::new();
        let mut blocked_zone_lines = BTreeSet::<usize>::new();
        let mut blocked_hard_constraint_lines = BTreeSet::<usize>::new();
        for candidate in candidates {
            if !capability.allows_zone(candidate.zone) {
                let zone_ok = zone_relaxed
                    && candidate.zone != crate::engine::zone::PolicyZone::Preprocessor;
                if !zone_ok {
                    blocked_zone_lines.insert(candidate.line);
                    continue;
                }
            }
            let has_hard_constraint = candidate.hard_constraints_touched.iter().any(|clause| {
                let spec_hard = SemanticContract::invariant_spec(*clause)
                    .map(|spec| spec.hard)
                    .unwrap_or(true);
                if !spec_hard {
                    return false;
                }
                !crate::engine::fuzzy_inference::fuzzy_hard_constraint_override(certainty, *clause)
            });
            if has_hard_constraint {
                blocked_hard_constraint_lines.insert(candidate.line);
                continue;
            }
            allowed.push(candidate.clone());
        }
        GuardianAssessment {
            allowed,
            blocked_zone_lines,
            blocked_hard_constraint_lines,
        }
    }

    fn zone_for_line(
        line: usize,
        project_query: &ProjectContextQuery<'_>,
        comment_lines: Option<&BTreeSet<usize>>,
    ) -> PolicyZone {
        if project_query
            .regions_for_line(line)
            .iter()
            .any(|region| region.kind == SemanticRegionKind::Preprocessor)
        {
            return PolicyZone::Preprocessor;
        }
        if project_query
            .region_at(line, 1)
            .is_some_and(|region| region.kind == SemanticRegionKind::Preprocessor)
        {
            return PolicyZone::Preprocessor;
        }
        if project_query
            .scope_at(line, 1)
            .is_some_and(|scope| scope.kind == SemanticScopeKind::Preprocessor)
        {
            return PolicyZone::Preprocessor;
        }
        if project_query.is_macro_region(line, 1) {
            return PolicyZone::Preprocessor;
        }
        if comment_lines.is_some_and(|lines| lines.contains(&line)) {
            return PolicyZone::Comments;
        }
        PolicyZone::Code
    }

    fn text_fingerprint(value: &str) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    fn line_column_fingerprint(line: usize, column: usize) -> u64 {
        Self::text_fingerprint(format!("{line}:{column}").as_str())
    }

    fn style_gain(before: &str, after: &str) -> f64 {
        if before == after {
            return 0.0;
        }
        let before_trimmed = before.trim();
        let after_trimmed = after.trim();
        let whitespace_change = (before_trimmed == after_trimmed) as u8 as f64;
        let delta = before_trimmed.len().abs_diff(after_trimmed.len()) as f64;
        let normalized_delta = (delta / 32.0).min(1.0);
        let (base, ws_bonus, delta_bonus) =
            crate::engine::fuzzy_inference::fuzzy_style_gain_weights(
                crate::engine::fuzzy_inference::DEFAULT_TRUST,
            );
        base + (whitespace_change * ws_bonus) + ((1.0 - normalized_delta) * delta_bonus)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::engine::convergence::ConvergencePolicySignal;
    use crate::engine::catalog::{PolicyCapabilityMatrix, PolicyCertainty};
    use crate::engine::proposer::ProposerController;
    use crate::model::edit::Edit;
    use crate::model::policy_result::PolicyResult;
    use crate::model::project_query::ProjectContextQuery;
    use crate::model::context_query::SemanticContextQuery;
    use crate::parser::file_context::{
        SemanticFileContext, SemanticScope, SemanticScopeKind,
    };
    use crate::parser::node_kind;

    #[test]
    fn proposer_marks_preprocessor_lines_as_protected_zone() {
        let semantic = SemanticFileContext {
            scopes: vec![SemanticScope {
                kind: SemanticScopeKind::Preprocessor,
                node_kind: node_kind::PREPROC_IF,
                start_offset: 0,
                end_offset: 40,
                start_line: 2,
                end_line: 2,
            }],
            ..SemanticFileContext::default()
        };
        let semantic_query = SemanticContextQuery::from_semantic_file_context(Some(&semantic));
        let project_query = ProjectContextQuery::new(semantic_query.clone(), None);
        let result = PolicyResult {
            text: "#if 1\nx\n#endif\n".to_string(),
            violations: Vec::new(),
            edits: vec![Edit {
                policy: "compact_declarations".into(),
                line: 2,
                before: "x".to_string(),
                after: "x ".to_string(),
            }],
            warnings: Vec::new(),
        };
        let capability = PolicyCapabilityMatrix::for_policy("compact_declarations");
        let certainty = PolicyCertainty {
            overall: 0.9,
            structural: 0.9,
            semantic: 0.9,
            ..Default::default()
        };
        let candidates = ProposerController::new().propose(
            "compact_declarations",
            &result,
            &project_query,
            Some(&BTreeSet::new()),
            &ConvergencePolicySignal::default(),
            &capability,
            0.9,
            &certainty,
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].zone.as_str(), "preprocessor");
    }
}
