pub(super) mod checkpoint;
pub(super) mod confidence;
pub(super) mod context;
pub(super) mod edit_utils;
pub(super) mod semantic_impact;
pub(super) mod suppression;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use arc_swap::ArcSwap;
use tracing::{debug, warn};

use rustc_hash::FxHashMap;

use crate::config::types::ConfidenceConfig;
use crate::config::enums::Enforcement;
use crate::config::policy_config::PolicyConfig;
use crate::engine::gate_decision::{ConfidenceGateDecision, ConfidenceReasonCode};
use crate::engine::convergence::ConvergenceController;
use crate::engine::convergence::ConvergencePolicyProfile;
use crate::engine::convergence::ConvergencePolicySignal;
use crate::engine::conflict_solver::GlobalConflictSolver;
use crate::engine::catalog::PolicyCapabilities;
use crate::engine::catalog::policy_catalog;
use crate::engine::conflict_detector::PolicyConflictDetector;
use crate::engine::edit_candidate::PolicyDecisionOutcome;
use crate::engine::edit_candidate::PolicyEditCandidate;
use crate::engine::run_options::PolicyRunOptions;
use crate::engine::suppression::PolicySuppression;
use crate::engine::proposer::ProposerController;
use crate::engine::semantic_contract::PolicyGuidanceMode;
use crate::engine::semantic_contract::SemanticContract;
use crate::engine::semantic_contract::SemanticInvariantClause;
use crate::model::edit::Edit;
use crate::model::pass_result::{FormatPassMetrics, FormatPassResult};
use crate::model::policy_context::PolicyContext;
use crate::model::exec_trace::{
    PolicyCandidateOutcome, PolicyCandidateTrace, PolicyExecutionTrace,
};
use crate::model::policy_result::PolicyResult;
use crate::model::violation::Violation;
use crate::parser::clang_result::ClangParseResult;
use crate::parser::manager::ParserManager;
use crate::parser::query_cache::TsQueryCache;
use crate::parser::manager::SemanticCompdbContextKind;
use crate::parser::file_context::SemanticFileContext;
use crate::parser::file_context::SemanticSummary;
use crate::policy::Policy;
use crate::runtime::adaptive_telemetry::AdaptiveTelemetry;
use crate::runtime::cluster_telemetry::PolicyClusterTelemetry;
use crate::runtime::telemetry::{
    PolicyConfidenceSample, PolicyExecutionSample, PolicyTelemetry,
};
use crate::parser::text_scan;
use tree_sitter::Tree;
use crate::engine::context_tracker::{
    PolicyContextTracker,
};
use crate::parser::ts_traversal;
use crate::policy::shared_data::PolicySharedData;

const CONVERGENCE_MAX_IMPACT_RANGES_PER_LINE: usize = 6;

pub struct PolicyPipeline {
    policies: Vec<Box<dyn Policy>>,
    parser_manager: ParserManager,
    policy_settings: FxHashMap<String, PolicyConfig>,
    convergence_profiles: Arc<FxHashMap<String, ConvergencePolicyProfile>>,
    confidence_enabled: bool,
    confidence_default_enforcement: Enforcement,
    conflict_detection_enabled: bool,
    conflict_touch_threshold: usize,
    context_tracker: ArcSwap<PolicyContextTracker>,
    query_cache: TsQueryCache,
    adaptive_state: Arc<ArcSwap<crate::engine::certainty_filter::CertaintyFilterState>>,
}


struct ParseCache {
    tree: Option<Tree>,
    prev_tree: Option<Tree>,
    changed_ranges: Option<Vec<tree_sitter::Range>>,
    clang: Option<Arc<ClangParseResult>>,
    semantic: Option<SemanticFileContext>,
    comment_lines: Option<BTreeSet<usize>>,
    summary: Option<SemanticSummary>,
    error_lines: Option<BTreeSet<usize>>,
    clang_edits_since: usize,
    exact_compdb: bool,
    semantic_kind: SemanticCompdbContextKind,
    has_semantic_compdb: bool,
    fidelity_score: f64,
}

impl ParseCache {
    fn summary_or_default(&self) -> SemanticSummary {
        self.summary.unwrap_or_default()
    }

    fn error_lines_cached(&mut self) -> &BTreeSet<usize> {
        if self.error_lines.is_none() {
            self.error_lines = Some(
                self.tree
                    .as_ref()
                    .map(|t| ts_traversal::tree_error_stats(t).error_lines)
                    .unwrap_or_default(),
            );
        }
        self.error_lines.as_ref().unwrap()
    }

    fn compute_changed_ranges(&mut self) {
        self.changed_ranges = match (&self.prev_tree, &self.tree) {
            (Some(prev), Some(current)) => {
                let ranges: Vec<_> = prev.changed_ranges(current).collect();
                if ranges.is_empty() { None } else { Some(ranges) }
            }
            _ => None,
        };
    }

    fn invalidate(&mut self, is_semantic_rewrite: bool, clang_invalidating: bool) {
        self.prev_tree = self.tree.take();
        self.changed_ranges = None;
        self.error_lines = None;
        if is_semantic_rewrite && clang_invalidating {
            self.clang = None;
            self.clang_edits_since = 0;
        } else {
            self.clang_edits_since += 1;
        }
        self.semantic = None;
        self.comment_lines = None;
        self.summary = None;
    }
}

struct CandidateState {
    internal: Vec<PolicyEditCandidate>,
    selected: Vec<PolicyEditCandidate>,
    conflict: PolicyConflictDetector,
    proposer: ProposerController,
    rename_signal: Option<f64>,
}

struct TelemetryState {
    samples: Vec<PolicyExecutionSample>,
    context_mods: [f32; 24],
    block_mods: [[f32; 24]; 6],
}

pub(super) struct PipelineState<'a> {
    path: &'a Path,
    options: &'a PolicyRunOptions,
    current: Arc<str>,
    parse: ParseCache,
    cand: CandidateState,
    telem: TelemetryState,
    all_violations: Vec<Violation>,
    all_edits: Vec<Edit>,
    all_warnings: Vec<String>,
    policy_traces: Vec<PolicyExecutionTrace>,
    convergence_controller: ConvergenceController,
    retry_batch_size: Option<usize>,
}

#[derive(Clone, Copy)]
struct PreparedPolicyStage {
    capability: PolicyCapabilities,
    guidance_mode: PolicyGuidanceMode,
}

#[derive(Clone, Copy)]
struct SemanticGuidanceConfig<'a> {
    policy_name: &'a str,
    guidance_mode: PolicyGuidanceMode,
    exact_compdb_for_file: bool,
    semantic_context_kind: SemanticCompdbContextKind,
}

#[derive(Clone, Copy)]
struct ScopeFilterConfig<'a> {
    policy_name: &'a str,
    scope_stage: &'a str,
    capability: &'a PolicyCapabilities,
}

struct ConvergenceSignalInput<'a> {
    result: &'a PolicyResult,
    semantic: Option<&'a SemanticFileContext>,
    summary: crate::parser::file_context::SemanticSummary,
    previous_contract_failures: &'a BTreeSet<SemanticInvariantClause>,
    capability: &'a PolicyCapabilities,
    cluster_radius_cap: Option<usize>,
    adaptive: &'a crate::engine::certainty_filter::CertaintyFilterState,
}

struct CommitPolicyInput<'a> {
    policy_name: &'a str,
    policy_started: Instant,
    coordinated: CoordinatedPolicyStage,
    is_semantic_rewrite: bool,
    clang_invalidating: bool,
    parse_ms: f64,
    execute_ms: f64,
    checkpoint_ms: f64,
}

pub(super) struct PartialRollbackInput<'a> {
    coordinated: &'a CoordinatedPolicyStage,
    policy_name: &'a str,
    before_errors: usize,
    new_error_lines: &'a BTreeSet<usize>,
}

struct ExecutedPolicyStage {
    result: PolicyResult,
    capability: PolicyCapabilities,
    context_cluster: u64,
    confidence_sample: Option<PolicyConfidenceSample>,
    confidence_outcome: Option<PolicyDecisionOutcome>,
    confidence_score: Option<f64>,
    confidence_threshold: Option<f64>,
    dropped_line_count: usize,
    convergence_signal: ConvergencePolicySignal,
}

pub(super) enum PolicyCheckpointResult {
    Accept { validated_tree: Option<Tree> },
    PartialRollback {
        recovered_text: String,
        recovered_edits: Vec<Edit>,
        validated_tree: Option<Tree>,
        warning: String,
    },
    Rollback { reason: String, after_error_count: usize },
}

pub(super) struct CoordinatedPolicyStage {
    result: PolicyResult,
    accepted_candidates: Vec<PolicyEditCandidate>,
    candidate_trace: Vec<PolicyCandidateTrace>,
    conflict_violations: Vec<Violation>,
    context_cluster: u64,
    confidence_sample: Option<PolicyConfidenceSample>,
    confidence_outcome: Option<PolicyDecisionOutcome>,
    confidence_score: Option<f64>,
    confidence_threshold: Option<f64>,
    dropped_line_count: usize,
    convergence_signal: ConvergencePolicySignal,
    text_changed: bool,
}

