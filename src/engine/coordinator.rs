use std::collections::{BTreeMap, BTreeSet};

use rustc_hash::FxHashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Result};

use crate::config::types::AccuracyGateConfig;
use crate::config::types::RetryConfig;
use crate::engine::accuracy_gate::{
    AccuracyGate, AccuracyGateFailure, AccuracyGateInput, AccuracyGateStatus,
};
use crate::engine::catalog::PolicyCapabilityMatrix;
use crate::engine::pipeline::PolicyPipeline;
use crate::engine::run_options::PolicyRunOptions;
use crate::engine::post_check::CheckBaseline;
use crate::engine::post_check::PostEditCheckResult;
use crate::engine::post_check::PostEditChecker;
use crate::engine::post_check::PostEditFailureKind;
use crate::engine::semantic_contract::SemanticContract;
use crate::engine::semantic_contract::SemanticContractSnapshot;
use crate::model::edit::Edit;
use crate::model::pass_result::{FormatPassMetrics, FormatPassResult};
use crate::model::policy_result::PolicyResult;
use crate::model::violation::Violation;
use crate::parser::manager::{ParserManager, SemanticCompdbContextKind};
use crate::runtime::cluster_telemetry::{ClusterOutcome, PolicyClusterTelemetry};
use crate::runtime::graph_runtime::ProjectGraphRuntime;
use crate::parser::text_scan;

const UNTRACKED_TEXT_DELTA_SYNTHESIS_CAP: usize = 256;

#[derive(Clone, Copy)]
struct AccuracyGateEvaluation {
    semantic_ready: bool,
    attempted_edits: usize,
    attempted_violations: usize,
    accepted_edits: usize,
    semantic_context_kind: SemanticCompdbContextKind,
}

pub struct FormatterEngine {
    pipeline: PolicyPipeline,
    post_edit_checker: PostEditChecker,
    retry: RetryConfig,
    accuracy_gate: AccuracyGateConfig,
    project_graph: Option<Arc<ProjectGraphRuntime>>,
    observation_only: AtomicBool,
    adaptive_state: Arc<arc_swap::ArcSwap<crate::engine::certainty_filter::CertaintyFilterState>>,
}

impl FormatterEngine {
    pub fn new(
        pipeline: PolicyPipeline,
        parser_manager: ParserManager,
        retry: RetryConfig,
        accuracy_gate: AccuracyGateConfig,
        project_graph: Option<Arc<ProjectGraphRuntime>>,
    ) -> Self {
        let adaptive_state = pipeline.adaptive_state().clone();
        let semantic_contract = SemanticContract::new();
        debug_assert!(
            !SemanticContract::invariant_specs().is_empty(),
            "semantic contract invariants must be declared"
        );
        let post_edit_checker = PostEditChecker::new(
            parser_manager,
            retry.post_edit_fail_on_parser_unavailable,
            retry.post_edit_tree_error_ratio_tolerance,
            semantic_contract,
        );
        Self {
            pipeline,
            post_edit_checker,
            retry,
            accuracy_gate,
            project_graph,
            observation_only: AtomicBool::new(false),
            adaptive_state,
        }
    }

    pub fn set_observation_only(&self, value: bool) {
        self.observation_only.store(value, Ordering::Relaxed);
    }

    pub fn set_context_tracker(&self, tracker: crate::engine::context_tracker::PolicyContextTracker) {
        self.pipeline.set_context_tracker(tracker);
    }

    pub fn save_context_tracker(&self, path: &Path) -> anyhow::Result<()> {
        self.pipeline.save_context_tracker(path)
    }


    pub fn apply(&self, text: &str, path: &Path) -> Result<FormatPassResult> {
        self.apply_inner(text, path)
    }

