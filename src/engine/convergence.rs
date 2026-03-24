/// Convergence Controller — priority-based edit conflict resolution.
///
/// When two policies propose edits on the same line, the one with the higher
/// priority number wins. All edits on unclaimed lines are accepted immediately.
///
/// This replaces the previous graph-based convergence system (history tracking,
/// impact radius, semantic overlap, signature conflict detection). Those
/// mechanisms tried to retroactively resolve conflicts created by sequential
/// mutation. The correct answer is simpler: priority resolves conflict,
/// the understanding gate (Phase 1-2) prevents unsafe edits.
use std::collections::{BTreeMap, BTreeSet};
use rustc_hash::FxHashMap;
use std::sync::Arc;

use smallvec::SmallVec;

use crate::engine::catalog::policy_catalog;
use crate::model::edit::Edit;
use crate::model::policy_result::PolicyResult;
use crate::model::violation::Violation;
use crate::parser::text_scan;

// ── Public types (kept for pipeline compatibility) ─────────────────────────

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ConvergenceRiskTier {
    Stabilizer,
    Balanced,
    Rewrite,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConvergencePolicyProfile {
    pub domain: String,
    pub priority: u16,
    pub impact_radius: usize,
    pub priority_weight_bp: u16,
    pub risk_tier: ConvergenceRiskTier,
}

/// Signal carrying per-policy context into reconciliation.
/// Most fields are informational only in the simplified controller;
/// `impact_radius` is forwarded to the trace for diagnostics.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConvergencePolicySignal {
    pub semantic_confidence_bp: u16,
    pub impact_radius: usize,
    pub capability_semantic_rewrite: bool,
    pub capability_macro_sensitive: bool,
    pub capability_whitespace_safe: bool,
    pub solver_dropped_lines: usize,
    pub hard_blocked_lines: usize,
    pub impact_ranges: BTreeMap<usize, SmallVec<[(usize, usize); 4]>>,
    pub symbol_ids: BTreeMap<usize, SmallVec<[u64; 4]>>,
}

pub struct ConvergenceFinalizeResult {
    pub edits: Vec<Edit>,
    pub violations: Vec<Violation>,
    pub warnings: Vec<String>,
    pub convergence_pairs: BTreeMap<(String, String), usize>,
}

// ── Internal state ─────────────────────────────────────────────────────────

struct LineClaim {
    policy: String,
    priority: u16,
}

// ── Controller ─────────────────────────────────────────────────────────────

pub struct ConvergenceController {
    policy_profiles: Arc<FxHashMap<String, ConvergencePolicyProfile>>,
    /// Which policy has claimed each line and at what priority.
    claimed_lines: FxHashMap<usize, LineClaim>,
    /// Lines suppressed from a policy's final edit list (previous policy was
    /// outranked by a later one; its text changes are already in state.current
    /// but the edit is excluded from the final report).
    suppressed_lines_by_policy: FxHashMap<String, BTreeSet<usize>>,
}

impl ConvergencePolicyProfile {
    #[cfg(test)]
    pub fn new(
        domain: String,
        priority: u16,
        impact_radius: usize,
        priority_weight_bp: u16,
    ) -> Self {
        Self {
            domain,
            priority,
            impact_radius,
            priority_weight_bp,
            risk_tier: ConvergenceRiskTier::Balanced,
        }
    }

    pub fn with_risk_tier(
        domain: String,
        priority: u16,
        impact_radius: usize,
        priority_weight_bp: u16,
        risk_tier: ConvergenceRiskTier,
    ) -> Self {
        Self {
            domain,
            priority,
            impact_radius,
            priority_weight_bp,
            risk_tier,
        }
    }
}

impl ConvergenceController {
    pub fn new() -> Self {
        Self::with_profiles(Arc::new(FxHashMap::default()))
    }

    pub fn with_profiles(
        policy_profiles: Arc<FxHashMap<String, ConvergencePolicyProfile>>,
    ) -> Self {
        Self {
            policy_profiles,
            claimed_lines: FxHashMap::default(),
            suppressed_lines_by_policy: FxHashMap::default(),
        }
    }