impl PolicyPipeline {
    pub fn new(
        policies: Vec<Box<dyn Policy>>,
        parser_manager: ParserManager,
        policy_settings: FxHashMap<String, PolicyConfig>,
        confidence_config: ConfidenceConfig,
        conflict_detection_enabled: bool,
        conflict_touch_threshold: usize,
        adaptive_state: Arc<ArcSwap<crate::engine::certainty_filter::CertaintyFilterState>>,
    ) -> Self {
        let convergence_profiles = Arc::new(Self::build_convergence_profiles(&policy_settings));
        let query_cache = TsQueryCache::new(tree_sitter_cpp::LANGUAGE.into());
        Self {
            policies,
            parser_manager,
            policy_settings,
            convergence_profiles,
            confidence_enabled: confidence_config.enabled,
            confidence_default_enforcement: confidence_config.default_enforcement,
            conflict_detection_enabled,
            conflict_touch_threshold,
            context_tracker: ArcSwap::new(Arc::new(PolicyContextTracker::new())),
            query_cache,
            adaptive_state,
        }
    }

    pub fn adaptive_state(&self) -> &Arc<ArcSwap<crate::engine::certainty_filter::CertaintyFilterState>> {
        &self.adaptive_state
    }

    pub fn set_context_tracker(&self, tracker: PolicyContextTracker) {
        self.context_tracker.store(Arc::new(tracker));
    }



    pub fn save_context_tracker(&self, path: &Path) -> anyhow::Result<()> {
        let tracker = self.context_tracker.load();
        (**tracker).save_to_path(path)
    }

    pub fn run_with_options(
        &self,
        text: &str,
        path: &Path,
        options: &PolicyRunOptions,
    ) -> Result<FormatPassResult> {
        let mut state = self.initialize_run_state(text, path, options);
        let boot_parse_started = Instant::now();
        if let Some(first_policy) = self.policies.first() {
            let boot = Instant::now();
            self.ensure_policy_parse_stage(&mut state, first_policy.as_ref(), first_policy.name(), boot)?;
        }
        let boot_parse_ms = boot_parse_started.elapsed().as_secs_f64() * 1000.0;
        if options.observation_only {
            PolicyTelemetry::record_batch(&state.telem.samples);
            return Ok(FormatPassResult {
                boot_parse_ms,
                ..Default::default()
            });
        }
        self.record_initial_fidelity_warnings(&mut state);

        for policy in &self.policies {
            self.run_policy_stage(&mut state, policy.as_ref())?;
        }

        PolicyTelemetry::record_batch(&state.telem.samples);
        let finalized = state.convergence_controller.finalize(
            state.all_edits,
            state.all_violations,
            state.all_warnings,
        );
        let mut final_warnings = finalized.warnings;
        let final_text = Self::stabilize_output_text(
            text,
            state.current,
            finalized.edits.as_slice(),
            &mut final_warnings,
        );

        Ok(FormatPassResult {
            policy_result: PolicyResult {
                text: final_text,
                violations: finalized.violations,
                edits: finalized.edits,
                warnings: final_warnings,
                changed: true,
            },
            convergence_pairs: finalized.convergence_pairs,
            policy_traces: state.policy_traces,
            accuracy_gate: None,
            metrics: FormatPassMetrics::default(),
            boot_parse_ms,
            clang_parse: state.parse.clang.clone(),
        })
    }