    fn apply_inner(&self, text: &str, path: &Path) -> Result<FormatPassResult> {
        let adaptive_snap = self.adaptive_state.load();
        let mut warnings = Vec::<String>::new();
        let baseline_required = self.retry.post_edit_check_enabled
            || self.accuracy_gate.semantic_required
            || self.accuracy_gate.enabled;
        let semantic_context_kind = self.post_edit_checker.semantic_context_kind_for_path(path);
        let post_edit_baseline: Option<CheckBaseline> =
            baseline_required.then(|| self.post_edit_checker.build_baseline(path, text));
        let semantic_ready = post_edit_baseline
            .as_ref()
            .map(CheckBaseline::semantic_ready)
            .unwrap_or(false);

        if self.accuracy_gate.semantic_required && !semantic_ready {
            let detail = post_edit_baseline
                .as_ref()
                .and_then(CheckBaseline::semantic_readiness_note)
                .unwrap_or("semantic baseline unavailable");
            let decision = AccuracyGate::evaluate(
                &self.accuracy_gate,
                AccuracyGateInput {
                    semantic_ready: false,
                    attempted_edits: 0,
                    attempted_violations: 0,
                    accepted_edits: 0,
                    semantic_context_kind,
                },
            );
            if decision.status == AccuracyGateStatus::FailedClosed {
                return Err(
                    AccuracyGateFailure::semantic_required_unmet(path, detail, decision).into(),
                );
            }
            warnings.push(format!(
                "accuracy_gate warning: semantic_required unmet for {} ({})",
                path.display(),
                detail
            ));
        }

        let project_graph_snapshot = self
            .project_graph
            .as_ref()
            .map(|runtime| runtime.snapshot());
        // Pass 1: full pipeline run
        let obs_only = self.observation_only.load(Ordering::Relaxed);
        let options_1 = PolicyRunOptions {
            project_graph_snapshot: project_graph_snapshot.clone(),
            observation_only: obs_only,
            ..Default::default()
        };
        let mut pass_result = self.pipeline.run_with_options(text, path, &options_1)?;
        Self::normalize_untracked_text_delta(text, &mut pass_result);

        // Quick accept: no edits were produced
        if crate::parser::text_scan::TEXT_SCAN.strings_equal(&pass_result.policy_result.text, text) {
            pass_result.policy_result.warnings = Self::dedup_warning_slices(&[
                warnings.as_slice(),
                pass_result.policy_result.warnings.as_slice(),
            ]);
            let attempted_edits = pass_result.policy_result.edits.len();
            let attempted_violations = pass_result.policy_result.violations.len();
            self.apply_gate(
                &mut pass_result,
                path,
                AccuracyGateEvaluation {
                    semantic_ready,
                    attempted_edits,
                    attempted_violations,
                    accepted_edits: attempted_edits,
                    semantic_context_kind,
                },
            )?;
            PolicyClusterTelemetry::record_outcome(
                pass_result.policy_traces.as_slice(),
                ClusterOutcome::Accepted,
            );
            self.record_success(0);
            self.observe_adaptive(post_edit_baseline.as_ref(), 1.0);
            return Ok(pass_result);
        }

        // If post-edit checking is disabled, accept the edits as-is
        if !self.retry.post_edit_check_enabled {
            pass_result.policy_result.warnings = Self::dedup_warning_slices(&[
                warnings.as_slice(),
                pass_result.policy_result.warnings.as_slice(),
            ]);
            let attempted_edits = pass_result.policy_result.edits.len();
            let attempted_violations = pass_result.policy_result.violations.len();
            self.apply_gate(
                &mut pass_result,
                path,
                AccuracyGateEvaluation {
                    semantic_ready,
                    attempted_edits,
                    attempted_violations,
                    accepted_edits: attempted_edits,
                    semantic_context_kind,
                },
            )?;
            PolicyClusterTelemetry::record_outcome(
                pass_result.policy_traces.as_slice(),
                ClusterOutcome::Accepted,
            );
            self.record_success(0);
            self.observe_adaptive(post_edit_baseline.as_ref(), 1.0);
            return Ok(pass_result);
        }

        // Validate Pass 1 result with PostEditChecker
        let edited_lines_1 = pass_result
            .policy_result
            .edits
            .iter()
            .map(|edit| edit.line)
            .collect::<BTreeSet<_>>();
        let all_structural_safe = !pass_result.policy_result.edits.is_empty()
            && pass_result.policy_result.edits.iter().all(|edit| {
                let cap = PolicyCapabilityMatrix::for_policy(edit.policy.as_str());
                cap.structural_safe && !cap.semantic_rewrite
            });
        let check_1 = if all_structural_safe {
            if let Some(baseline) = post_edit_baseline.as_ref() {
                self.post_edit_checker.validate_structural_only(
                    path,
                    pass_result.policy_result.text.as_str(),
                    baseline,
                )
            } else {
                self.post_edit_checker.validate_for_edits(
                    path,
                    text,
                    pass_result.policy_result.text.as_str(),
                    Some(&edited_lines_1),
                    &adaptive_snap,
                )
            }
        } else if let Some(baseline) = post_edit_baseline.as_ref() {
            self.post_edit_checker.validate_with_baseline_for_edits(
                path,
                pass_result.policy_result.text.as_str(),
                baseline,
                Some(&edited_lines_1),
                &adaptive_snap,
            )
        } else {
            self.post_edit_checker.validate_for_edits(
                path,
                text,
                pass_result.policy_result.text.as_str(),
                Some(&edited_lines_1),
                &adaptive_snap,
            )
        };

        let scope_ranges = post_edit_baseline
            .as_ref()
            .and_then(|b| b.before_semantic_snapshot())
            .map(|s| &s.scopes.ranges_by_kind);

        if let Some((_acceptance_score_1, _)) = self.accept_pass_result(
            &pass_result,
            &edited_lines_1,
            &check_1,
            &mut warnings,
            scope_ranges,
            semantic_context_kind,
        ) {
            pass_result.policy_result.warnings = Self::dedup_warning_slices(&[
                warnings.as_slice(),
                pass_result.policy_result.warnings.as_slice(),
            ]);
            let attempted_edits = pass_result.policy_result.edits.len();
            let attempted_violations = pass_result.policy_result.violations.len();
            self.apply_gate(
                &mut pass_result,
                path,
                AccuracyGateEvaluation {
                    semantic_ready,
                    attempted_edits,
                    attempted_violations,
                    accepted_edits: attempted_edits,
                    semantic_context_kind,
                },
            )?;
            let outcome = if check_1.accepted {
                ClusterOutcome::Accepted
            } else {
                ClusterOutcome::Regressed
            };
            PolicyClusterTelemetry::record_outcome(
                pass_result.policy_traces.as_slice(),
                outcome,
            );
            self.record_success(0);
            let obs_score = if check_1.accepted { 1.0 } else { 0.50 };
            self.observe_adaptive(post_edit_baseline.as_ref(), obs_score);
            return Ok(pass_result);
        }

        // Identify which policies caused culprit lines (direct match + declaration backtrack)
        tracing::debug!(
            path = %path.display(),
            failure_kinds = ?check_1.failure_kinds,
            culprit_lines = ?check_1.culprit_lines,
            messages = ?check_1.messages,
            accepted = check_1.accepted,
            "pass 1 rejected by accept_pass_result"
        );
        let culprit_policies = Self::causal_culprit_policies(
            &pass_result.policy_result.edits,
            &check_1,
            post_edit_baseline
                .as_ref()
                .and_then(|b| b.before_semantic_snapshot()),
        );
        if culprit_policies.is_empty() {
            self.observe_adaptive(post_edit_baseline.as_ref(), 0.25);
            return self.reverted_result(path, text, pass_result, warnings, 1);
        }

        // Pass 2: block culprit policies and skip policies that produced zero edits in Pass 1
        let zero_edit_policies: FxHashSet<String> = pass_result.policy_traces.iter()
            .filter(|t| t.candidate_line_count == 0 && !culprit_policies.contains(t.policy.as_str()))
            .map(|t| t.policy.as_str().to_string())
            .collect();
        let options_2 = PolicyRunOptions {
            blocked_policies: culprit_policies,
            skip_zero_edit_policies: if zero_edit_policies.is_empty() { None } else { Some(zero_edit_policies) },
            project_graph_snapshot,
            ..Default::default()
        };
        let mut pass_2 = self.pipeline.run_with_options(text, path, &options_2)?;
        Self::normalize_untracked_text_delta(text, &mut pass_2);

        let edited_lines_2 = pass_2
            .policy_result
            .edits
            .iter()
            .map(|edit| edit.line)
            .collect::<BTreeSet<_>>();
        let all_structural_safe_2 = !pass_2.policy_result.edits.is_empty()
            && pass_2.policy_result.edits.iter().all(|edit| {
                let cap = PolicyCapabilityMatrix::for_policy(edit.policy.as_str());
                cap.structural_safe && !cap.semantic_rewrite
            });
        let check_2 = if all_structural_safe_2 {
            if let Some(baseline) = post_edit_baseline.as_ref() {
                self.post_edit_checker.validate_structural_only(
                    path,
                    pass_2.policy_result.text.as_str(),
                    baseline,
                )
            } else {
                self.post_edit_checker.validate_for_edits(
                    path,
                    text,
                    pass_2.policy_result.text.as_str(),
                    Some(&edited_lines_2),
                    &adaptive_snap,
                )
            }
        } else if let Some(baseline) = post_edit_baseline.as_ref() {
            self.post_edit_checker.validate_with_baseline_for_edits(
                path,
                pass_2.policy_result.text.as_str(),
                baseline,
                Some(&edited_lines_2),
                &adaptive_snap,
            )
        } else {
            self.post_edit_checker.validate_for_edits(
                path,
                text,
                pass_2.policy_result.text.as_str(),
                Some(&edited_lines_2),
                &adaptive_snap,
            )
        };

        if let Some((_acceptance_score_2, _)) = self.accept_pass_result(
            &pass_2,
            &edited_lines_2,
            &check_2,
            &mut warnings,
            scope_ranges,
            semantic_context_kind,
        ) {
            pass_2.policy_result.warnings = Self::dedup_warning_slices(&[
                warnings.as_slice(),
                pass_2.policy_result.warnings.as_slice(),
            ]);
            let attempted_edits = pass_2.policy_result.edits.len();
            let attempted_violations = pass_2.policy_result.violations.len();
            self.apply_gate(
                &mut pass_2,
                path,
                AccuracyGateEvaluation {
                    semantic_ready,
                    attempted_edits,
                    attempted_violations,
                    accepted_edits: attempted_edits,
                    semantic_context_kind,
                },
            )?;
            PolicyClusterTelemetry::record_outcome(
                pass_2.policy_traces.as_slice(),
                ClusterOutcome::Accepted,
            );
            self.record_success(1);
            let obs_score = if check_2.accepted { 0.85 } else { 0.40 };
            self.observe_adaptive(post_edit_baseline.as_ref(), obs_score);
            return Ok(pass_2);
        }

        // Both passes failed → revert all edits
        self.observe_adaptive(post_edit_baseline.as_ref(), 0.25);
        self.reverted_result(path, text, pass_2, warnings, 2)
    }