    /// Resolve conflicts between this policy's proposed edits and previously
    /// claimed lines by other policies.
    ///
    /// **Rule**: on any line already claimed by another policy, the policy with
    /// the higher priority number wins. Equal priority favors the previous
    /// claimant (first-writer wins on ties).
    ///
    /// Edits on unclaimed lines are accepted immediately and the line is claimed.
    pub fn reconcile_policy_result(
        &mut self,
        policy_name: &str,
        before_text: &str,
        mut result: PolicyResult,
    ) -> PolicyResult {
        if result.edits.is_empty() {
            return result;
        }

        let current_priority = self.policy_profile(policy_name).priority;
        let mut dropped_lines = BTreeSet::new();

        for edit in &result.edits {
            if edit.line == 0 || edit.before == edit.after {
                continue;
            }
            if let Some(claim) = self.claimed_lines.get(&edit.line) {
                if claim.priority >= current_priority {
                    // Previous wins (equal or higher priority) — drop this edit.
                    dropped_lines.insert(edit.line);
                } else {
                    // Current wins — suppress the previous claimant's edit on
                    // this line from the final edit report.
                    self.suppressed_lines_by_policy
                        .entry(claim.policy.clone())
                        .or_default()
                        .insert(edit.line);
                }
            }
        }

        if !dropped_lines.is_empty() {
            result = Self::apply_line_suppression(before_text, result, &dropped_lines);
            tracing::debug!(
                policy = policy_name,
                dropped = dropped_lines.len(),
                "convergence_controller: dropped conflicting line edit(s) superseded by higher-priority policy"
            );
        }

        // Claim all winning lines for this policy.
        for edit in &result.edits {
            if edit.line == 0 || edit.before == edit.after {
                continue;
            }
            self.claimed_lines.insert(
                edit.line,
                LineClaim {
                    policy: policy_name.to_string(),
                    priority: current_priority,
                },
            );
        }

        result
    }

    /// Filter the accumulated edits/violations by suppressed lines and return
    /// the final result. Called once after all policies have run.
    pub fn finalize(
        self,
        edits: Vec<Edit>,
        violations: Vec<Violation>,
        warnings: Vec<String>,
    ) -> ConvergenceFinalizeResult {
        let filtered_edits = edits
            .into_iter()
            .filter(|edit| !self.is_suppressed(edit.policy.as_str(), edit.line))
            .collect::<Vec<_>>();
        let filtered_violations = violations
            .into_iter()
            .filter(|v| !self.is_suppressed(v.policy.as_str(), v.line))
            .collect::<Vec<_>>();
        ConvergenceFinalizeResult {
            edits: filtered_edits,
            violations: filtered_violations,
            warnings,
            convergence_pairs: BTreeMap::new(),
        }
    }

    // ── Catalog lookups (used by policy_pipeline build_convergence_profiles) ─

    pub fn default_priority_for(policy_name: &str) -> u16 {
        policy_catalog()
            .convergence(policy_name)
            .priority
    }

    pub fn default_risk_tier_for(policy_name: &str) -> ConvergenceRiskTier {
        use crate::engine::catalog::CatalogConvergenceRiskTier;
        match policy_catalog()
            .convergence(policy_name)
            .risk_tier
        {
            CatalogConvergenceRiskTier::Stabilizer => ConvergenceRiskTier::Stabilizer,
            CatalogConvergenceRiskTier::Balanced => ConvergenceRiskTier::Balanced,
            CatalogConvergenceRiskTier::Rewrite => ConvergenceRiskTier::Rewrite,
        }
    }

    pub fn default_impact_radius_for(policy_name: &str) -> usize {
        policy_catalog()
            .convergence(policy_name)
            .impact_radius
    }

    pub fn default_priority_weight_bp_for(policy_name: &str) -> u16 {
        policy_catalog()
            .convergence(policy_name)
            .priority_weight_bp
    }

    // ── Private helpers ────────────────────────────────────────────────────

    fn is_suppressed(&self, policy_name: &str, line: usize) -> bool {
        self.suppressed_lines_by_policy
            .get(policy_name)
            .is_some_and(|lines| lines.contains(&line))
    }