    fn initialize_run_state<'a>(
        &self,
        text: &str,
        path: &'a Path,
        options: &'a PolicyRunOptions,
    ) -> PipelineState<'a> {
        let convergence_controller =
            if self.convergence_profiles.is_empty() {
                ConvergenceController::new()
            } else {
                ConvergenceController::with_profiles(
                    self.convergence_profiles.clone(),
                )
            };
        let exact_compdb_for_file = self.parser_manager.has_exact_compdb(path);
        let semantic_context_kind = self
            .parser_manager
            .semantic_compdb_kind(path);
        let semantic_compdb_context_for_file = self
            .parser_manager
            .has_semantic_compdb(path);
        PipelineState {
            path,
            options,
            current: Arc::from(text),
            parse: ParseCache {
                tree: None,
                prev_tree: None,
                changed_ranges: None,
                clang: None,
                semantic: None,
                comment_lines: None,
                summary: None,
                error_lines: None,
                clang_edits_since: 0,
                exact_compdb: exact_compdb_for_file,
                semantic_kind: semantic_context_kind,
                has_semantic_compdb: semantic_compdb_context_for_file,
                fidelity_score: 1.0,
            },
            cand: CandidateState {
                internal: Vec::new(),
                selected: Vec::new(),
                conflict: PolicyConflictDetector::new(
                    self.conflict_detection_enabled,
                    self.conflict_touch_threshold,
                ),
                proposer: ProposerController::new(),
                rename_signal: None,
            },
            telem: TelemetryState {
                samples: Vec::with_capacity(self.policies.len()),
                context_mods: self.batch_context_modifiers(path),
                block_mods: self.batch_block_mods(),
            },
            all_violations: Vec::new(),
            all_edits: Vec::new(),
            all_warnings: Vec::new(),
            policy_traces: Vec::with_capacity(self.policies.len()),
            convergence_controller,
            retry_batch_size: None,
        }
    }

    fn run_policy_stage(
        &self,
        state: &mut PipelineState<'_>,
        policy: &dyn Policy,
    ) -> Result<()> {
        let policy_name = policy.name();
        let policy_started = Instant::now();
        if state.options.is_policy_blocked(policy_name) {
            state
                .all_warnings
                .push(format!("retry guard skipped policy '{}'", policy_name));
            state.telem.samples.push(PolicyExecutionSample::blocked(
                policy_name,
                policy_started.elapsed(),
            ));
            state.policy_traces.push(PolicyExecutionTrace {
                policy: policy_name.into(),
                parse_mode: "blocked".to_string(),
                elapsed_ms: policy_started.elapsed().as_secs_f64() * 1000.0,
                ..Default::default()
            });
            return Ok(());
        }
        if state.options.is_policy_skipped(policy_name) {
            state.telem.samples.push(PolicyExecutionSample::blocked(
                policy_name,
                policy_started.elapsed(),
            ));
            state.policy_traces.push(PolicyExecutionTrace {
                policy: policy_name.into(),
                parse_mode: "skipped".to_string(),
                elapsed_ms: policy_started.elapsed().as_secs_f64() * 1000.0,
                ..Default::default()
            });
            return Ok(());
        }

        self.ensure_policy_parse_stage(state, policy, policy_name, policy_started)?;
        let parse_end = Instant::now();
        let Some(prepared) = self.prepare_policy_stage(state, policy_name, policy_started) else {
            return Ok(());
        };
        let is_semantic_rewrite = prepared.capability.semantic_rewrite;
        let clang_invalidating = prepared.capability.clang_invalidating;
        let is_whitespace_safe = prepared.capability.whitespace_safe;
        let executed = self.execute_policy_stage(state, policy, prepared, policy_started)?;
        let execute_end = Instant::now();
        if executed.result.edits.is_empty()
            && executed.result.violations.is_empty()
            && (!executed.result.changed
                || text_scan::TEXT_SCAN.strings_equal(&executed.result.text, &state.current))
        {
            state.telem.samples.push(PolicyExecutionSample::success(
                policy_name,
                policy_started.elapsed(),
                0,
                0,
            ));
            state.policy_traces.push(PolicyExecutionTrace {
                policy: policy_name.into(),
                parse_mode: "n/a".to_string(),
                elapsed_ms: policy_started.elapsed().as_secs_f64() * 1000.0,
                parse_ms: parse_end.duration_since(policy_started).as_secs_f64() * 1000.0,
                execute_ms: execute_end.duration_since(parse_end).as_secs_f64() * 1000.0,
                ..Default::default()
            });
            return Ok(());
        }
        let coordinated = self.coordinate_policy_stage(state, policy_name, executed);
        let parse_ms = parse_end.duration_since(policy_started).as_secs_f64() * 1000.0;
        let execute_ms = execute_end.duration_since(parse_end).as_secs_f64() * 1000.0;
        if is_whitespace_safe && coordinated.text_changed {
            self.commit_policy_stage(state, CommitPolicyInput {
                policy_name, policy_started, coordinated, is_semantic_rewrite, clang_invalidating,
                parse_ms, execute_ms, checkpoint_ms: 0.0,
            });
            return Ok(());
        }
        let checkpoint_started = Instant::now();
        match self.checkpoint_policy_stage(state, &coordinated, policy_name) {
            PolicyCheckpointResult::Accept { validated_tree } => {
                let checkpoint_ms = checkpoint_started.elapsed().as_secs_f64() * 1000.0;
                self.commit_policy_stage(state, CommitPolicyInput {
                    policy_name, policy_started, coordinated, is_semantic_rewrite, clang_invalidating,
                    parse_ms, execute_ms, checkpoint_ms,
                });
                if let Some(tree) = validated_tree {
                    state.parse.tree = Some(tree);
                }
            }
            PolicyCheckpointResult::PartialRollback {
                recovered_text,
                recovered_edits,
                validated_tree,
                warning,
            } => {
                let checkpoint_ms = checkpoint_started.elapsed().as_secs_f64() * 1000.0;
                state.all_warnings.push(warning);
                let edit_count = recovered_edits.len();
                state.policy_traces.push(PolicyExecutionTrace {
                    policy: policy_name.into(),
                    parse_mode: if is_semantic_rewrite { "hybrid" } else { "tree-sitter" }.to_string(),
                    context_cluster: coordinated.context_cluster,
                    candidate_line_count: edit_count,
                    dropped_line_count: coordinated.result.edits.len().saturating_sub(edit_count),
                    semantic_impact_radius: coordinated.convergence_signal.impact_radius,
                    confidence_outcome: coordinated.confidence_outcome,
                    confidence_score: coordinated.confidence_score,
                    confidence_threshold: coordinated.confidence_threshold,
                    executor_scope: state.options.retry_scope_stage,
                    elapsed_ms: policy_started.elapsed().as_secs_f64() * 1000.0,
                    parse_ms, execute_ms, checkpoint_ms,
                    candidate_trace: Vec::new(),
                });
                state.telem.samples.push(PolicyExecutionSample::success(
                    policy_name,
                    policy_started.elapsed(),
                    edit_count,
                    0,
                ));
                state.current = Arc::from(recovered_text.as_str());
                state.parse.error_lines = None;
                state.all_edits.extend(recovered_edits);
                if let Some(tree) = validated_tree {
                    state.parse.tree = Some(tree);
                }
                if is_semantic_rewrite {
                    state.parse.invalidate(true, clang_invalidating);
                }
            }
            PolicyCheckpointResult::Rollback { reason, after_error_count } => {
                let mut retried = false;
                if state.retry_batch_size.is_none() && after_error_count > 0 {
                    let retry_batch = self.adaptive_state.load().retry_batch();
                    if retry_batch < state.current.lines().count() {
                        state.retry_batch_size = Some(retry_batch);
                        self.ensure_policy_parse_stage(state, policy, policy_name, policy_started)?;
                        let Some(prepared) = self.prepare_policy_stage(state, policy_name, policy_started) else {
                            state.all_warnings.push(reason);
                            return Ok(());
                        };
                        let retry_executed = self.execute_policy_stage(state, policy, prepared, policy_started)?;
                        if !text_scan::TEXT_SCAN.strings_equal(&retry_executed.result.text, &state.current) {
                            retried = true;
                            let retry_coordinated = self.coordinate_policy_stage(state, policy_name, retry_executed);
                            match self.checkpoint_policy_stage(state, &retry_coordinated, policy_name) {
                                PolicyCheckpointResult::Accept { validated_tree } => {
                                    self.commit_policy_stage(state, CommitPolicyInput {
                                        policy_name, policy_started, coordinated: retry_coordinated, is_semantic_rewrite, clang_invalidating,
                                        parse_ms, execute_ms, checkpoint_ms: checkpoint_started.elapsed().as_secs_f64() * 1000.0,
                                    });
                                    if let Some(tree) = validated_tree {
                                        state.parse.tree = Some(tree);
                                    }
                                    return Ok(());
                                }
                                PolicyCheckpointResult::PartialRollback {
                                    recovered_text, recovered_edits, validated_tree, warning,
                                } => {
                                    state.all_warnings.push(warning);
                                    let edit_count = recovered_edits.len();
                                    state.telem.samples.push(PolicyExecutionSample::success(
                                        policy_name, policy_started.elapsed(), edit_count, 0,
                                    ));
                                    state.current = Arc::from(recovered_text.as_str());
                                    state.parse.error_lines = None;
                                    state.all_edits.extend(recovered_edits);
                                    if let Some(tree) = validated_tree {
                                        state.parse.tree = Some(tree);
                                    }
                                    return Ok(());
                                }
                                PolicyCheckpointResult::Rollback { reason: retry_reason, .. } => {
                                    state.all_warnings.push(retry_reason);
                                }
                            }
                        }
                    }
                }
                if !retried {
                    state.all_warnings.push(reason);
                }
                state.telem.samples.push(PolicyExecutionSample::failed(
                    policy_name,
                    policy_started.elapsed(),
                    false,
                ));
            }
        }
        Ok(())
    }

    fn ensure_policy_parse_stage(
        &self,
        state: &mut PipelineState<'_>,
        _policy: &dyn Policy,
        policy_name: &str,
        policy_started: Instant,
    ) -> Result<()> {
        // Tree-sitter is always needed (all policies use structural parse context).
        // Clang is parsed eagerly when compdb context exists — all policies benefit from
        // fused clang+tree-sitter context (declarations, references, diagnostics).
        let needs_tree = state.parse.tree.is_none();
        let needs_clang = state.parse.clang.is_none()
            && state.parse.has_semantic_compdb;

        if needs_tree && needs_clang {
            // Dispatch clang (non-blocking). Then parse tree-sitter (fast, local),
            // then collect clang result with bounded deadline.
            let clang_handle = self.parser_manager.dispatch_clang(&state.current, state.path);
            let tree_result = self
                .parser_manager
                .reparse_tree(&state.current, state.path, state.parse.prev_tree.as_ref());
            match tree_result {
                Ok(value) => {
                    state.parse.tree = Some(value);
                    state.parse.compute_changed_ranges();
                    state.parse.prev_tree = None;
                }
                Err(err) => {
                    state.telem.samples.push(PolicyExecutionSample::failed(
                        policy_name,
                        policy_started.elapsed(),
                        false,
                    ));
                    PolicyTelemetry::record_batch(&state.telem.samples);
                    warn!(policy = policy_name, error = %err, "tree-sitter parse failed");
                    return Err(anyhow!("{policy_name}: {err}"));
                }
            }
            let clang_result = match clang_handle {
                Ok(Some(handle)) => {
                    let deadline = Instant::now() + std::time::Duration::from_secs(30);
                    self.parser_manager
                        .collect_clang(handle, &state.current, state.path, deadline)
                }
                Ok(None) => self.parser_manager.parse_clang(&state.current, state.path),
                Err(err) => Err(err),
            };
            match clang_result {
                Ok(value) => {
                    state.parse.clang = Some(value);
                    state.parse.clang_edits_since = 0;
                }
                Err(err) => {
                    state.telem.samples.push(PolicyExecutionSample::failed(
                        policy_name,
                        policy_started.elapsed(),
                        false,
                    ));
                    PolicyTelemetry::record_batch(&state.telem.samples);
                    warn!(policy = policy_name, error = %err, "clang parse failed");
                    return Err(anyhow!("{policy_name}: {err}"));
                }
            }
        } else if needs_tree {
            let parsed = match self
                .parser_manager
                .reparse_tree(&state.current, state.path, state.parse.prev_tree.as_ref())
            {
                Ok(value) => value,
                Err(err) => {
                    state.telem.samples.push(PolicyExecutionSample::failed(
                        policy_name,
                        policy_started.elapsed(),
                        false,
                    ));
                    PolicyTelemetry::record_batch(&state.telem.samples);
                    warn!(policy = policy_name, error = %err, "tree-sitter parse failed");
                    return Err(anyhow!("{policy_name}: {err}"));
                }
            };
            state.parse.tree = Some(parsed);
            state.parse.compute_changed_ranges();
            state.parse.prev_tree = None;
        } else if needs_clang {
            let parsed = match self.parser_manager.parse_clang(&state.current, state.path) {
                Ok(value) => value,
                Err(err) => {
                    state.telem.samples.push(PolicyExecutionSample::failed(
                        policy_name,
                        policy_started.elapsed(),
                        false,
                    ));
                    PolicyTelemetry::record_batch(&state.telem.samples);
                    warn!(policy = policy_name, error = %err, "clang parse failed");
                    return Err(anyhow!("{policy_name}: {err}"));
                }
            };
            state.parse.clang = Some(parsed);
            state.parse.clang_edits_since = 0;
        }
        if needs_clang && state.parse.semantic.is_some() {
            state.parse.semantic = None;
            state.parse.summary = None;
        }
        if state.parse.semantic.is_none() {
            let semantic = SemanticFileContext::from_parses_cached(
                &state.current, state.path,
                state.parse.tree.as_ref(),
                state.parse.clang.as_deref(),
                Some(&self.query_cache),
            );
            state.parse.summary = Some(semantic.summary());
            state.parse.semantic = Some(semantic);
        }
        if state.parse.comment_lines.is_none() {
            state.parse.comment_lines =
                Some(Self::comment_lines_from_tree(state.parse.tree.as_ref(), state.current.as_bytes(), &self.query_cache));
        }
        Ok(())
    }

    fn prepare_policy_stage(
        &self,
        _state: &mut PipelineState<'_>,
        policy_name: &str,
        _policy_started: Instant,
    ) -> Option<PreparedPolicyStage> {
        let capability = policy_catalog().capabilities_by_name(policy_name);
        let guidance_mode = self
            .policy_settings
            .get(policy_name)
            .map(|settings| SemanticContract::policy_guidance_mode(policy_name, settings))
            .unwrap_or(PolicyGuidanceMode::SoftGuideline);
        Some(PreparedPolicyStage {
            capability,
            guidance_mode,
        })
    }

    fn execute_policy_stage(
        &self,
        state: &mut PipelineState<'_>,
        policy: &dyn Policy,
        prepared: PreparedPolicyStage,
        policy_started: Instant,
    ) -> Result<ExecutedPolicyStage> {
        let policy_name = policy.name();
        let tree_error_lines = state.parse.error_lines_cached().clone();
        let shared = PolicySharedData::new(&state.current, state.parse.semantic.as_ref());
        let mut context = PolicyContext::new(&state.current, state.path)
            .with_tree(state.parse.tree.as_ref())
            .with_semantic(state.parse.semantic.as_ref())
            .with_graph(state.options.project_graph_snapshot.as_deref())
            .with_query_cache(Some(&self.query_cache))
            .with_shared(Some(&shared))
            .with_changed_ranges(state.parse.changed_ranges.as_deref());
        context.forced_batch_size = state.retry_batch_size.take();
        let semantic_query = context.semantic_query();
        let project_query = context.project_query();
        let mut result = policy.apply(&context);
        if let Some(sig) = result.rename_coverage_signal() {
            state.cand.rename_signal = Some(sig);
        }
        let disabled_lines = PolicySuppression::disabled_lines(&state.current, policy_name);
        if !disabled_lines.is_empty() {
            result = Self::apply_line_suppression(&state.current, result, &disabled_lines);
        }
        result = Self::apply_scope_filter(
            &state.current,
            result,
            state.options.allowed_edit_lines.as_ref(),
            ScopeFilterConfig {
                policy_name,
                scope_stage: state.options.retry_scope_stage.as_str(),
                capability: &prepared.capability,
            },
        );
        if let Some(fatal) = result
            .warnings
            .iter()
            .find(|warning| warning.starts_with("fatal:"))
        {
            state.telem.samples.push(PolicyExecutionSample::failed(
                policy_name,
                policy_started.elapsed(),
                true,
            ));
            PolicyTelemetry::record_batch(&state.telem.samples);
            warn!(policy = policy_name, reason = %fatal, "policy produced fatal warning");
            return Err(anyhow!("{policy_name}: {fatal}"));
        }
        result = Self::apply_semantic_mode(
            &state.current,
            result,
            &semantic_query,
            &prepared.capability,
            SemanticGuidanceConfig {
                policy_name,
                guidance_mode: prepared.guidance_mode,
                exact_compdb_for_file: state.parse.exact_compdb,
                semantic_context_kind: state.parse.semantic_kind,
            },
        );
        result = Self::normalize_edit_coverage(policy_name, &state.current, result);
        if result.edits.is_empty() && !text_scan::TEXT_SCAN.strings_equal(&result.text, &state.current) {
            result.text = state.current.to_string();
            result.warnings.push(format!(
                "policy output guard: reverted untracked text delta for '{}'",
                policy_name
            ));
        }

        // Understanding gate: remove edits targeting parser-error regions.
        // The parser tells us which lines it did not fully understand — we do not edit those.
        // This is structural gating, not threshold gating. No score, no 0.80.
        if !result.edits.is_empty() {
            let excluded = if prepared.capability.semantic_rewrite {
                // Semantic policies: also exclude lines in clang diagnostic-error regions
                let regions = state.parse.semantic
                    .as_ref()
                    .map(|s| s.regions.as_slice())
                    .unwrap_or(&[]);
                let mut exc = Self::semantic_error_lines(regions);
                exc.extend(tree_error_lines.iter().copied());
                exc
            } else if prepared.capability.structural_safe {
                // Structural-safe policies cannot break code structure by definition.
                // Per-policy checkpoint (tree-sitter re-parse) already validates after each edit.
                // Bypassing the understanding gate here allows edits that FIX parse errors
                // (e.g., TAB→space, alternative tokens) instead of blocking them.
                BTreeSet::new()
            } else {
                // Other syntactic policies: exclude tree-sitter error lines
                tree_error_lines
            };
            if !excluded.is_empty() {
                let before_count = result.edits.len();
                result.edits.retain(|e| e.line == 0 || !excluded.contains(&e.line));
                let dropped = before_count.saturating_sub(result.edits.len());
                if dropped > 0 {
                    result.warnings.push(format!(
                        "understanding gate: suppressed {} edit(s) on parser-error line(s) for '{}'",
                        dropped, policy_name
                    ));
                    if result.edits.is_empty() {
                        result.text = state.current.to_string();
                    } else {
                        let resynthesized = Self::apply_edits_lenient(
                            &state.current, &result.edits,
                        );
                        match resynthesized {
                            Some(text) => result.text = text,
                            None => {
                                result.text = state.current.to_string();
                                result.edits.clear();
                                result.warnings.push(format!(
                                    "understanding gate: escalated to full rollback for '{}'",
                                    policy_name
                                ));
                            }
                        }
                    }
                }
            }
        }

        let candidate_lines = Self::edit_lines(result.edits.as_slice());
        let context_cluster = if candidate_lines.is_empty() {
            0
        } else {
            project_query.context_cluster_key(&candidate_lines)
        };
        let cluster_adaptive = self.adaptive_state.load();
        let cluster_controls =
            PolicyClusterTelemetry::adaptive_controls(policy_name, context_cluster, &cluster_adaptive);

        let mut confidence_sample = None;
        let mut confidence_outcome = None;
        let mut confidence_score = None;
        let mut confidence_threshold = None;
        let mut dropped_line_count = 0usize;
        if let Some(settings) = self.policy_settings.get(policy_name) {
            if self.confidence_enabled && !result.edits.is_empty() {
                let is_semantic = prepared.capability.semantic_rewrite
                    && state.parse.has_semantic_compdb;
                if is_semantic
                    && state.parse.tree.is_some()
                    && state.parse.clang.is_none()
                {
                    result.warnings.push(
                        "policy confidence mode downgraded to 'tree-sitter' due unavailable clang semantic context".to_string(),
                    );
                }
                let score = 1.0;
                let base_enforcement = if settings.has_key("enforcement") {
                    settings.enforcement
                } else {
                    self.confidence_default_enforcement
                };
                let outcome = PolicyDecisionOutcome::Apply;
                let decision = ConfidenceGateDecision {
                    outcome,
                    score,
                    threshold: 0.0,
                    base_enforcement,
                    effective_enforcement: base_enforcement,
                    reason_codes: vec![ConfidenceReasonCode::Stable],
                    dropped_lines: BTreeSet::new(),
                };
                let adjusted_decision = Self::apply_cluster_bias(
                    decision,
                    cluster_controls.enforcement_bias,
                );
                confidence_sample = Some(PolicyConfidenceSample::from_reason_codes(
                    adjusted_decision.outcome,
                    adjusted_decision.reason_codes.as_slice(),
                ));
                confidence_outcome = Some(adjusted_decision.outcome);
                confidence_score = Some(adjusted_decision.score);
                confidence_threshold = Some(adjusted_decision.threshold);
                dropped_line_count = adjusted_decision.dropped_lines.len();
                AdaptiveTelemetry::record_confidence_gate(
                    adjusted_decision.outcome,
                    adjusted_decision.reason_codes.as_slice(),
                );
                PolicyClusterTelemetry::record_decision(
                    policy_name,
                    context_cluster,
                    adjusted_decision.outcome,
                );
                result = Self::apply_confidence_decision(
                    policy_name,
                    &state.current,
                    result,
                    adjusted_decision,
                );
            }
        }

        let convergence_signal = Self::build_convergence_signal(ConvergenceSignalInput {
            result: &result,
            semantic: state.parse.semantic.as_ref(),
            summary: state.parse.summary_or_default(),
            previous_contract_failures: &state.options.previous_contract_failures,
            capability: &prepared.capability,
            cluster_radius_cap: cluster_controls.max_impact_radius_cap,
            adaptive: &cluster_adaptive,
        });

        Ok(ExecutedPolicyStage {
            result,
            capability: prepared.capability,
            context_cluster,
            confidence_sample,
            confidence_outcome,
            confidence_score,
            confidence_threshold,
            dropped_line_count,
            convergence_signal,
        })
    }

    fn coordinate_policy_stage(
        &self,
        state: &mut PipelineState<'_>,
        policy_name: &str,
        mut executed: ExecutedPolicyStage,
    ) -> CoordinatedPolicyStage {
        let context = PolicyContext::new(&state.current, state.path)
            .with_tree(state.parse.tree.as_ref())
            .with_semantic(state.parse.semantic.as_ref())
            .with_graph(state.options.project_graph_snapshot.as_deref())
            .with_query_cache(Some(&self.query_cache));
        let project_query = context.project_query();
        let edit_block_index = Self::dominant_block_index(state, &executed.result.edits);
        let context_mod = self.context_modifier_for_policy(state, policy_name, Some(edit_block_index));
        let candidate_confidence = executed.confidence_score.unwrap_or(1.0) * context_mod;
        let adaptive_snap = self.adaptive_state.load();
        let proposed_candidates = state.cand.proposer.propose(
            policy_name,
            &executed.result,
            &crate::engine::proposer::ProposalContext {
                project_query: &project_query,
                comment_lines: state.parse.comment_lines.as_ref(),
                convergence_signal: &executed.convergence_signal,
                capability: &executed.capability,
                confidence: candidate_confidence,
                adaptive: &adaptive_snap,
            },
        );
        let guardian_assessment = ProposerController::assess(
            proposed_candidates.as_slice(), &executed.capability);
        let disallowed_zone_lines = guardian_assessment.blocked_zone_lines;
        let guardian_hard_blocked_lines = guardian_assessment.hard_blocked_lines;
        let mut guardian_suppressed_lines = BTreeSet::new();
        guardian_suppressed_lines.extend(disallowed_zone_lines.iter().copied());
        guardian_suppressed_lines.extend(guardian_hard_blocked_lines.iter().copied());
        let mut candidates = guardian_assessment.allowed;
        if !guardian_suppressed_lines.is_empty() {
            executed.result = if executed.capability.structural_safe {
                Self::suppress_structural(
                    &state.current,
                    executed.result,
                    &guardian_suppressed_lines,
                )
            } else {
                Self::apply_line_suppression(
                    &state.current,
                    executed.result,
                    &guardian_suppressed_lines,
                )
            };
            candidates.retain(|candidate| !guardian_suppressed_lines.contains(&candidate.line));
            executed.dropped_line_count = executed
                .dropped_line_count
                .saturating_add(guardian_suppressed_lines.len());
        }
        if !disallowed_zone_lines.is_empty() {
            executed.result.warnings.push(format!(
                "guardian controller: dropped {} line(s) for '{}' touching disallowed zones",
                disallowed_zone_lines.len(),
                policy_name
            ));
        }
        if !guardian_hard_blocked_lines.is_empty() {
            executed.result.warnings.push(format!(
                "guardian controller: blocked {} line(s) for '{}' due to hard semantic constraints",
                guardian_hard_blocked_lines.len(),
                policy_name
            ));
        }

        let solve_result = GlobalConflictSolver::solve(
            candidates.as_slice(),
            state.cand.selected.as_slice(),
            state.options.retry_scope_stage,
            &adaptive_snap,
        );
        let mut hard_blocked_lines = guardian_hard_blocked_lines;
        hard_blocked_lines.extend(solve_result.hard_blocked_lines.iter().copied());
        if !solve_result.hard_blocked_lines.is_empty() {
            executed.result.warnings.push(format!(
                "global conflict solver: blocked {} line(s) for '{}' due to hard semantic constraints",
                solve_result.hard_blocked_lines.len(),
                policy_name
            ));
        }
        executed.convergence_signal.solver_dropped_lines = solve_result
            .dropped_lines
            .len()
            .saturating_add(guardian_suppressed_lines.len());
        executed.convergence_signal.hard_blocked_lines = hard_blocked_lines.len();
        if !solve_result.dropped_lines.is_empty() {
            if executed.capability.semantic_rewrite {
                executed.dropped_line_count = executed
                    .dropped_line_count
                    .saturating_add(solve_result.dropped_lines.len());
                executed.result = PolicyResult {
                    text: state.current.to_string(),
                    violations: executed.result.violations,
                    edits: Vec::new(),
                    warnings: executed.result.warnings,
                    changed: false,
                };
                candidates.clear();
                executed.result.warnings.push(format!(
                    "global conflict solver: reverted semantic rewrite '{}' because {} conflicting line(s) would cause partial propagation",
                    policy_name,
                    solve_result.dropped_lines.len()
                ));
            } else if policy_catalog()
                .behavior_by_name(policy_name)
                .keeps_nonlocal_batch
                && Self::has_nonlocal_change(
                    &state.current,
                    executed.result.text.as_str(),
                )
            {
                executed.result.warnings.push(format!(
                    "global conflict solver: kept non-local '{}' batch ({} conflicting line(s)) to avoid unsafe partial rollback",
                    policy_name,
                    solve_result.dropped_lines.len(),
                ));
            } else {
                executed.result = if executed.capability.structural_safe {
                    Self::suppress_structural(
                        &state.current,
                        executed.result,
                        &solve_result.dropped_lines,
                    )
                } else {
                    Self::apply_line_suppression(
                        &state.current,
                        executed.result,
                        &solve_result.dropped_lines,
                    )
                };
                candidates
                    .retain(|candidate| !solve_result.dropped_lines.contains(&candidate.line));
                executed.dropped_line_count = executed
                    .dropped_line_count
                    .saturating_add(solve_result.dropped_lines.len());
                executed.result.warnings.push(format!(
                    "global conflict solver: dropped {} conflicting line(s) for '{}' before commit",
                    solve_result.dropped_lines.len(),
                    policy_name
                ));
            }
        }
        executed.result = state.convergence_controller.reconcile_policy_result(
            policy_name,
            &state.current,
            executed.result,
        );
        executed.result =
            Self::normalize_edit_coverage(policy_name, &state.current, executed.result);
        if executed.result.edits.is_empty() && !text_scan::TEXT_SCAN.strings_equal(&executed.result.text, &state.current) {
            executed.result.text = state.current.to_string();
            executed.result.warnings.push(format!(
                "post-arbiter guard: reverted untracked text delta for '{}'",
                policy_name
            ));
        }
        let kept_lines = Self::edit_lines(executed.result.edits.as_slice());
        let convergence_total = solve_result.accepted.len();
        let accepted_candidates = solve_result
            .accepted
            .into_iter()
            .filter(|candidate| kept_lines.contains(&candidate.line))
            .collect::<Vec<_>>();
        let convergence_dropped = convergence_total
            .saturating_sub(accepted_candidates.len());
        executed.dropped_line_count = executed
            .dropped_line_count
            .saturating_add(convergence_dropped);
        let candidate_trace = Self::build_policy_candidate_trace(
            proposed_candidates.as_slice(),
            state.options.retry_scope_stage,
            &disallowed_zone_lines,
            &hard_blocked_lines,
            &solve_result.dropped_lines,
            &kept_lines,
            &adaptive_snap,
        );
        let conflict_violations = state.cand.conflict
            .observe(policy_name, executed.result.edits.as_slice());
        let text_changed = !text_scan::TEXT_SCAN.strings_equal(&executed.result.text, &state.current);

        CoordinatedPolicyStage {
            result: executed.result,
            accepted_candidates,
            candidate_trace,
            conflict_violations,
            context_cluster: executed.context_cluster,
            confidence_sample: executed.confidence_sample,
            confidence_outcome: executed.confidence_outcome,
            confidence_score: executed.confidence_score,
            confidence_threshold: executed.confidence_threshold,
            dropped_line_count: executed.dropped_line_count,
            convergence_signal: executed.convergence_signal,
            text_changed,
        }
    }

    fn commit_policy_stage(
        &self,
        state: &mut PipelineState<'_>,
        input: CommitPolicyInput<'_>,
    ) {
        let CommitPolicyInput {
            policy_name,
            policy_started,
            coordinated,
            is_semantic_rewrite,
            clang_invalidating,
            parse_ms,
            execute_ms,
            checkpoint_ms,
        } = input;
        Self::append_selected_candidates(
            &mut state.cand.selected,
            coordinated.accepted_candidates.as_slice(),
        );
        state.cand.internal
            .extend(coordinated.accepted_candidates.iter().cloned());

        state.policy_traces.push(PolicyExecutionTrace {
            policy: policy_name.into(),
            parse_mode: if is_semantic_rewrite { "hybrid" } else { "tree-sitter" }.to_string(),
            context_cluster: coordinated.context_cluster,
            candidate_line_count: coordinated.accepted_candidates.len(),
            dropped_line_count: coordinated.dropped_line_count,
            semantic_impact_radius: coordinated.convergence_signal.impact_radius,
            confidence_outcome: coordinated.confidence_outcome,
            confidence_score: coordinated.confidence_score,
            confidence_threshold: coordinated.confidence_threshold,
            executor_scope: state.options.retry_scope_stage,
            elapsed_ms: policy_started.elapsed().as_secs_f64() * 1000.0,
            parse_ms, execute_ms, checkpoint_ms,
            candidate_trace: coordinated.candidate_trace,
        });

        let summary = state.parse.summary_or_default();
        let mut sample = PolicyExecutionSample::success(
            policy_name,
            policy_started.elapsed(),
            coordinated.result.edits.len(),
            coordinated.result.violations.len(),
        );
        if let Some(confidence) = coordinated.confidence_sample {
            sample = sample.with_confidence(confidence);
        }
        state.telem.samples.push(sample);
        debug!(
            policy = policy_name,
            edits = coordinated.result.edits.len(),
            violations = coordinated.result.violations.len(),
            text_changed = coordinated.text_changed,
            semantic_declarations = summary.declaration_count,
            semantic_references = summary.reference_count,
            semantic_scopes = summary.scope_count,
            semantic_preprocessor_scopes = summary.preprocessor_scope_count,
            semantic_usr_decls = summary.usr_backed_declaration_count,
            semantic_errors = summary.diagnostic_error_count,
            internal_candidate_count = state.cand.internal.len(),
            "policy applied"
        );
        state.current = Arc::from(coordinated.result.text);
        state.all_violations.extend(coordinated.result.violations);
        state.all_violations.extend(coordinated.conflict_violations);
        state.all_edits.extend(coordinated.result.edits);
        state.all_warnings.extend(coordinated.result.warnings);
        if coordinated.text_changed {
            state.parse.invalidate(is_semantic_rewrite, clang_invalidating);
        }
    }


    fn append_selected_candidates(
        selected: &mut Vec<PolicyEditCandidate>,
        candidates: &[PolicyEditCandidate],
    ) {
        if candidates.is_empty() {
            return;
        }
        selected.extend(candidates.iter().cloned());
    }

    fn build_policy_candidate_trace(
        all_candidates: &[PolicyEditCandidate],
        scope_stage: crate::engine::run_options::RetryScopeStage,
        disallowed_zone_lines: &BTreeSet<usize>,
        hard_blocked_lines: &BTreeSet<usize>,
        dropped_lines: &BTreeSet<usize>,
        kept_lines: &BTreeSet<usize>,
        adaptive: &crate::engine::certainty_filter::CertaintyFilterState,
    ) -> Vec<PolicyCandidateTrace> {
        let mut traces = all_candidates
            .iter()
            .map(|candidate| {
                let outcome = if kept_lines.contains(&candidate.line) {
                    PolicyCandidateOutcome::Selected
                } else if disallowed_zone_lines.contains(&candidate.line) {
                    PolicyCandidateOutcome::BlockedZone
                } else if hard_blocked_lines.contains(&candidate.line) {
                    PolicyCandidateOutcome::BlockedHardConstraint
                } else if dropped_lines.contains(&candidate.line) {
                    PolicyCandidateOutcome::DroppedConflict
                } else {
                    PolicyCandidateOutcome::DroppedConvergence
                };
                PolicyCandidateTrace {
                    line: candidate.line,
                    confidence: candidate.confidence,
                    style_gain: candidate.style_gain,
                    utility: GlobalConflictSolver::utility_score(candidate, scope_stage, adaptive),
                    risk_tier: candidate.risk_tier,
                    impact_radius: candidate.impact_radius,
                    symbol_footprint_count: candidate.symbol_footprint.len(),
                    range_footprint_count: candidate.range_footprint.len(),
                    hard_constraints_touched: crate::engine::semantic_contract::ALL_CLAUSES
                        .iter()
                        .filter(|&&clause| (candidate.hard_constraints_touched & clause.bit()) != 0)
                        .copied()
                        .collect(),
                    zone: candidate.zone,
                    outcome,
                }
            })
            .collect::<Vec<_>>();
        traces.sort_by(|left, right| {
            left.line
                .cmp(&right.line)
                .then(left.outcome.as_str().cmp(right.outcome.as_str()))
                .then(left.risk_tier.as_str().cmp(right.risk_tier.as_str()))
        });
        traces
    }

}