    /// Returns the acceptance score for this pass result. Post-edit is informational only —
    /// per-policy checkpoint is the only gate. The score feeds Kalman learning for adaptive trust.
    fn accept_pass_result(
        &self,
        pass: &FormatPassResult,
        _edited_lines: &BTreeSet<usize>,
        check: &PostEditCheckResult,
        warnings: &mut Vec<String>,
        _scope_ranges: Option<&BTreeMap<String, BTreeSet<(usize, usize)>>>,
        _context_kind: SemanticCompdbContextKind,
    ) -> Option<(f64, Option<()>)> {
        let has_rewrite_edits = pass.policy_result.edits.iter().any(|edit| {
            PolicyCapabilityMatrix::for_policy(edit.policy.as_str()).semantic_rewrite
        });
        if has_rewrite_edits {
            warnings.extend(
                check.messages.iter()
                    .filter(|m| !m.contains("semantic identity signature changed"))
                    .cloned()
            );
        } else {
            warnings.extend(check.messages.iter().cloned());
        }
        if check.accepted || check.failure_kinds.is_empty() {
            return Some((1.0, None));
        }
        tracing::debug!(
            failure_kinds = ?check.failure_kinds,
            culprit_lines = check.culprit_lines.len(),
            messages = check.messages.len(),
            "post-edit accept_pass_result: always accept (informational)"
        );
        warnings.push(format!(
            "post-edit check informational: acceptance_score=1.000, failure_kinds={:?}",
            check.failure_kinds
        ));
        Some((1.0, None))
    }

