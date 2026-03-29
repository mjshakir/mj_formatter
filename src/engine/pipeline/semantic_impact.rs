use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};

use rustc_hash::FxHashMap;
use smallvec::{smallvec, SmallVec};

use crate::config::policy_config::PolicyConfig;
use crate::engine::catalog::PolicyCapabilities;
use crate::engine::convergence::ConvergenceController;
use crate::engine::convergence::ConvergencePolicyProfile;
use crate::engine::convergence::ConvergencePolicySignal;
use crate::engine::convergence::ConvergenceRiskTier;
use crate::engine::semantic_contract::PolicyGuidanceMode;
use crate::engine::semantic_contract::SemanticInvariantClause;
use crate::model::policy_result::PolicyResult;
use crate::model::context_query::SemanticContextQuery;
use crate::model::violation::Violation;
use crate::parser::file_context::SemanticFileContext;
use crate::parser::manager::SemanticCompdbContextKind;
use crate::parser::semantic_region::{SemanticRegion, SemanticRegionKind};
use crate::policy::zone::PolicyZone;

use super::{
    ConvergenceSignalInput, PolicyPipeline, SemanticGuidanceConfig,
    CONVERGENCE_MAX_IMPACT_RANGES_PER_LINE,
};

impl PolicyPipeline {
    pub(super) fn build_convergence_profiles(
        policy_settings: &FxHashMap<String, PolicyConfig>,
    ) -> FxHashMap<String, ConvergencePolicyProfile> {
        let mut profiles = FxHashMap::default();
        for (name, settings) in policy_settings {
            let mut domain = name.clone();
            let mut priority = ConvergenceController::default_priority_for(name.as_str());
            let mut impact_radius = ConvergenceController::default_impact_radius_for(name.as_str());
            let mut priority_weight_bp =
                ConvergenceController::default_priority_weight_bp_for(name.as_str());
            let mut risk_tier = ConvergenceController::default_risk_tier_for(name.as_str());
            if let Some(table) = settings.table_value("convergence") {
                if let Some(value) = table.get("domain").and_then(|item| item.as_str()) {
                    let trimmed = value.trim();
                    if !trimmed.is_empty() {
                        domain = trimmed.to_string();
                    }
                }
                if let Some(value) = table.get("priority").and_then(|item| item.as_integer()) {
                    if value >= 0 {
                        priority = value.min(u16::MAX as i64) as u16;
                    }
                }
                if let Some(value) = table
                    .get("impact_radius")
                    .and_then(|item| item.as_integer())
                {
                    if value >= 0 {
                        impact_radius = value as usize;
                    }
                }
                if let Some(value) = table
                    .get("priority_weight_bp")
                    .and_then(|item| item.as_integer())
                {
                    if value >= 0 {
                        priority_weight_bp = value.min(1_000) as u16;
                    }
                } else if let Some(value) = table
                    .get("semantic_priority_weight")
                    .and_then(|item| item.as_float())
                    .or_else(|| {
                        table
                            .get("semantic_priority_weight")
                            .and_then(|item| item.as_integer())
                            .map(|item| item as f64)
                    })
                {
                    priority_weight_bp = (value.clamp(0.0, 1.0) * 1_000.0).round() as u16;
                }
                if let Some(value) = table.get("risk_tier").and_then(|item| item.as_str()) {
                    let normalized = value.trim().to_ascii_lowercase();
                    risk_tier = match normalized.as_str() {
                        "stabilizer" | "stabilize" | "low" => ConvergenceRiskTier::Stabilizer,
                        "rewrite" | "high" => ConvergenceRiskTier::Rewrite,
                        "balanced" | "medium" => ConvergenceRiskTier::Balanced,
                        _ => risk_tier,
                    };
                }
            }
            profiles.insert(
                name.clone(),
                ConvergencePolicyProfile::with_risk_tier(
                    domain,
                    priority,
                    impact_radius,
                    priority_weight_bp,
                    risk_tier,
                ),
            );
        }
        profiles
    }

