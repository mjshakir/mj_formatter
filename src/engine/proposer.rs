use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use smallvec::SmallVec;

use crate::engine::convergence::ConvergencePolicySignal;
use crate::engine::catalog::PolicyCapabilities;
use crate::engine::edit_candidate::PolicyEditCandidate;
use crate::policy::zone::PolicyZone;
use crate::engine::semantic_contract::{SemanticContract, SemanticInvariantClause, ALL_CLAUSES};
use crate::model::policy_result::PolicyResult;
use crate::model::project_query::ProjectContextQuery;
use crate::parser::file_context::SemanticScopeKind;
use crate::parser::semantic_region::SemanticRegionKind;

#[derive(Clone, Debug, Default)]
pub struct GuardianAssessment {
    pub allowed: Vec<PolicyEditCandidate>,
    pub blocked_zone_lines: BTreeSet<usize>,
    pub hard_blocked_lines: BTreeSet<usize>,
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
        adaptive: &crate::engine::certainty_filter::CertaintyFilterState,
    ) -> Vec<PolicyEditCandidate> {
        let mut candidates = Vec::<PolicyEditCandidate>::with_capacity(result.edits.len());
        for edit in &result.edits {
            if edit.line == 0 || edit.before == edit.after {
                continue;
            }
            let zone = Self::zone_for_line(edit.line, project_query, comment_lines);
            let mut hard_constraints_touched: u16 = 0;
            if project_query.is_macro_region(edit.line, 1) {
                hard_constraints_touched |= SemanticInvariantClause::MacroRegionSafety.bit();
            }
            if !project_query.is_safe_edit(edit.line, 1) {
                hard_constraints_touched |= SemanticInvariantClause::EditSafety.bit();
            }
            if !project_query.is_available() {
                hard_constraints_touched |= SemanticInvariantClause::ParserAvailability.bit();
            }
            if !capability.allows_zone(zone) {
                hard_constraints_touched |= SemanticInvariantClause::TouchContract.bit();
            }
            let trust_deficit_penalty = adaptive.trust_deficit_penalty();
            let mut symbol_footprint: SmallVec<[u64; 8]> = convergence_signal
                .symbol_ids
                .get(&edit.line)
                .map(|v| v.iter().copied().collect())
                .unwrap_or_default();
            if let Some(symbol) = project_query.symbol_at(edit.line, 1, &[]) {
                if project_query
                    .decl_by_id(symbol.stable_id.as_str())
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
            let range_footprint: SmallVec<[(usize, usize); 4]> = convergence_signal
                .impact_ranges
                .get(&edit.line)
                .cloned()
                .unwrap_or_else(|| smallvec::smallvec![(edit.line, edit.line)]);
            let confidence_penalty = adaptive.confidence_penalty();
            let resolved_confidence =
                (confidence - confidence_penalty - trust_deficit_penalty).clamp(0.0, 1.0);
            let mut impact_radius = convergence_signal.impact_radius.max(1);
            let project_signal = project_query
                .symbol_at(edit.line, 1, &[])
                .and_then(|symbol| {
                    project_query.signal(symbol.stable_id.as_str())
                })
                .or_else(|| project_query.signal((edit.line, 1)));
            let richness_multiplier = adaptive.richness_multiplier();
            let richness_radius = richness_multiplier.round().clamp(1.0, 3.0) as usize;
            impact_radius = impact_radius.max(richness_radius);
            let _ = project_signal;
            candidates.push(PolicyEditCandidate {
                policy: Arc::from(policy_name),
                line: edit.line,
                confidence: resolved_confidence,
                risk_tier: capability.risk_tier,
                impact_radius,
                symbol_footprint: symbol_footprint.into_vec().into(),
                range_footprint: range_footprint.into_vec().into(),
                hard_constraints_touched,
                zone,
                after_fingerprint: Self::text_fingerprint(edit.after.as_str()),
                style_gain: Self::style_gain(edit.before.as_str(), edit.after.as_str(), adaptive),
            });
        }
        candidates
    }

    pub fn assess(
        candidates: &[PolicyEditCandidate],
        capability: &PolicyCapabilities,
    ) -> GuardianAssessment {
        let zone_relaxed = false;
        let mut allowed = Vec::<PolicyEditCandidate>::new();
        let mut blocked_zone_lines = BTreeSet::<usize>::new();
        let mut hard_blocked_lines = BTreeSet::<usize>::new();
        for candidate in candidates {
            if !capability.allows_zone(candidate.zone) {
                let zone_ok = zone_relaxed
                    && candidate.zone != crate::policy::zone::PolicyZone::Preprocessor;
                if !zone_ok {
                    blocked_zone_lines.insert(candidate.line);
                    continue;
                }
            }
            let has_hard_constraint = ALL_CLAUSES.iter()
                .filter(|&&clause| (candidate.hard_constraints_touched & clause.bit()) != 0)
                .any(|&clause| {
                    let spec_hard = SemanticContract::invariant_spec(clause)
                        .map(|spec| spec.hard)
                        .unwrap_or(true);
                    if !spec_hard {
                        return false;
                    }
                    true
                });
            if has_hard_constraint {
                hard_blocked_lines.insert(candidate.line);
                continue;
            }
            allowed.push(candidate.clone());
        }
        GuardianAssessment {
            allowed,
            blocked_zone_lines,
            hard_blocked_lines,
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
        let mut hasher = rustc_hash::FxHasher::default();
        value.hash(&mut hasher);
        hasher.finish()
    }

    fn line_column_fingerprint(line: usize, column: usize) -> u64 {
        Self::text_fingerprint(format!("{line}:{column}").as_str())
    }

    fn style_gain(before: &str, after: &str, adaptive: &crate::engine::certainty_filter::CertaintyFilterState) -> f64 {
        if before == after {
            return 0.0;
        }
        let before_trimmed = before.trim();
        let after_trimmed = after.trim();
        let whitespace_change = (before_trimmed == after_trimmed) as u8 as f64;
        let delta = before_trimmed.len().abs_diff(after_trimmed.len()) as f64;
        let normalized_delta = (delta / 32.0).min(1.0);
        let (base, ws_bonus, delta_bonus) = adaptive.style_gain_weights();
        base + (whitespace_change * ws_bonus) + ((1.0 - normalized_delta) * delta_bonus)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::engine::convergence::ConvergencePolicySignal;
    use crate::engine::catalog::PolicyCapabilityMatrix;
    use crate::engine::proposer::ProposerController;
    use crate::model::edit::Edit;
    use crate::model::policy_result::PolicyResult;
    use crate::model::project_query::ProjectContextQuery;
    use crate::model::context_query::SemanticContextQuery;
    use crate::parser::file_context::{
        SemanticFileContext, SemanticScope, SemanticScopeKind,
    };
    #[test]
    fn marks_protected_zone() {
        let semantic = SemanticFileContext {
            scopes: vec![SemanticScope {
                kind: SemanticScopeKind::Preprocessor,
                node_kind_id: crate::parser::ts_cpp_symbols::sym_preproc_if,
                start_offset: 0,
                end_offset: 40,
                start_line: 2,
                end_line: 2,
            }],
            ..SemanticFileContext::default()
        };
        let semantic_query = SemanticContextQuery::from_semantic(Some(&semantic));
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
            changed: true,
        };
        let capability = PolicyCapabilityMatrix::for_policy("compact_declarations");
        let adaptive = crate::engine::certainty_filter::CertaintyFilterState::new();
        let candidates = ProposerController::new().propose(
            "compact_declarations",
            &result,
            &project_query,
            Some(&BTreeSet::new()),
            &ConvergencePolicySignal::default(),
            &capability,
            0.9,
            &adaptive,
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].zone.as_str(), "preprocessor");
    }
}