    fn reverted_result(
        &self,
        path: &Path,
        original_text: &str,
        failed_pass: FormatPassResult,
        mut warnings: Vec<String>,
        attempt_count: usize,
    ) -> Result<FormatPassResult> {
        let revert_message = format!(
            "Post-edit parser check failed after retries; reverted file changes (attempts={})",
            attempt_count
        );
        let mut violations = failed_pass.policy_result.violations;
        if self.accuracy_gate.fail_closed {
            violations.push(Violation {
                policy: "post_edit_check".into(),
                message: revert_message.clone(),
                line: 1,
                column: Some(1),
            });
            PolicyClusterTelemetry::record_outcome(
                failed_pass.policy_traces.as_slice(),
                ClusterOutcome::Reverted,
            );
            return Err(anyhow!(
                "accuracy gate fail-closed: post-edit validation failed after {} attempt(s) for {}",
                attempt_count,
                path.display()
            ));
        }
        warnings.push(format!("post-edit check warning: {}", revert_message));
        let all_warnings = Self::dedup_warning_slices(&[
            warnings.as_slice(),
            failed_pass.policy_result.warnings.as_slice(),
        ]);
        PolicyClusterTelemetry::record_outcome(
            failed_pass.policy_traces.as_slice(),
            ClusterOutcome::Reverted,
        );
        self.record_revert();
        Ok(FormatPassResult {
            policy_result: PolicyResult {
                text: original_text.to_string(),
                violations,
                edits: Vec::new(),
                warnings: all_warnings,
                changed: false,
            },
            convergence_pairs: BTreeMap::new(),
            policy_traces: Vec::new(),
            accuracy_gate: None,
            metrics: FormatPassMetrics::default(),
            boot_parse_ms: 0.0,
            clang_parse: None,
        })
    }