    pub(super) fn build_convergence_signal(input: ConvergenceSignalInput<'_>) -> ConvergencePolicySignal {
        let ConvergenceSignalInput {
            result,
            semantic,
            summary,
            previous_contract_failures,
            capability,
            cluster_radius_cap,
            adaptive,
        } = input;
        let mut semantic_confidence_bp = adaptive.semantic_confidence_bp_base();
        let mut impact_radius = 0usize;
        if let Some(context) = semantic {
            if context.clang_success {
                semantic_confidence_bp = semantic_confidence_bp.saturating_add(200);
            }
            if !context.tree_has_error {
                semantic_confidence_bp = semantic_confidence_bp.saturating_add(150);
            }
            if context.diagnostic_summary.error_total() == 0 {
                semantic_confidence_bp = semantic_confidence_bp.saturating_add(150);
            } else {
                let penalty = context.diagnostic_summary.error_total().min(10) as u16 * 20;
                semantic_confidence_bp = semantic_confidence_bp.saturating_sub(penalty);
            }
            if summary.usr_backed_declaration_count > 0 {
                semantic_confidence_bp = semantic_confidence_bp.saturating_add(100);
            }
            let ref_radius = if summary.reference_count > 2000 { 3 }
                else if summary.reference_count > 500 { 2 }
                else { 1 };
            impact_radius = impact_radius.max(ref_radius);
        } else {
            semantic_confidence_bp = semantic_confidence_bp.saturating_sub(100);
        }
        if previous_contract_failures.contains(&SemanticInvariantClause::SymbolIdentity)
            || previous_contract_failures
                .contains(&SemanticInvariantClause::DeclarationReferenceIntegrity)
        {
            semantic_confidence_bp = semantic_confidence_bp.saturating_sub(80);
            impact_radius = impact_radius.max(2);
        }
        if previous_contract_failures.contains(&SemanticInvariantClause::ScopeIntegrity) {
            semantic_confidence_bp = semantic_confidence_bp.saturating_sub(60);
            impact_radius = impact_radius.max(2);
        }
        if previous_contract_failures.contains(&SemanticInvariantClause::EditSafety) {
            semantic_confidence_bp = semantic_confidence_bp.saturating_sub(40);
        }
        let edit_radius = if result.edits.len() > 40 { 3 }
            else if result.edits.len() > 10 { 2 }
            else { 1 };
        impact_radius = impact_radius.max(edit_radius);
        let mut resolved_radius = impact_radius.min(8);
        if let Some(cap) = cluster_radius_cap {
            resolved_radius = resolved_radius.min(cap.max(1));
        }
        let (impact_ranges, symbol_ids) =
            Self::build_semantic_impact_maps(result, semantic, resolved_radius);
        ConvergencePolicySignal {
            semantic_confidence_bp: semantic_confidence_bp.min(1_000),
            impact_radius: resolved_radius,
            capability_semantic_rewrite: capability.semantic_rewrite,
            capability_macro_sensitive: capability.macro_sensitive,
            capability_whitespace_safe: capability.whitespace_safe,
            solver_dropped_lines: 0,
            hard_blocked_lines: 0,
            impact_ranges,
            symbol_ids,
        }
    }