#[cfg(test)]
mod tests {
    use super::{ConvergenceSignalInput, ScopeFilterConfig, SemanticGuidanceConfig};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use toml::Table;

    use crate::config::policy_config::PolicyConfig;
    use crate::engine::certainty_filter::CertaintyFilterState;
    use crate::engine::conflict_solver::GlobalConflictSolver;
    use crate::engine::catalog::{
        PolicyCapabilities, policy_catalog,
    };
    use crate::engine::edit_candidate::{CandidateRiskTier, PolicyEditCandidate};
    use crate::engine::pipeline::PolicyPipeline;
    use crate::engine::run_options::RetryScopeStage;
    use crate::policy::zone::PolicyZone;
    use crate::engine::semantic_contract::{PolicyGuidanceMode, SemanticInvariantClause};
    use crate::model::edit::Edit;
    use crate::model::policy_result::PolicyResult;
    use crate::model::context_query::SemanticContextQuery;
    use crate::model::violation::Violation;
    use crate::parser::clang_result::ClangDiagnosticEntry;

    use crate::parser::manager::SemanticCompdbContextKind;
    use crate::parser::file_context::{
        SemanticDeclaration, SemanticFileContext, SemanticIdProvenance, SemanticReference,
        SemanticScope,
    };
    #[test]
    fn guideline_drops_unsafe() {
        let semantic = SemanticFileContext {
            scopes: vec![SemanticScope {
                node_kind_id: crate::parser::ts_cpp_symbols::sym_preproc_if,
                start_offset: 0,
                end_offset: 16,
                start_line: 2,
                end_line: 2,
            }],
            ..SemanticFileContext::default()
        };
        let query = SemanticContextQuery::from_semantic(Some(&semantic));
        let before = "line1\nline2\nline3\n";
        let result = PolicyResult {
            text: "line1\nunsafe\nsafe\n".to_string(),
            violations: Vec::new(),
            edits: vec![
                Edit {
                    policy: "sample".into(),
                    line: 2,
                    before: "line2".to_string(),
                    after: "unsafe".to_string(),
                },
                Edit {
                    policy: "sample".into(),
                    line: 3,
                    before: "line3".to_string(),
                    after: "safe".to_string(),
                },
            ],
            warnings: Vec::new(),
            changed: true,
        };
        let capability = policy_catalog().capabilities_by_name("sample");

        let adjusted = PolicyPipeline::apply_semantic_mode(
            before,
            result,
            &query,
            &capability,
            SemanticGuidanceConfig {
                policy_name: "sample",
                guidance_mode: PolicyGuidanceMode::SoftGuideline,
                exact_compdb_for_file: true,
                semantic_context_kind: SemanticCompdbContextKind::Exact,
            },
        );
        assert_eq!(adjusted.edits.len(), 1);
        assert_eq!(adjusted.edits[0].line, 3);
        assert_eq!(adjusted.text, "line1\nline2\nsafe\n");
        assert!(adjusted
            .warnings
            .iter()
            .any(|warning| warning.contains("semantic guideline dropped")));
    }