    fn apply_gate(
        &self,
        pass_result: &mut FormatPassResult,
        path: &Path,
        evaluation: AccuracyGateEvaluation,
    ) -> Result<()> {
        let decision = AccuracyGate::evaluate(
            &self.accuracy_gate,
            AccuracyGateInput {
                semantic_ready: evaluation.semantic_ready,
                attempted_edits: evaluation.attempted_edits,
                attempted_violations: evaluation.attempted_violations,
                accepted_edits: evaluation.accepted_edits,
                semantic_context_kind: evaluation.semantic_context_kind,
            },
        );
        if decision.passed() {
            return Ok(());
        }
        let message = decision.summary();
        match decision.status {
            AccuracyGateStatus::Passed => Ok(()),
            AccuracyGateStatus::WarningOnly => {
                pass_result.accuracy_gate = Some(decision.clone());
                pass_result.policy_result.warnings.push(message);
                pass_result.policy_result.violations.push(Violation {
                    policy: "accuracy_gate".into(),
                    message: "accuracy gate warning (fail-open rollout)".to_string(),
                    line: 1,
                    column: Some(1),
                });
                Ok(())
            }
            AccuracyGateStatus::FailedClosed => {
                Err(AccuracyGateFailure::threshold_miss(path, decision).into())
            }
        }
    }

    fn record_success(&self, _attempt_index: usize) {
        // adaptive calibrator removed; outcomes are tracked via AdaptiveTelemetry only
    }

    fn record_revert(&self) {
        // adaptive calibrator removed
    }

    fn observe_adaptive(
        &self,
        baseline: Option<&CheckBaseline>,
        acceptance_score: f64,
    ) {
        let structural_obs = baseline
            .and_then(|b| b.before_tree_error_ratio())
            .map(|r| 1.0 - r)
            .unwrap_or(0.85);

        let semantic_obs = baseline
            .and_then(|b| b.before_clang_summary())
            .map(|s| 1.0 - ((s.error as f64) * 0.1 + (s.warning as f64) * 0.02).min(1.0))
            .unwrap_or(0.50);

        let (coverage_obs, richness_obs) = baseline
            .and_then(|b| b.before_semantic_snapshot())
            .map(|snap| {
                let cov = (snap.summary.reference_count as f64 / 50.0).min(1.0);
                let rich = ((snap.scopes.counts.namespace
                    + snap.scopes.counts.type_scope
                    + snap.scopes.counts.function
                    + snap.summary.declaration_count) as f64
                    / 30.0)
                    .min(1.0);
                (cov, rich)
            })
            .unwrap_or((0.50, 0.50));

        let measurement = [structural_obs, semantic_obs, coverage_obs, richness_obs, acceptance_score];
        let mut state: crate::engine::certainty_filter::CertaintyFilterState = (**self.adaptive_state.load()).clone();
        state.observe(measurement);
        self.adaptive_state.store(Arc::new(state));
    }

    #[cfg(test)]
    fn scope_aware_culprit_locality(
        check: &PostEditCheckResult,
        edited_lines: &BTreeSet<usize>,
        numeric_radius: usize,
        scope_ranges: &BTreeMap<String, BTreeSet<(usize, usize)>>,
    ) -> (usize, usize) {
        if check.culprit_lines.is_empty() {
            return (0, 0);
        }
        let edited_scopes: Vec<(&str, usize, usize)> = scope_ranges
            .iter()
            .flat_map(|(kind, ranges)| {
                ranges.iter().filter_map(move |&(start, end)| {
                    if edited_lines.iter().any(|&el| el >= start && el <= end) {
                        Some((kind.as_str(), start, end))
                    } else {
                        None
                    }
                })
            })
            .collect();
        let local = check
            .culprit_lines
            .iter()
            .copied()
            .filter(|&cl| {
                if Self::line_near_edited_lines(cl, edited_lines, numeric_radius) {
                    return true;
                }
                edited_scopes
                    .iter()
                    .any(|&(_, start, end)| cl >= start && cl <= end)
            })
            .count();
        (local, check.culprit_lines.len())
    }