    #[allow(clippy::type_complexity)]
    pub(super) fn build_semantic_impact_maps(
        result: &PolicyResult,
        semantic: Option<&SemanticFileContext>,
        base_radius: usize,
    ) -> (
        BTreeMap<usize, SmallVec<[(usize, usize); 4]>>,
        BTreeMap<usize, SmallVec<[u64; 4]>>,
    ) {
        let impact_cap = 256usize;
        let scope_cap = 512usize;
        let symbol_cap = 512usize;
        let fallback_radius = base_radius.max(1);
        let mut edit_lines = result
            .edits
            .iter()
            .filter_map(|edit| (edit.line > 0 && edit.before != edit.after).then_some(edit.line))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .take(impact_cap)
            .collect::<Vec<_>>();
        edit_lines.sort_unstable();
        if edit_lines.is_empty() {
            return (BTreeMap::new(), BTreeMap::new());
        }

        let mut ranges_by_line = BTreeMap::<usize, SmallVec<[(usize, usize); 4]>>::new();
        let mut symbols_by_line = BTreeMap::<usize, SmallVec<[u64; 4]>>::new();
        let Some(semantic) = semantic else {
            for line in edit_lines {
                ranges_by_line.insert(
                    line,
                    smallvec![(
                        line.saturating_sub(fallback_radius).max(1),
                        line.saturating_add(fallback_radius).max(1),
                    )],
                );
            }
            return (ranges_by_line, symbols_by_line);
        };

        let mut symbol_lines: FxHashMap<u64, (usize, usize)> = FxHashMap::default();
        let mut symbol_ids_by_line: FxHashMap<usize, SmallVec<[u64; 4]>> = FxHashMap::default();
        for declaration in &semantic.declarations {
            if declaration.line == 0 {
                continue;
            }
            let stable = Self::hash_semantic_stable_id(declaration.stable_id.as_str());
            symbol_ids_by_line
                .entry(declaration.line)
                .or_default()
                .push(stable);
            symbol_lines
                .entry(stable)
                .and_modify(|bounds| {
                    bounds.0 = bounds.0.min(declaration.line);
                    bounds.1 = bounds.1.max(declaration.line);
                })
                .or_insert((declaration.line, declaration.line));
        }
        for reference in &semantic.references {
            if reference.line == 0 {
                continue;
            }
            let stable = Self::hash_semantic_stable_id(reference.stable_id.as_str());
            symbol_ids_by_line
                .entry(reference.line)
                .or_default()
                .push(stable);
            symbol_lines
                .entry(stable)
                .and_modify(|bounds| {
                    bounds.0 = bounds.0.min(reference.line);
                    bounds.1 = bounds.1.max(reference.line);
                })
                .or_insert((reference.line, reference.line));
        }

        let mut scopes_by_width: Vec<(usize, usize, usize)> = semantic
            .scopes
            .iter()
            .map(|s| {
                let start = s.start_line.max(1);
                let end = s.end_line.max(1);
                (end.saturating_sub(start), start, end)
            })
            .filter(|(_, start, end)| start <= end)
            .collect();
        scopes_by_width.sort_unstable();

        for line in edit_lines {
            let mut ranges = SmallVec::<[(usize, usize); 4]>::new();
            for &(width, start, end) in &scopes_by_width {
                if width.saturating_add(1) > scope_cap {
                    break;
                }
                if start > line || end < line {
                    continue;
                }
                ranges.push((start, end));
                if ranges.len() >= CONVERGENCE_MAX_IMPACT_RANGES_PER_LINE {
                    break;
                }
            }

            let mut symbol_ids = symbol_ids_by_line.remove(&line).unwrap_or_default();
            symbol_ids.sort_unstable();
            symbol_ids.dedup();
            for stable in &symbol_ids {
                if let Some((start, end)) = symbol_lines.get(stable).copied() {
                    let width = end.saturating_sub(start).saturating_add(1);
                    if width <= symbol_cap {
                        ranges.push((start.max(1), end.max(1)));
                    }
                }
            }

            if ranges.is_empty() {
                ranges.push((
                    line.saturating_sub(fallback_radius).max(1),
                    line.saturating_add(fallback_radius).max(1),
                ));
            }
            Self::normalize_impact_ranges(&mut ranges);
            if ranges.len() > CONVERGENCE_MAX_IMPACT_RANGES_PER_LINE {
                ranges.truncate(CONVERGENCE_MAX_IMPACT_RANGES_PER_LINE);
            }

            if !symbol_ids.is_empty() {
                symbols_by_line.insert(line, symbol_ids);
            }
            ranges_by_line.insert(line, ranges);
        }

        (ranges_by_line, symbols_by_line)
    }