    fn policy_profile(&self, policy_name: &str) -> ConvergencePolicyProfile {
        self.policy_profiles
            .get(policy_name)
            .cloned()
            .unwrap_or_else(|| {
                let defaults = policy_catalog().convergence(policy_name);
                ConvergencePolicyProfile::with_risk_tier(
                    defaults.domain,
                    defaults.priority,
                    defaults.impact_radius,
                    defaults.priority_weight_bp,
                    match defaults.risk_tier {
                        crate::engine::catalog::CatalogConvergenceRiskTier::Stabilizer => {
                            ConvergenceRiskTier::Stabilizer
                        }
                        crate::engine::catalog::CatalogConvergenceRiskTier::Balanced => {
                            ConvergenceRiskTier::Balanced
                        }
                        crate::engine::catalog::CatalogConvergenceRiskTier::Rewrite => {
                            ConvergenceRiskTier::Rewrite
                        }
                    },
                )
            })
    }

    fn apply_line_suppression(
        before_text: &str,
        result: PolicyResult,
        disabled_lines: &BTreeSet<usize>,
    ) -> PolicyResult {
        if disabled_lines.is_empty() {
            return result;
        }
        let kept_violations = result
            .violations
            .into_iter()
            .filter(|item| !disabled_lines.contains(&item.line))
            .collect::<Vec<_>>();
        let kept_edits = result
            .edits
            .into_iter()
            .filter(|item| !disabled_lines.contains(&item.line))
            .collect::<Vec<_>>();

        if kept_edits.is_empty() {
            return PolicyResult {
                text: before_text.to_string(),
                violations: kept_violations,
                edits: kept_edits,
                warnings: result.warnings,
                changed: false,
            };
        }

        let before_lines = text_scan::TEXT_SCAN.split_lines_as_slices(before_text, true);
        let mut after_lines: Vec<std::borrow::Cow<'_, str>> = text_scan::TEXT_SCAN
            .split_lines_as_slices(result.text.as_str(), true)
            .into_iter()
            .map(std::borrow::Cow::Borrowed)
            .collect();
        let max_count = before_lines.len().min(after_lines.len());
        for line_no in disabled_lines {
            let index = line_no.saturating_sub(1);
            if index < max_count {
                after_lines[index] = std::borrow::Cow::Owned(before_lines[index].to_string());
            }
        }
        let mut merged = String::with_capacity(before_text.len());
        for line in &after_lines {
            merged.push_str(line);
        }
        PolicyResult {
            text: merged,
            violations: kept_violations,
            edits: kept_edits,
            warnings: result.warnings,
            changed: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rustc_hash::FxHashMap;

    use crate::model::edit::Edit;
    use crate::model::policy_result::PolicyResult;
    use crate::model::violation::Violation;

    use super::{ConvergenceController, ConvergencePolicyProfile};

    fn result_for(
        policy: &str,
        text: &str,
        before: &str,
        after: &str,
        line: usize,
    ) -> PolicyResult {
        PolicyResult {
            text: text.to_string(),
            violations: vec![Violation {
                policy: policy.into(),
                message: "test".to_string(),
                line,
                column: Some(1),
            }],
            edits: vec![Edit {
                policy: policy.into(),
                line,
                before: before.to_string(),
                after: after.to_string(),
            }],
            warnings: Vec::new(),
            changed: true,
        }
    }

    #[test]
    fn suppresses_prior_reversal() {
        // naming_conventions (priority 700) runs first, then clang_format (1000) reverses it.
        // clang_format has higher priority → wins. naming_conventions is suppressed.
        let mut controller = ConvergenceController::new();
        let before = "int x = 1;\n";
        let align_text = "int x  = 1;\n";
        let clang_text = "int x = 1;\n";

        let align = result_for(
            "naming_conventions",
            align_text,
            "int x = 1;",
            "int x  = 1;",
            1,
        );
        let align =
            controller.reconcile_policy_result("naming_conventions", before, align);
        let clang = result_for("clang_format", clang_text, "int x  = 1;", "int x = 1;", 1);
        let clang = controller.reconcile_policy_result(
            "clang_format",
            align.text.as_str(),
            clang,
        );

        let mut edits = Vec::new();
        edits.extend(align.edits);
        edits.extend(clang.edits);

        let mut violations = Vec::new();
        violations.extend(align.violations);
        violations.extend(clang.violations);

        let finalized = controller.finalize(edits, violations, Vec::new());
        assert_eq!(finalized.edits.len(), 1);
        assert_eq!(finalized.edits[0].policy, "clang_format");
        assert_eq!(finalized.violations.len(), 1);
        assert_eq!(finalized.violations[0].policy, "clang_format");
    }

    #[test]
    fn drops_lower_priority() {
        // clang_format (1000) runs first, naming_conventions (700) reverses it.
        // naming_conventions has lower priority → its edit is dropped immediately.
        let mut controller = ConvergenceController::new();
        let original = "int x  = 1;\n";
        let clang_text = "int x = 1;\n";
        let align_text = "int x  = 1;\n";

        let clang = result_for("clang_format", clang_text, "int x  = 1;", "int x = 1;", 1);
        let clang = controller.reconcile_policy_result("clang_format", original, clang);
        let align = result_for(
            "naming_conventions",
            align_text,
            "int x = 1;",
            "int x  = 1;",
            1,
        );
        let align = controller.reconcile_policy_result(
            "naming_conventions",
            clang.text.as_str(),
            align,
        );

        assert!(align.edits.is_empty());
        assert_eq!(align.text, clang.text);
    }

    #[test]
    fn equal_priority_wins() {
        // Both policies have the same priority. The first one to claim the line wins.
        let mut profiles = FxHashMap::default();
        profiles.insert(
            "policy_a".to_string(),
            ConvergencePolicyProfile::new("layout".to_string(), 700, 0, 240),
        );
        profiles.insert(
            "policy_b".to_string(),
            ConvergencePolicyProfile::new("layout".to_string(), 700, 0, 240),
        );
        let mut controller =
            ConvergenceController::with_profiles(Arc::new(profiles));

        let base = "int x=1;\n";
        let a = result_for("policy_a", "int x = 1;\n", "int x=1;", "int x = 1;", 1);
        let a = controller.reconcile_policy_result("policy_a", base, a);
        let b = result_for("policy_b", "int x=2;\n", "int x = 1;", "int x=2;", 1);
        let b = controller.reconcile_policy_result("policy_b", a.text.as_str(), b);

        // policy_b (equal priority) must lose — first writer wins.
        assert!(b.edits.is_empty());
        assert_eq!(b.text, a.text);
    }

    #[test]
    fn unclaimed_accepted_immediately() {
        let mut controller = ConvergenceController::new();
        let base = "int a=1;\nint b=2;\nint c=3;\n";
        // Policy A claims line 1; Policy B claims line 3. Line 2 is free.
        let a = result_for("naming_conventions", "int a = 1;\nint b=2;\nint c=3;\n", "int a=1;", "int a = 1;", 1);
        let a = controller.reconcile_policy_result("naming_conventions", base, a);
        let b = result_for("clang_format", "int a = 1;\nint b=2;\nint c = 3;\n", "int c=3;", "int c = 3;", 3);
        let b = controller.reconcile_policy_result("clang_format", a.text.as_str(), b);

        assert_eq!(b.edits.len(), 1);
        assert_eq!(b.edits[0].policy, "clang_format");
        assert_eq!(b.edits[0].line, 3);
    }

    #[test]
    fn finalize_filters_suppressed() {
        let mut controller = ConvergenceController::new();
        let before = "int x = 1;\n";
        let align_text = "int x  = 1;\n";
        let clang_text = "int x = 1;\n";

        let align = result_for("naming_conventions", align_text, "int x = 1;", "int x  = 1;", 1);
        let align = controller.reconcile_policy_result("naming_conventions", before, align);
        let clang = result_for("clang_format", clang_text, "int x  = 1;", "int x = 1;", 1);
        let clang = controller.reconcile_policy_result("clang_format", align.text.as_str(), clang);

        let mut edits = align.edits.clone();
        edits.extend(clang.edits.clone());
        let finalized = controller.finalize(edits, vec![], vec![]);

        // Only clang_format's edit survives; naming_conventions' is suppressed.
        assert_eq!(finalized.edits.len(), 1);
        assert_eq!(finalized.edits[0].policy, "clang_format");
    }
}