    fn policies_touching_lines(edits: &[Edit], lines: &BTreeSet<usize>) -> FxHashSet<String> {
        if edits.is_empty() || lines.is_empty() {
            return FxHashSet::default();
        }
        let mut policies = FxHashSet::default();
        for edit in edits {
            if edit.line == 0 || edit.policy.is_empty() || !lines.contains(&edit.line) {
                continue;
            }
            policies.insert(edit.policy.to_string());
        }
        policies
    }

    fn causal_culprit_policies(
        edits: &[Edit],
        check: &PostEditCheckResult,
        before_snapshot: Option<&SemanticContractSnapshot>,
    ) -> FxHashSet<String> {
        let mut policies = Self::policies_touching_lines(edits, &check.culprit_lines);
        let needs_backtrack = check
            .failure_kinds
            .contains(&PostEditFailureKind::SemanticReferenceIntegrityRegressed)
            || check
                .failure_kinds
                .contains(&PostEditFailureKind::SemanticIdentityRegressed);
        if !needs_backtrack {
            return policies;
        }
        let Some(snapshot) = before_snapshot else {
            return policies;
        };
        let mut declaration_lines = BTreeSet::new();
        for culprit_line in &check.culprit_lines {
            if let Some(stable_ids) = snapshot.identity.decl_ids_by_line.get(culprit_line) {
                for sid in stable_ids {
                    if let Some(&decl_line) = snapshot.identity.id_decl_lines.get(sid) {
                        declaration_lines.insert(decl_line);
                    }
                }
            }
            for (ref_sid, &ref_first_line) in &snapshot.identity.ref_first_line {
                if ref_first_line == *culprit_line {
                    if let Some(&decl_line) = snapshot.identity.id_decl_lines.get(ref_sid) {
                        declaration_lines.insert(decl_line);
                    }
                }
            }
        }
        if !declaration_lines.is_empty() {
            let backtracked = Self::policies_touching_lines(edits, &declaration_lines);
            policies.extend(backtracked);
        }
        policies
    }

    fn normalize_untracked_text_delta(
        input_text: &str,
        pass_result: &mut FormatPassResult,
    ) -> bool {
        if pass_result.policy_result.edits.is_empty()
            && pass_result.policy_result.text != input_text
        {
            let fallback_policy = pass_result
                .policy_result
                .violations
                .first()
                .map(|item| item.policy.as_str())
                .unwrap_or("retry_guard");
            let synthesized = Self::synthesize_line_edits(
                input_text,
                pass_result.policy_result.text.as_str(),
                fallback_policy,
            );
            if !synthesized.is_empty() {
                if synthesized.len() > UNTRACKED_TEXT_DELTA_SYNTHESIS_CAP {
                    pass_result.policy_result.text = input_text.to_string();
                    pass_result.policy_result.warnings.push(format!(
                        "retry guard: discarded oversized untracked text delta ({} line(s) > cap {})",
                        synthesized.len(),
                        UNTRACKED_TEXT_DELTA_SYNTHESIS_CAP
                    ));
                    return true;
                }
                let count = synthesized.len();
                pass_result.policy_result.edits = synthesized;
                pass_result.policy_result.warnings.push(format!(
                    "retry guard: synthesized {count} edit(s) from untracked text delta"
                ));
            }
        }
        false
    }

    fn synthesize_line_edits(before: &str, after: &str, fallback_policy: &str) -> Vec<Edit> {
        let before_lines = text_scan::split_lines_as_slices(before, true);
        let after_lines = text_scan::split_lines_as_slices(after, true);
        let max_lines = before_lines.len().max(after_lines.len());
        let mut edits = Vec::<Edit>::new();
        for idx in 0..max_lines {
            let left = before_lines.get(idx).copied().unwrap_or("");
            let right = after_lines.get(idx).copied().unwrap_or("");
            if text_scan::TEXT_SCAN.slices_equal(left.as_bytes(), right.as_bytes()) {
                continue;
            }
            edits.push(Edit {
                policy: fallback_policy.into(),
                line: idx + 1,
                before: left.to_string(),
                after: right.to_string(),
            });
        }
        edits
    }