    #[test]
    fn invariant_blocks_unsafe() {
        let semantic = SemanticFileContext {
            diagnostic_entries: vec![ClangDiagnosticEntry {
                line: 4,
                column: 1,
                severity: clang_sys::CXDiagnostic_Error as u32,
                warning_option: String::new(),
                fix_its: Vec::new(),
            }],
            declarations: vec![SemanticDeclaration {
                stable_id: "usr:c:@F@unsafe#".to_string(),
                provenance: SemanticIdProvenance::Usr,
                name: "unsafe".to_string(),
                kind: clang_sys::CXCursor_FunctionDecl,
                line: 4,
                column: 1,
                usr: Some("c:@F@unsafe#".to_string()),
                scope_usr: None,
                canonical_type_kind: clang_sys::CXType_Unexposed,
                ..Default::default()
            }],
            ..SemanticFileContext::default()
        };
        let query = SemanticContextQuery::from_semantic(Some(&semantic));
        let before = "line1\nline2\nline3\nline4\n";
        let result = PolicyResult {
            text: "line1\nline2\nline3\nunsafe\n".to_string(),
            violations: Vec::new(),
            edits: vec![Edit {
                policy: "sample".into(),
                line: 4,
                before: "line4".to_string(),
                after: "unsafe".to_string(),
            }],
            warnings: Vec::new(),
            changed: true,
        };
        let capability = policy_catalog().capabilities_by_name("sample");

        let adjusted = PolicyPipeline::apply_semantic_mode(
            before,
            result,
            &query,
            &capability,
            SemanticGuidanceConfig {
                policy_name: "sample",
                guidance_mode: PolicyGuidanceMode::HardInvariant,
                exact_compdb_for_file: true,
                semantic_context_kind: SemanticCompdbContextKind::Exact,
            },
        );
        assert_eq!(adjusted.text, before);
        assert!(adjusted.edits.is_empty());
        assert!(adjusted
            .violations
            .iter()
            .any(|violation| violation.policy == "semantic_contract"));
    }