    pub(super) fn normalize_impact_ranges(ranges: &mut SmallVec<[(usize, usize); 4]>) {
        if ranges.is_empty() {
            return;
        }
        ranges.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
        let mut merged = SmallVec::<[(usize, usize); 4]>::with_capacity(ranges.len());
        for &(start, end) in ranges.iter() {
            let start: usize = start.max(1);
            let end: usize = end.max(start);
            if let Some(last) = merged.last_mut() {
                if start <= last.1.saturating_add(1) {
                    last.1 = last.1.max(end);
                    continue;
                }
            }
            merged.push((start, end));
        }
        *ranges = merged;
    }

    pub(super) fn hash_semantic_stable_id(value: &str) -> u64 {
        let mut hasher = rustc_hash::FxHasher::default();
        value.hash(&mut hasher);
        hasher.finish()
    }

    pub(super) fn apply_semantic_mode(
        before_text: &str,
        result: PolicyResult,
        semantic_query: &SemanticContextQuery<'_>,
        capability: &PolicyCapabilities,
        config: SemanticGuidanceConfig<'_>,
    ) -> PolicyResult {
        if result.edits.is_empty() || !semantic_query.is_available() {
            return result;
        }
        let unsafe_lines = result
            .edits
            .iter()
            .filter_map(|edit| {
                if edit.line == 0 || edit.before == edit.after {
                    return None;
                }
                let line = edit.line;
                let safe_line = if semantic_query.is_safe_edit(line, 1) {
                    true
                } else if !capability.semantic_rewrite {
                    let profile = semantic_query.line_profile(line);
                    let consensus_diag_relax = !config.exact_compdb_for_file
                        && matches!(
                            config.semantic_context_kind,
                            SemanticCompdbContextKind::PairedSourceHeuristic
                                | SemanticCompdbContextKind::HeaderConsensus
                                | SemanticCompdbContextKind::SourceConsensus
                        )
                        && profile.has_diagnostic_error
                        && !profile.in_macro_region
                        && profile.declaration_count == 0
                        && profile.reference_count == 0
                        && capability.structural_safe;
                    (capability.allows_zone(PolicyZone::Preprocessor)
                        && semantic_query.is_safe_global(line, 1))
                        || (capability.allows_zone(PolicyZone::Comments)
                            && !semantic_query.has_diag_error(line))
                        || consensus_diag_relax
                } else {
                    false
                };
                (!safe_line).then_some(line)
            })
            .collect::<BTreeSet<_>>();
        if unsafe_lines.is_empty() {
            return result;
        }

        let lines_hint = Self::line_hint(unsafe_lines.iter().copied(), unsafe_lines.len(), 8);
        match config.guidance_mode {
            PolicyGuidanceMode::SoftGuideline => {
                let dropped_count = unsafe_lines.len();
                let mut suppressed = if capability.structural_safe {
                    Self::suppress_structural(before_text, result, &unsafe_lines)
                } else {
                    Self::apply_line_suppression(before_text, result, &unsafe_lines)
                };
                suppressed.warnings.push(format!(
                    "semantic guideline dropped {} unsafe edit line(s) for '{}' (mode={}, lines={})",
                    dropped_count,
                    config.policy_name,
                    config.guidance_mode.as_str(),
                    lines_hint
                ));
                suppressed
            }
            PolicyGuidanceMode::HardInvariant => {
                let line = unsafe_lines.iter().next().copied().unwrap_or(1);
                let mut violations = result.violations;
                violations.push(Violation {
                    policy: "semantic_contract".into(),
                    message: format!(
                        "semantic hard invariant blocked '{}' in unsafe region(s) (lines={})",
                        config.policy_name, lines_hint
                    ),
                    line,
                    column: Some(1),
                });
                PolicyResult {
                    text: before_text.to_string(),
                    violations,
                    edits: Vec::new(),
                    warnings: result.warnings,
                    changed: false,
                }
            }
        }
    }

    pub(super) fn semantic_error_lines(regions: &[SemanticRegion]) -> BTreeSet<usize> {
        let mut error_lines = BTreeSet::new();
        for region in regions {
            if region.has_diagnostic_error && region.kind == SemanticRegionKind::Diagnostic {
                for line in region.start_line..=region.end_line {
                    error_lines.insert(line);
                }
            }
        }
        error_lines
    }
}