    fn dedup_warning_slices(sources: &[&[String]]) -> Vec<String> {
        let mut seen: FxHashSet<&str> = FxHashSet::default();
        let mut merged = Vec::new();
        for source in sources {
            for warning in *source {
                if seen.insert(warning.as_str()) {
                    merged.push(warning.clone());
                }
            }
        }
        merged
    }

    #[cfg(test)]
    fn line_near_edited_lines(line: usize, edited_lines: &BTreeSet<usize>, radius: usize) -> bool {
        if line == 0 || edited_lines.is_empty() {
            return false;
        }
        let start = line.saturating_sub(radius);
        let end = line.saturating_add(radius);
        edited_lines.range(start..=end).next().is_some()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::engine::post_check::{PostEditCheckResult, PostEditFailureKind};
    use crate::model::edit::Edit;
    use crate::model::pass_result::FormatPassResult;

    use super::FormatterEngine;

    #[test]
    fn normalize_synthesizes_edits() {
        let mut pass_result = FormatPassResult::default();
        pass_result.policy_result.text = "changed".to_string();
        let normalized =
            FormatterEngine::normalize_untracked_text_delta("baseline", &mut pass_result);
        assert!(!normalized);
        assert_eq!(pass_result.policy_result.text, "changed");
        assert_eq!(pass_result.policy_result.edits.len(), 1);
        assert_eq!(pass_result.policy_result.edits[0].policy, "retry_guard");
        assert!(pass_result
            .policy_result
            .warnings
            .iter()
            .any(|warning| warning.contains("synthesized")));
    }

    #[test]
    fn normalize_synthesizes_eof() {
        let mut pass_result = FormatPassResult::default();
        pass_result.policy_result.text = "baseline\n".to_string();
        let normalized =
            FormatterEngine::normalize_untracked_text_delta("baseline", &mut pass_result);
        assert!(!normalized);
        assert_eq!(pass_result.policy_result.text, "baseline\n");
        assert_eq!(pass_result.policy_result.edits.len(), 1);
        assert!(pass_result
            .policy_result
            .warnings
            .iter()
            .any(|warning| warning.contains("synthesized")));
    }

    #[test]
    fn normalize_discards_oversized() {
        let mut pass_result = FormatPassResult::default();
        let mut changed = String::new();
        for index in 0..=super::UNTRACKED_TEXT_DELTA_SYNTHESIS_CAP {
            changed.push_str(format!("line_{index}\n").as_str());
        }
        pass_result.policy_result.text = changed;
        let normalized =
            FormatterEngine::normalize_untracked_text_delta("baseline\n", &mut pass_result);
        assert!(normalized);
        assert_eq!(pass_result.policy_result.text, "baseline\n");
        assert!(pass_result.policy_result.edits.is_empty());
        assert!(pass_result
            .policy_result
            .warnings
            .iter()
            .any(|warning| warning.contains("discarded oversized untracked text delta")));
    }

    #[test]
    fn normalize_keeps_tracked() {
        let mut pass_result = FormatPassResult::default();
        pass_result.policy_result.text = "changed".to_string();
        pass_result.policy_result.edits.push(Edit {
            policy: "compact_declarations".into(),
            line: 10,
            before: "a".to_string(),
            after: "b".to_string(),
        });
        let normalized =
            FormatterEngine::normalize_untracked_text_delta("baseline", &mut pass_result);
        assert!(!normalized);
        assert_eq!(pass_result.policy_result.text, "changed");
    }

    #[test]
    fn culprit_traces_declaration() {
        use crate::engine::semantic_contract::SemanticContractSnapshot;

        let edits = vec![
            Edit {
                policy: "naming_conventions".into(),
                line: 10,
                before: "int myVar".to_string(),
                after: "int my_var".to_string(),
            },
        ];
        let check = PostEditCheckResult {
            accepted: false,

            messages: vec![],
            failure_kinds: BTreeSet::from([
                PostEditFailureKind::SemanticReferenceIntegrityRegressed,
            ]),
            culprit_lines: BTreeSet::from([20, 25, 30]),
        };
        let mut snapshot = SemanticContractSnapshot::default();
        snapshot
            .identity.decl_ids_by_line
            .insert(20, BTreeSet::from(["sid_myVar".to_string()]));
        snapshot
            .identity.id_decl_lines
            .insert("sid_myVar".to_string(), 10);
        let result =
            FormatterEngine::causal_culprit_policies(&edits, &check, Some(&snapshot));
        assert!(result.contains("naming_conventions"));
    }

    #[test]
    fn culprit_direct_fallback() {
        let edits = vec![
            Edit {
                policy: "compact_declarations".into(),
                line: 20,
                before: "a=1".to_string(),
                after: "a = 1".to_string(),
            },
        ];
        let check = PostEditCheckResult {
            accepted: false,

            messages: vec![],
            failure_kinds: BTreeSet::from([
                PostEditFailureKind::SemanticReferenceIntegrityRegressed,
            ]),
            culprit_lines: BTreeSet::from([20]),
        };
        let result = FormatterEngine::causal_culprit_policies(&edits, &check, None);
        assert!(result.contains("compact_declarations"));
    }

    #[test]
    fn culprit_union_both() {
        use crate::engine::semantic_contract::SemanticContractSnapshot;

        let edits = vec![
            Edit {
                policy: "naming_conventions".into(),
                line: 10,
                before: "int myVar".to_string(),
                after: "int my_var".to_string(),
            },
            Edit {
                policy: "compact_declarations".into(),
                line: 25,
                before: "a=1".to_string(),
                after: "a = 1".to_string(),
            },
        ];
        let check = PostEditCheckResult {
            accepted: false,

            messages: vec![],
            failure_kinds: BTreeSet::from([
                PostEditFailureKind::SemanticReferenceIntegrityRegressed,
                PostEditFailureKind::SemanticIdentityRegressed,
            ]),
            culprit_lines: BTreeSet::from([20, 25]),
        };
        let mut snapshot = SemanticContractSnapshot::default();
        snapshot
            .identity.decl_ids_by_line
            .insert(20, BTreeSet::from(["sid_myVar".to_string()]));
        snapshot
            .identity.id_decl_lines
            .insert("sid_myVar".to_string(), 10);
        let result =
            FormatterEngine::causal_culprit_policies(&edits, &check, Some(&snapshot));
        assert!(result.contains("naming_conventions"));
        assert!(result.contains("compact_declarations"));
    }

    #[test]
    fn scope_accepts_same() {
        use std::collections::BTreeMap;

        let check = PostEditCheckResult {
            accepted: false,

            messages: vec![],
            failure_kinds: BTreeSet::from([
                PostEditFailureKind::SemanticReferenceIntegrityRegressed,
            ]),
            culprit_lines: BTreeSet::from([150]),
        };
        let edited_lines = BTreeSet::from([100]);
        let mut scope_ranges: BTreeMap<String, BTreeSet<(usize, usize)>> = BTreeMap::new();
        scope_ranges.insert(
            "function".to_string(),
            BTreeSet::from([(80, 200)]),
        );
        let (local, total) = FormatterEngine::scope_aware_culprit_locality(
            &check,
            &edited_lines,
            2,
            &scope_ranges,
        );
        assert_eq!(total, 1);
        assert_eq!(local, 1);
    }

    #[test]
    fn scope_rejects_different() {
        use std::collections::BTreeMap;

        let check = PostEditCheckResult {
            accepted: false,

            messages: vec![],
            failure_kinds: BTreeSet::from([
                PostEditFailureKind::SemanticReferenceIntegrityRegressed,
            ]),
            culprit_lines: BTreeSet::from([50]),
        };
        let edited_lines = BTreeSet::from([100]);
        let mut scope_ranges: BTreeMap<String, BTreeSet<(usize, usize)>> = BTreeMap::new();
        scope_ranges.insert(
            "function".to_string(),
            BTreeSet::from([(10, 60), (80, 120)]),
        );
        let (local, total) = FormatterEngine::scope_aware_culprit_locality(
            &check,
            &edited_lines,
            2,
            &scope_ranges,
        );
        assert_eq!(total, 1);
        assert_eq!(local, 0);
    }

    #[test]
    fn scope_numeric_fallback() {
        use std::collections::BTreeMap;

        let check = PostEditCheckResult {
            accepted: false,

            messages: vec![],
            failure_kinds: BTreeSet::from([
                PostEditFailureKind::SemanticReferenceIntegrityRegressed,
            ]),
            culprit_lines: BTreeSet::from([102]),
        };
        let edited_lines = BTreeSet::from([100]);
        let scope_ranges: BTreeMap<String, BTreeSet<(usize, usize)>> = BTreeMap::new();
        let (local, total) = FormatterEngine::scope_aware_culprit_locality(
            &check,
            &edited_lines,
            5,
            &scope_ranges,
        );
        assert_eq!(total, 1);
        assert_eq!(local, 1);
    }

}