    #[test]
    fn guideline_allows_structural() {
        let semantic = SemanticFileContext {
            scopes: vec![SemanticScope {
                node_kind_id: crate::parser::ts_cpp_symbols::sym_preproc_include,
                start_offset: 0,
                end_offset: 20,
                start_line: 2,
                end_line: 2,
            }],
            ..SemanticFileContext::default()
        };
        let query = SemanticContextQuery::from_semantic(Some(&semantic));
        let before = "line1\n#include <a>\nline3\n";
        let result = PolicyResult {
            text: "line1\n#include <b>\nline3\n".to_string(),
            violations: Vec::new(),
            edits: vec![Edit {
                policy: "include_order".into(),
                line: 2,
                before: "#include <a>".to_string(),
                after: "#include <b>".to_string(),
            }],
            warnings: Vec::new(),
            changed: true,
        };
        let capability = policy_catalog().capabilities_by_name("include_order");

        let adjusted = PolicyPipeline::apply_semantic_mode(
            before,
            result,
            &query,
            &capability,
            SemanticGuidanceConfig {
                policy_name: "include_order",
                guidance_mode: PolicyGuidanceMode::SoftGuideline,
                exact_compdb_for_file: true,
                semantic_context_kind: SemanticCompdbContextKind::Exact,
            },
        );
        assert_eq!(adjusted.edits.len(), 1);
        assert_eq!(adjusted.text, "line1\n#include <b>\nline3\n");
        assert!(!adjusted
            .warnings
            .iter()
            .any(|warning| warning.contains("semantic guideline dropped")));
    }

    #[test]
    fn scope_drops_outside() {
        let before = "l1\nl2\nl3\nl4\n";
        let result = PolicyResult {
            text: "l1\nx2\nx3\nx4\n".to_string(),
            violations: Vec::new(),
            edits: vec![
                Edit {
                    policy: "sample".into(),
                    line: 2,
                    before: "l2".to_string(),
                    after: "x2".to_string(),
                },
                Edit {
                    policy: "sample".into(),
                    line: 4,
                    before: "l4".to_string(),
                    after: "x4".to_string(),
                },
            ],
            warnings: Vec::new(),
            changed: true,
        };
        let allowed = BTreeSet::from([4usize]);
        let filtered = PolicyPipeline::apply_scope_filter(
            before,
            result,
            Some(&allowed),
            ScopeFilterConfig {
                policy_name: "sample",
                scope_stage: "line_local",
                capability: &PolicyCapabilities {
                    semantic_rewrite: false,
                    structural_safe: true,
                    whitespace_safe: false,
                    ..policy_catalog().capabilities_by_name("sample")
                },
            },
        );
        assert_eq!(filtered.edits.len(), 1);
        assert_eq!(filtered.edits[0].line, 4);
        assert_eq!(filtered.text, "l1\nl2\nx3\nx4\n");
        assert!(filtered
            .warnings
            .iter()
            .any(|warning| warning.contains("retry_scope(line_local)")));
    }

    #[test]
    fn scope_reverts_rewrite() {
        let before = "a\nb\nc\n";
        let result = PolicyResult {
            text: "x\ny\nc\n".to_string(),
            violations: Vec::new(),
            edits: vec![
                Edit {
                    policy: "naming_conventions".into(),
                    line: 1,
                    before: "a".to_string(),
                    after: "x".to_string(),
                },
                Edit {
                    policy: "naming_conventions".into(),
                    line: 2,
                    before: "b".to_string(),
                    after: "y".to_string(),
                },
            ],
            warnings: Vec::new(),
            changed: true,
        };
        let allowed = BTreeSet::from([1usize]);
        let filtered = PolicyPipeline::apply_scope_filter(
            before,
            result,
            Some(&allowed),
            ScopeFilterConfig {
                policy_name: "naming_conventions",
                scope_stage: "line_local",
                capability: &PolicyCapabilities {
                    semantic_rewrite: true,
                    structural_safe: false,
                    whitespace_safe: false,
                    ..policy_catalog().capabilities_by_name("naming_conventions")
                },
            },
        );
        assert_eq!(filtered.text, before);
        assert!(filtered.edits.is_empty());
        assert!(filtered
            .warnings
            .iter()
            .any(|warning| warning.contains("reverted semantic rewrite")));
    }

    #[test]
    fn scope_skips_saturated() {
        let before = "l1\nl2\nl3\n";
        let mut result = PolicyResult {
            text: "x1\nx2\nx3\nx4\nx5\nx6\n".to_string(),
            violations: Vec::new(),
            edits: Vec::new(),
            warnings: Vec::new(),
            changed: true,
        };
        for line in 1..=90usize {
            result.edits.push(Edit {
                policy: "class_layout".into(),
                line,
                before: format!("b{line}"),
                after: format!("a{line}"),
            });
        }
        let allowed = BTreeSet::from([1usize]);
        let filtered = PolicyPipeline::apply_scope_filter(
            before,
            result,
            Some(&allowed),
            ScopeFilterConfig {
                policy_name: "class_layout",
                scope_stage: "line_local",
                capability: &PolicyCapabilities {
                    semantic_rewrite: false,
                    structural_safe: true,
                    whitespace_safe: false,
                    ..policy_catalog().capabilities_by_name("class_layout")
                },
            },
        );
        assert!(filtered.warnings.iter().any(|warning| warning.contains("dropped") && warning.contains("out-of-scope")),
            "scope filter should drop out-of-scope lines without full revert");
    }

    #[test]
    fn wide_requires_compdb() {
        assert!(PolicyPipeline::needs_exact_compdb(
            "clang_format"
        ));
        assert!(PolicyPipeline::needs_exact_compdb(
            "include_order"
        ));
        assert!(PolicyPipeline::needs_exact_compdb(
            "class_layout"
        ));
        assert!(PolicyPipeline::needs_exact_compdb(
            "compact_declarations"
        ));
    }

    #[test]
    fn profile_reads_radius() {
        let mut policy_table = Table::new();
        policy_table.insert(
            "name".to_string(),
            toml::Value::String("policy_a".to_string()),
        );
        policy_table.insert("enabled".to_string(), toml::Value::Boolean(true));
        let mut convergence = Table::new();
        convergence.insert(
            "domain".to_string(),
            toml::Value::String("layout".to_string()),
        );
        convergence.insert("priority".to_string(), toml::Value::Integer(777));
        convergence.insert("impact_radius".to_string(), toml::Value::Integer(3));
        convergence.insert(
            "priority_weight_bp".to_string(),
            toml::Value::Integer(650),
        );
        policy_table.insert("convergence".to_string(), toml::Value::Table(convergence));
        let policy = PolicyConfig::from_policy_table(&policy_table).expect("policy config");
        let profiles = PolicyPipeline::build_convergence_profiles(&[(
            "policy_a".to_string(),
            policy,
        )].into_iter().collect());
        let profile = profiles.get("policy_a").expect("profile");
        assert_eq!(profile.domain, "layout");
        assert_eq!(profile.priority, 777);
        assert_eq!(profile.impact_radius, 3);
        assert_eq!(profile.priority_weight_bp, 650);
    }

    #[test]
    fn signal_increases_radius() {
        let result = PolicyResult {
            text: "x".to_string(),
            violations: Vec::new(),
            edits: (0..80)
                .map(|line| Edit {
                    policy: "p".into(),
                    line: line + 1,
                    before: "a".to_string(),
                    after: "b".to_string(),
                })
                .collect(),
            warnings: Vec::new(),
            changed: true,
        };
        let semantic = SemanticFileContext {
            clang_success: true,
            tree_has_error: false,
            diagnostic_counts: [0; 5],
            declarations: vec![],
            references: (0..2_100)
                .map(|offset| SemanticReference {
                    stable_id: "usr:test".to_string(),
                    provenance: SemanticIdProvenance::Usr,
                    decl_path: "sample.cpp".to_string(),
                    decl_kind: clang_sys::CXCursor_VarDecl,
                    offset,
                    line: 1,
                    column: 1,
                })
                .collect(),
            ..SemanticFileContext::default()
        };
        let summary = semantic.summary();
        let capability = policy_catalog().capabilities_by_name("compact_declarations");
        let signal = PolicyPipeline::build_convergence_signal(ConvergenceSignalInput {
            result: &result,
            semantic: Some(&semantic),
            summary,
            previous_contract_failures: &BTreeSet::new(),
            capability: &capability,
            cluster_radius_cap: None,
            adaptive: &CertaintyFilterState::new(),
        });
        assert!(signal.semantic_confidence_bp >= 700);
        assert!(signal.impact_radius >= 2);
    }

    #[test]
    fn signal_builds_maps() {
        let result = PolicyResult {
            text: "void f() {}\n".to_string(),
            violations: Vec::new(),
            edits: vec![
                Edit {
                    policy: "p".into(),
                    line: 10,
                    before: "int value=1;".to_string(),
                    after: "int value = 1;".to_string(),
                },
                Edit {
                    policy: "p".into(),
                    line: 12,
                    before: "value++;".to_string(),
                    after: "value ++;".to_string(),
                },
            ],
            warnings: Vec::new(),
            changed: true,
        };
        let semantic = SemanticFileContext {
            clang_success: true,
            tree_has_error: false,
            declarations: vec![SemanticDeclaration {
                stable_id: "usr:c:@F@value#".to_string(),
                provenance: SemanticIdProvenance::Usr,
                name: "value".to_string(),
                kind: clang_sys::CXCursor_VarDecl,
                line: 10,
                column: 5,
                usr: Some("c:@F@value#".to_string()),
                scope_usr: None,
                canonical_type_kind: clang_sys::CXType_Unexposed,
                ..Default::default()
            }],
            references: vec![SemanticReference {
                stable_id: "usr:c:@F@value#".to_string(),
                provenance: SemanticIdProvenance::Usr,
                decl_path: "sample.cpp".to_string(),
                decl_kind: clang_sys::CXCursor_VarDecl,
                offset: 0,
                line: 12,
                column: 3,
            }],
            scopes: vec![SemanticScope {
                node_kind_id: crate::parser::ts_cpp_symbols::sym_function_definition,
                start_offset: 0,
                end_offset: 100,
                start_line: 8,
                end_line: 20,
            }],
            diagnostic_counts: [0; 5],
            ..SemanticFileContext::default()
        };
        let capability = policy_catalog().capabilities_by_name("compact_declarations");
        let signal = PolicyPipeline::build_convergence_signal(ConvergenceSignalInput {
            result: &result,
            semantic: Some(&semantic),
            summary: semantic.summary(),
            previous_contract_failures: &BTreeSet::new(),
            capability: &capability,
            cluster_radius_cap: None,
            adaptive: &CertaintyFilterState::new(),
        });

        let line10_ranges = signal
            .impact_ranges
            .get(&10)
            .expect("line 10 ranges");
        assert!(line10_ranges
            .iter()
            .any(|(start, end)| *start <= 10 && *end >= 12));

        let line12_ranges = signal
            .impact_ranges
            .get(&12)
            .expect("line 12 ranges");
        assert!(line12_ranges
            .iter()
            .any(|(start, end)| *start <= 10 && *end >= 12));

        let ids_line10 = signal
            .symbol_ids
            .get(&10)
            .expect("line 10 symbol ids");
        let ids_line12 = signal
            .symbol_ids
            .get(&12)
            .expect("line 12 symbol ids");
        assert!(ids_line10.iter().any(|id| ids_line12.contains(id)));
    }

    #[test]
    fn signal_penalizes_failures() {
        let result = PolicyResult {
            text: "x".to_string(),
            violations: Vec::new(),
            edits: vec![Edit {
                policy: "p".into(),
                line: 10,
                before: "a".to_string(),
                after: "b".to_string(),
            }],
            warnings: Vec::new(),
            changed: true,
        };
        let semantic = SemanticFileContext {
            clang_success: true,
            tree_has_error: false,
            diagnostic_counts: [0; 5],
            ..SemanticFileContext::default()
        };
        let capability = policy_catalog().capabilities_by_name("compact_declarations");
        let base = PolicyPipeline::build_convergence_signal(ConvergenceSignalInput {
            result: &result,
            semantic: Some(&semantic),
            summary: semantic.summary(),
            previous_contract_failures: &BTreeSet::new(),
            capability: &capability,
            cluster_radius_cap: None,
            adaptive: &CertaintyFilterState::new(),
        });
        let failures = BTreeSet::from([
            SemanticInvariantClause::EditSafety,
            SemanticInvariantClause::DeclarationReferenceIntegrity,
        ]);
        let penalized = PolicyPipeline::build_convergence_signal(ConvergenceSignalInput {
            result: &result,
            semantic: Some(&semantic),
            summary: semantic.summary(),
            previous_contract_failures: &failures,
            capability: &capability,
            cluster_radius_cap: None,
            adaptive: &CertaintyFilterState::new(),
        });
        assert!(penalized.semantic_confidence_bp < base.semantic_confidence_bp);
        assert!(penalized.impact_radius >= base.impact_radius);
    }

    #[test]
    fn suppression_keeps_local() {
        let before = "int x = 0;\n\nint y = 1;\n";
        let result = PolicyResult {
            text: "int x = 0;\n    \nint y = 1;\n".to_string(),
            violations: vec![Violation {
                policy: "compact_declarations".into(),
                message: "blank line spacing".to_string(),
                line: 2,
                column: Some(1),
            }],
            edits: vec![Edit {
                policy: "compact_declarations".into(),
                line: 2,
                before: "".to_string(),
                after: "    ".to_string(),
            }],
            warnings: Vec::new(),
            changed: true,
        };
        let suppressed =
            PolicyPipeline::apply_line_suppression(before, result, &BTreeSet::from([3usize]));
        assert_eq!(suppressed.text, "int x = 0;\n    \nint y = 1;\n");
        assert_eq!(suppressed.edits.len(), 1);
    }

    #[test]
    fn suppression_handles_nonlocal() {
        let before = "A\nB\nC\n";
        let result = PolicyResult {
            text: "A\nB\nX\nC\n".to_string(),
            violations: vec![],
            edits: vec![Edit {
                policy: "class_layout".into(),
                line: 3,
                before: "C".to_string(),
                after: "X".to_string(),
            }],
            warnings: vec![],
            changed: true,
        };
        let suppressed =
            PolicyPipeline::apply_line_suppression(before, result, &BTreeSet::from([3usize]));
        assert_eq!(suppressed.text, "A\nB\nC\n");
        assert!(suppressed
            .warnings
            .iter()
            .any(|warning| warning.contains("best-effort non-local rollback")));
    }

    #[test]
    fn suppression_no_revert() {
        let before_lines: Vec<String> = (1..=70).map(|n| format!("{n}")).collect();
        let before = format!("{}\n", before_lines.join("\n"));
        let mut after_lines = before_lines.clone();
        after_lines.insert(2, "X".to_string());
        let after_text = format!("{}\n", after_lines.join("\n"));
        let result = PolicyResult {
            text: after_text,
            violations: vec![],
            edits: (2usize..=70)
                .map(|line| Edit {
                    policy: "clang_format".into(),
                    line,
                    before: format!("{line}"),
                    after: format!("{line} "),
                })
                .collect(),
            warnings: vec![],
            changed: true,
        };
        let suppressed =
            PolicyPipeline::apply_line_suppression(before.as_str(), result, &BTreeSet::from([3usize]));
        assert!(!suppressed.warnings.iter().any(|w| w.contains("wide non-local batch")),
            "no threshold-based wide batch revert should occur");
    }

    #[test]
    fn suppression_skips_escalation() {
        let before_lines: Vec<String> = (1..=70).map(|n| format!("{n}")).collect();
        let before = format!("{}\n", before_lines.join("\n"));
        let mut after_lines = before_lines.clone();
        after_lines.insert(2, "X".to_string());
        let after_text = format!("{}\n", after_lines.join("\n"));
        let result = PolicyResult {
            text: after_text,
            violations: vec![],
            edits: (2usize..=70)
                .map(|line| Edit {
                    policy: "clang_format".into(),
                    line,
                    before: format!("{line}"),
                    after: format!("{line} "),
                })
                .collect(),
            warnings: vec![],
            changed: true,
        };
        let suppressed = PolicyPipeline::suppress_structural(
            before.as_str(),
            result,
            &BTreeSet::from([3usize]),
        );
        // Structural-safe must NOT trigger "wide non-local batch" escalation.
        // It may still fall back to best-effort or rollback via the normal
        // non-local path, but NOT the early wide-batch guard.
        assert!(!suppressed
            .warnings
            .iter()
            .any(|warning| warning.contains("wide non-local batch")));
    }

    #[test]
    fn prefers_lower_risk() {
        let existing = PolicyEditCandidate {
            policy: "naming_conventions".into(),
            line: 42,
            confidence: 0.86,
            risk_tier: CandidateRiskTier::High,
            impact_radius: 3,
            symbol_footprint: vec![11u64].into(),
            range_footprint: vec![(40usize, 44usize)].into(),
            hard_constraints_touched: SemanticInvariantClause::EditSafety.bit(),
            zone: PolicyZone::Code,
            after_fingerprint: 1,
            style_gain: 1.0,
        };
        let incoming = PolicyEditCandidate {
            policy: "compact_declarations".into(),
            line: 42,
            confidence: 0.84,
            risk_tier: CandidateRiskTier::Low,
            impact_radius: 1,
            symbol_footprint: vec![11u64].into(),
            range_footprint: vec![(42usize, 42usize)].into(),
            hard_constraints_touched: 0,
            zone: PolicyZone::Code,
            after_fingerprint: 2,
            style_gain: 1.2,
        };
        let adaptive = CertaintyFilterState::new();
        let result = GlobalConflictSolver::solve(
            &[incoming],
            &[existing],
            RetryScopeStage::LineLocal,
            &adaptive,
        );
        assert!(result.accepted.is_empty());
        assert!(result.dropped_lines.contains(&42));
    }

    #[test]
    fn stabilize_reverts_untracked() {
        let mut warnings = Vec::new();
        let output = PolicyPipeline::stabilize_output_text(
            "int value = 0;\n",
            Arc::from("int m_value = 0;\n"),
            &[],
            &mut warnings,
        );
        assert_eq!(output, "int value = 0;\n");
        assert!(warnings
            .iter()
            .any(|item| item.contains("reverted untracked text delta")));
    }

    #[test]
    fn stabilize_keeps_delta() {
        let mut warnings = Vec::new();
        let edits = vec![Edit {
            policy: "compact_declarations".into(),
            line: 1,
            before: "int value = 0;".to_string(),
            after: "int value= 0;".to_string(),
        }];
        let output = PolicyPipeline::stabilize_output_text(
            "int value = 0;\n",
            Arc::from("int value= 0;\n"),
            edits.as_slice(),
            &mut warnings,
        );
        assert_eq!(output, "int value= 0;\n");
        assert!(warnings.is_empty());
    }

    #[test]
    fn coverage_expands_block() {
        let result = PolicyResult {
            text: "line1\nline2 changed\nline3 changed\n".to_string(),
            violations: Vec::new(),
            edits: vec![Edit {
                policy: "pragma_once_spacing".into(),
                line: 2,
                before: "block".to_string(),
                after: "block_changed".to_string(),
            }],
            warnings: Vec::new(),
            changed: true,
        };
        let normalized = PolicyPipeline::normalize_edit_coverage(
            "pragma_once_spacing",
            "line1\nline2\nline3\n",
            result,
        );
        assert_eq!(normalized.edits.len(), 2);
        assert!(normalized.warnings.iter().any(|item| {
            item.contains("normalized edit coverage") && item.contains("pragma_once_spacing")
        }));
    }

    #[test]
    fn coverage_clears_stale() {
        let result = PolicyResult {
            text: "line1\nline2\n".to_string(),
            violations: Vec::new(),
            edits: vec![Edit {
                policy: "include_order".into(),
                line: 1,
                before: "line1".to_string(),
                after: "line1 changed".to_string(),
            }],
            warnings: Vec::new(),
            changed: true,
        };
        let normalized =
            PolicyPipeline::normalize_edit_coverage("include_order", "line1\nline2\n", result);
        assert!(normalized.edits.is_empty());
        assert!(normalized
            .warnings
            .iter()
            .any(|item| item.contains("cleared stale edit records")));
    }

    #[test]
    fn coverage_keeps_multiline() {
        let result = PolicyResult {
            text: "line1\ncall(\n    first,\n    second\n);\nline3\n".to_string(),
            violations: Vec::new(),
            edits: vec![
                Edit {
                    policy: "class_layout".into(),
                    line: 2,
                    before: "call(first, second);\n".to_string(),
                    after: "call(\n".to_string(),
                },
                Edit {
                    policy: "class_layout".into(),
                    line: 3,
                    before: String::new(),
                    after: "    first,\n".to_string(),
                },
                Edit {
                    policy: "class_layout".into(),
                    line: 4,
                    before: String::new(),
                    after: "    second\n".to_string(),
                },
                Edit {
                    policy: "class_layout".into(),
                    line: 5,
                    before: String::new(),
                    after: ");\n".to_string(),
                },
            ],
            warnings: Vec::new(),
            changed: true,
        };
        let normalized = PolicyPipeline::normalize_edit_coverage(
            "class_layout",
            "line1\ncall(first, second);\nline3\n",
            result,
        );
        assert_eq!(normalized.edits.len(), 4);
        assert!(normalized
            .warnings
            .iter()
            .all(|item| !item.contains("normalized edit coverage")));
    }
}
