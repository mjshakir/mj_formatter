pub(super) mod checkpoint;

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use tree_sitter::StreamingIterator;
use tracing::{debug, warn};

use crate::config::confidence_config::ConfidenceConfig;
use crate::config::enums::Enforcement;
use crate::config::policy_config::PolicyConfig;
use crate::engine::confidence_context::ConfidenceContext;
use crate::engine::gate_decision::{ConfidenceGateDecision, ConfidenceReasonCode};
use crate::engine::convergence::ConvergenceController;
use crate::engine::convergence::ConvergencePolicyProfile;
use crate::engine::convergence::ConvergencePolicySignal;
use crate::engine::convergence::ConvergenceRiskTier;
use crate::engine::edit_guard::EditGuard;
use crate::engine::conflict_solver::GlobalConflictSolver;
use crate::engine::catalog::PolicyCapabilities;
use crate::engine::catalog::PolicyCapabilityMatrix;
use crate::engine::catalog::PolicyCertainty;
use crate::engine::catalog::policy_catalog;
use crate::engine::conflict_detector::PolicyConflictDetector;
use crate::engine::edit_candidate::PolicyDecisionOutcome;
use crate::engine::edit_candidate::PolicyEditCandidate;
use crate::engine::run_options::PolicyRunOptions;
use crate::engine::suppression::PolicySuppression;
use crate::engine::zone::PolicyZone;
use crate::engine::proposer::ProposerController;
use crate::engine::semantic_contract::PolicyGuidanceMode;
use crate::engine::semantic_contract::SemanticContract;
use crate::engine::semantic_contract::SemanticInvariantClause;
use crate::model::edit::Edit;
use crate::model::pass_result::{FormatPassMetrics, FormatPassResult};
use crate::model::policy_context::{ParserTrust, PolicyContext};
use crate::model::exec_trace::{
    PolicyCandidateOutcome, PolicyCandidateTrace, PolicyExecutionTrace,
};
use crate::model::policy_result::PolicyResult;
use crate::model::context_query::SemanticContextQuery;
use crate::model::violation::Violation;
use crate::parser::clang_result::ClangParseResult;
use crate::parser::manager::ParserManager;
use crate::parser::query_cache::TsQueryCache;
use crate::parser::manager::SemanticCompdbContextKind;
use crate::parser::file_context::SemanticFileContext;
use crate::parser::file_context::SemanticSummary;
use crate::policy::traits::Policy;
use crate::runtime::adaptive_telemetry::AdaptiveTelemetry;
use crate::runtime::cluster_telemetry::ClusterEnforcementBias;
use crate::runtime::cluster_telemetry::PolicyClusterTelemetry;
use crate::runtime::telemetry::{
    PolicyConfidenceSample, PolicyExecutionSample, PolicyTelemetry,
};
use crate::text_scan;
use tree_sitter::Tree;
use crate::engine::filter_store::CertaintyFilterStore;
use crate::engine::context_tracker::{
    BlockContextKind, FileContextKind, PolicyContextTracker, PolicyOutcomeRecord,
};
use crate::parser::semantic_region::{SemanticRegion, SemanticRegionKind};
use crate::parser::ts_traversal;

const CONVERGENCE_MAX_IMPACT_RANGES_PER_LINE: usize = 6;
type SemanticImpactRangesByLine = BTreeMap<usize, Vec<(usize, usize)>>;
type SemanticImpactSymbolsByLine = BTreeMap<usize, Vec<u64>>;

pub struct PolicyPipeline {
    policies: Vec<Box<dyn Policy>>,
    parser_manager: ParserManager,
    policy_settings: HashMap<String, PolicyConfig>,
    convergence_profiles: Arc<HashMap<String, ConvergencePolicyProfile>>,
    confidence_enabled: bool,
    confidence_default_enforcement: Enforcement,
    conflict_detection_enabled: bool,
    conflict_touch_threshold: usize,
    certainty_filter_store: CertaintyFilterStore,
    context_tracker: std::sync::Mutex<PolicyContextTracker>,
    query_cache: TsQueryCache,
}


pub(super) struct PolicyPipelineRunState<'a> {
    path: &'a Path,
    options: &'a PolicyRunOptions,
    current: Arc<str>,
    all_violations: Vec<Violation>,
    all_edits: Vec<Edit>,
    all_warnings: Vec<String>,
    policy_traces: Vec<PolicyExecutionTrace>,
    tree_for_text: Option<Tree>,
    clang_for_text: Option<Arc<ClangParseResult>>,
    semantic_for_text: Option<SemanticFileContext>,
    comment_lines_for_text: Option<BTreeSet<usize>>,
    semantic_summary: Option<SemanticSummary>,
    internal_candidates: Vec<PolicyEditCandidate>,
    selected_candidates: Vec<PolicyEditCandidate>,
    convergence_controller: ConvergenceController,
    telemetry_samples: Vec<PolicyExecutionSample>,
    conflict_detector: PolicyConflictDetector,
    proposer_controller: ProposerController,
    exact_compdb_for_file: bool,
    semantic_context_kind: SemanticCompdbContextKind,
    semantic_compdb_context_for_file: bool,
    semantic_fidelity_score: f64,
    cached_certainty: Option<PolicyCertainty>,
    content_hash: u64,
    clang_structural_edits_since_parse: usize,
    file_context_kind: FileContextKind,
    context_modifiers: [f32; 24],
    block_context_modifiers: [[f32; 24]; 6],
    pending_context_outcomes: Vec<PolicyOutcomeRecord>,
}

impl<'a> PolicyPipelineRunState<'a> {
    fn semantic_summary_or_default(&self) -> SemanticSummary {
        self.semantic_summary.unwrap_or_default()
    }

    fn invalidate_semantic_state(&mut self, is_semantic_rewrite: bool) {
        self.tree_for_text = None;
        if is_semantic_rewrite {
            self.clang_for_text = None;
            self.clang_structural_edits_since_parse = 0;
        } else {
            self.clang_structural_edits_since_parse += 1;
            let reobs_trust = self
                .cached_certainty
                .map(|c| c.trust_for_general())
                .unwrap_or(crate::engine::fuzzy_inference::DEFAULT_TRUST);
            let reobs_interval =
                crate::engine::fuzzy_inference::fuzzy_kalman_reobservation_interval(reobs_trust);
            if self.clang_structural_edits_since_parse.is_multiple_of(reobs_interval) {
                self.cached_certainty = None;
            }
        }
        self.semantic_for_text = None;
        self.comment_lines_for_text = None;
        self.semantic_summary = None;
    }
}

#[derive(Clone, Copy)]
struct PreparedPolicyStage {
    capability: PolicyCapabilities,
    policy_certainty: PolicyCertainty,
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
    semantic_fidelity_score: f64,
    previous_contract_failures: &'a BTreeSet<SemanticInvariantClause>,
    capability: &'a PolicyCapabilities,
    cluster_radius_cap: Option<usize>,
    trust: f64,
}

struct CommitPolicyInput<'a> {
    policy_name: &'a str,
    policy_started: Instant,
    coordinated: CoordinatedPolicyStage,
    is_semantic_rewrite: bool,
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
    policy_certainty: PolicyCertainty,
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
    Rollback { reason: String },
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
        policy_settings: HashMap<String, PolicyConfig>,
        confidence_config: ConfidenceConfig,
        conflict_detection_enabled: bool,
        conflict_touch_threshold: usize,
        population_context: Option<crate::engine::population_context::PopulationContext>,
    ) -> Self {
        let convergence_profiles = Arc::new(Self::build_convergence_profiles(&policy_settings));
        let certainty_filter_store = CertaintyFilterStore::new(population_context.as_ref());
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
            certainty_filter_store,
            context_tracker: std::sync::Mutex::new(PolicyContextTracker::new()),
            query_cache,
        }
    }

    pub fn set_context_tracker(&self, tracker: PolicyContextTracker) {
        if let Ok(mut guard) = self.context_tracker.lock() {
            *guard = tracker;
        }
    }

    fn context_modifier_for_policy(&self, state: &PolicyPipelineRunState<'_>, policy_name: &str, block_kind: Option<BlockContextKind>) -> f64 {
        let policy_idx = crate::engine::context_tracker::policy_index(policy_name)
            .unwrap_or(u8::MAX);
        if (policy_idx as usize) < 22 {
            let file_mod = state.context_modifiers[policy_idx as usize] as f64;
            let block_mod = match block_kind {
                Some(bk) => state.block_context_modifiers[bk as usize][policy_idx as usize] as f64,
                None => 1.0,
            };
            file_mod * block_mod
        } else {
            1.0
        }
    }

    pub fn save_context_tracker(&self, path: &Path) -> anyhow::Result<()> {
        let guard = self.context_tracker.lock().map_err(|e| anyhow!("lock: {}", e))?;
        guard.save_to_path(path)
    }

    pub fn save_certainty_state(&self, path: &Path) -> anyhow::Result<()> {
        self.certainty_filter_store.save_to_path(path)
    }

    pub fn record_edit_outcome(&self, path: &Path, outcome: f64) {
        self.certainty_filter_store.record_edit_outcome(&path.to_string_lossy(), outcome);
    }

    pub fn correlate_paired_certainty(&self, path: &Path, estimates: [f64; crate::engine::certainty_filter::NUM_DIMS]) {
        use crate::files::file_unit::FileUnitKind;
        let trust = estimates[0].clamp(0.0, 1.0);
        let damping = crate::engine::fuzzy_inference::fuzzy_damping_factor(trust);
        let companions = FileUnitKind::paired_companion_paths_on_disk(path);
        for companion in companions {
            let key = companion.to_string_lossy();
            self.certainty_filter_store
                .correlate_paired_observation(&key, estimates, damping);
        }
    }

    pub fn run_with_options(
        &self,
        text: &str,
        path: &Path,
        options: &PolicyRunOptions,
    ) -> Result<FormatPassResult> {
        let mut state = self.initialize_run_state(text, path, options);
        // Eagerly compute certainty before the policy loop so that
        // fuzzy_execution_level() and all downstream gates
        // receive real Kalman state instead of None/defaults.
        if let Some(first_policy) = self.policies.first() {
            let boot = Instant::now();
            self.ensure_policy_parse_stage(&mut state, first_policy.as_ref(), first_policy.name(), boot)?;
        }
        if state.cached_certainty.is_none() {
            let confidence = ConfidenceContext::from_parsers_and_semantic(
                state.tree_for_text.as_ref(),
                state.clang_for_text.as_deref(),
                state.semantic_for_text.as_ref(),
            );
            let measurement = Self::extract_raw_observation(
                &confidence,
                state.semantic_summary,
                state.semantic_compdb_context_for_file,
                state.clang_structural_edits_since_parse,
            );
            let certainty = self.apply_certainty_filter(
                state.path,
                measurement,
                state.content_hash,
            );
            state.cached_certainty = Some(certainty);
            state.semantic_fidelity_score = crate::engine::fuzzy_inference::fuzzy_semantic_fidelity(
                state.semantic_context_kind,
                Some(&certainty),
            );
        }
        self.record_initial_fidelity_warnings(&mut state);
        for policy in &self.policies {
            self.run_policy_stage(&mut state, policy.as_ref())?;
        }

        self.flush_context_outcomes(&state);
        PolicyTelemetry::record_batch(state.telemetry_samples);
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
            },
            convergence_pairs: finalized.convergence_pairs,
            policy_traces: state.policy_traces,
            accuracy_gate: None,
            metrics: FormatPassMetrics::default(),
            policy_certainty: state.cached_certainty,
        })
    }

    fn initialize_run_state<'a>(
        &self,
        text: &str,
        path: &'a Path,
        options: &'a PolicyRunOptions,
    ) -> PolicyPipelineRunState<'a> {
        let convergence_controller =
            if self.convergence_profiles.is_empty() {
                ConvergenceController::new()
            } else {
                ConvergenceController::with_profiles(
                    self.convergence_profiles.clone(),
                )
            };
        let exact_compdb_for_file = self.parser_manager.has_exact_compdb_entry_for_path(path);
        let semantic_context_kind = self
            .parser_manager
            .semantic_compdb_context_kind_for_path(path);
        let semantic_compdb_context_for_file = self
            .parser_manager
            .has_semantic_compdb_context_for_path(path);
        PolicyPipelineRunState {
            path,
            options,
            current: Arc::from(text),
            all_violations: Vec::new(),
            all_edits: Vec::new(),
            all_warnings: Vec::new(),
            policy_traces: Vec::with_capacity(self.policies.len()),
            tree_for_text: None,
            clang_for_text: None,
            semantic_for_text: None,
            comment_lines_for_text: None,
            semantic_summary: None,
            internal_candidates: Vec::new(),
            selected_candidates: Vec::new(),
            convergence_controller,
            telemetry_samples: Vec::with_capacity(self.policies.len()),
            conflict_detector: PolicyConflictDetector::new(
                self.conflict_detection_enabled,
                self.conflict_touch_threshold,
            ),
            proposer_controller: ProposerController::new(),
            exact_compdb_for_file,
            semantic_context_kind,
            semantic_compdb_context_for_file,
            semantic_fidelity_score: crate::engine::fuzzy_inference::fuzzy_semantic_fidelity(
                semantic_context_kind,
                None,
            ),
            cached_certainty: None,
            content_hash: crc32fast::hash(text.as_bytes()) as u64,
            clang_structural_edits_since_parse: 0,
            file_context_kind: FileContextKind::from_path(path),
            context_modifiers: self.batch_context_modifiers(path),
            block_context_modifiers: self.batch_block_context_modifiers(),
            pending_context_outcomes: Vec::new(),
        }
    }

    fn flush_context_outcomes(&self, state: &PolicyPipelineRunState<'_>) {
        if state.pending_context_outcomes.is_empty() {
            return;
        }
        if let Ok(mut guard) = self.context_tracker.lock() {
            guard.batch_observe_outcomes(state.file_context_kind, &state.pending_context_outcomes);
        }
    }

    fn dominant_block_kind_for_edits(
        state: &PolicyPipelineRunState<'_>,
        edits: &[Edit],
    ) -> BlockContextKind {
        if edits.is_empty() {
            return BlockContextKind::Global;
        }
        let semantic = match &state.semantic_for_text {
            Some(ctx) => ctx,
            None => return BlockContextKind::Global,
        };
        let mut counts = [0u32; 6];
        for edit in edits {
            if edit.line == 0 {
                counts[BlockContextKind::Global as usize] += 1;
                continue;
            }
            let kind = semantic
                .scope_at_location(crate::parser::file_context::SourceLocation::new(edit.line, 1))
                .map(|scope| BlockContextKind::from_scope_kind(scope.kind))
                .unwrap_or(BlockContextKind::Global);
            counts[kind as usize] += 1;
        }
        let max_idx = counts
            .iter()
            .enumerate()
            .max_by_key(|(_, &c)| c)
            .map(|(i, _)| i)
            .unwrap_or(4);
        match max_idx {
            0 => BlockContextKind::Namespace,
            1 => BlockContextKind::Type,
            2 => BlockContextKind::Function,
            3 => BlockContextKind::Preprocessor,
            5 => BlockContextKind::Template,
            _ => BlockContextKind::Global,
        }
    }

    fn batch_context_modifiers(&self, path: &Path) -> [f32; 24] {
        let file_kind = FileContextKind::from_path(path);
        match self.context_tracker.lock() {
            Ok(guard) => guard.batch_file_modifiers(file_kind),
            Err(_) => [1.0f32; 24],
        }
    }

    fn batch_block_context_modifiers(&self) -> [[f32; 24]; 6] {
        match self.context_tracker.lock() {
            Ok(guard) => {
                let mut result = [[1.0f32; 24]; 6];
                result[0] = guard.batch_block_modifiers(BlockContextKind::Namespace);
                result[1] = guard.batch_block_modifiers(BlockContextKind::Type);
                result[2] = guard.batch_block_modifiers(BlockContextKind::Function);
                result[3] = guard.batch_block_modifiers(BlockContextKind::Preprocessor);
                result[4] = guard.batch_block_modifiers(BlockContextKind::Global);
                result[5] = guard.batch_block_modifiers(BlockContextKind::Template);
                result
            }
            Err(_) => [[1.0f32; 24]; 6],
        }
    }

    fn record_initial_fidelity_warnings(&self, state: &mut PolicyPipelineRunState<'_>) {
        state.all_warnings.push(format!(
            "semantic fidelity: {:.2} for {}",
            state.semantic_fidelity_score,
            state.path.display()
        ));
        if state.exact_compdb_for_file {
            return;
        }
        let detail = match state.semantic_context_kind {
            SemanticCompdbContextKind::PairedSourceHeuristic => {
                "using paired-source heuristic semantic context"
            }
            SemanticCompdbContextKind::HeaderConsensus => "using multi-TU header consensus context",
            SemanticCompdbContextKind::SourceConsensus => {
                "using compdb-derived source consensus context"
            }
            SemanticCompdbContextKind::Exact | SemanticCompdbContextKind::None => {
                "using compdb-derived semantic context"
            }
        };
        state.all_warnings.push(format!(
            "semantic fidelity lock: no exact compile_commands entry for {}; {}",
            state.path.display(),
            detail
        ));
    }

    fn run_policy_stage(
        &self,
        state: &mut PolicyPipelineRunState<'_>,
        policy: &dyn Policy,
    ) -> Result<()> {
        let policy_name = policy.name();
        let policy_started = Instant::now();
        if state.options.is_policy_blocked(policy_name) {
            state
                .all_warnings
                .push(format!("retry guard skipped policy '{}'", policy_name));
            state.telemetry_samples.push(PolicyExecutionSample::blocked(
                policy_name,
                policy_started.elapsed(),
            ));
            return Ok(());
        }

        self.ensure_policy_parse_stage(state, policy, policy_name, policy_started)?;
        let Some(prepared) = self.prepare_policy_stage(state, policy_name, policy_started) else {
            return Ok(());
        };
        let is_semantic_rewrite = prepared.capability.semantic_rewrite;
        let is_whitespace_safe = prepared.capability.whitespace_safe;
        let executed = self.execute_policy_stage(state, policy, prepared, policy_started)?;
        if executed.result.edits.is_empty()
            && executed.result.violations.is_empty()
            && text_scan::TEXT_SCAN.strings_equal(&executed.result.text, &state.current)
        {
            state.telemetry_samples.push(PolicyExecutionSample::success(
                policy_name,
                policy_started.elapsed(),
                0,
                0,
            ));
            return Ok(());
        }
        let coordinated = self.coordinate_policy_stage(state, policy_name, executed);
        let policy_idx = crate::engine::context_tracker::policy_index(policy_name);
        if is_whitespace_safe && coordinated.text_changed {
            let block_kind = Self::dominant_block_kind_for_edits(state, &coordinated.result.edits);
            self.commit_policy_stage(state, CommitPolicyInput {
                policy_name, policy_started, coordinated, is_semantic_rewrite,
            });
            if let Some(idx) = policy_idx {
                state.pending_context_outcomes.push(PolicyOutcomeRecord {
                    policy_index: idx,
                    block_kind,
                    success: true,
                });
            }
            return Ok(());
        }
        match self.checkpoint_policy_stage(state, &coordinated, policy_name) {
            PolicyCheckpointResult::Accept { validated_tree } => {
                let block_kind = Self::dominant_block_kind_for_edits(state, &coordinated.result.edits);
                self.commit_policy_stage(state, CommitPolicyInput {
                    policy_name, policy_started, coordinated, is_semantic_rewrite,
                });
                if let Some(idx) = policy_idx {
                    state.pending_context_outcomes.push(PolicyOutcomeRecord {
                        policy_index: idx,
                        block_kind,
                        success: true,
                    });
                }
                if let Some(tree) = validated_tree {
                    state.tree_for_text = Some(tree);
                }
            }
            PolicyCheckpointResult::PartialRollback {
                recovered_text,
                recovered_edits,
                validated_tree,
                warning,
            } => {
                let block_kind = Self::dominant_block_kind_for_edits(state, &recovered_edits);
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
                    candidate_trace: Vec::new(),
                });
                state.telemetry_samples.push(PolicyExecutionSample::success(
                    policy_name,
                    policy_started.elapsed(),
                    edit_count,
                    0,
                ));
                state.current = Arc::from(recovered_text.as_str());
                state.all_edits.extend(recovered_edits);
                if let Some(idx) = policy_idx {
                    state.pending_context_outcomes.push(PolicyOutcomeRecord {
                        policy_index: idx,
                        block_kind,
                        success: true,
                    });
                }
                if let Some(tree) = validated_tree {
                    state.tree_for_text = Some(tree);
                }
                if is_semantic_rewrite {
                    state.invalidate_semantic_state(true);
                }
            }
            PolicyCheckpointResult::Rollback { reason } => {
                state.all_warnings.push(reason);
                if let Some(idx) = policy_idx {
                    state.pending_context_outcomes.push(PolicyOutcomeRecord {
                        policy_index: idx,
                        block_kind: BlockContextKind::Global,
                        success: false,
                    });
                }
                state.telemetry_samples.push(PolicyExecutionSample::success(
                    policy_name,
                    policy_started.elapsed(),
                    0,
                    0,
                ));
            }
        }
        Ok(())
    }

    fn ensure_policy_parse_stage(
        &self,
        state: &mut PolicyPipelineRunState<'_>,
        _policy: &dyn Policy,
        policy_name: &str,
        policy_started: Instant,
    ) -> Result<()> {
        // Tree-sitter is always needed (all policies use structural parse context).
        // Clang is parsed eagerly when compdb context exists — all policies benefit from
        // fused clang+tree-sitter context (declarations, references, diagnostics).
        let needs_tree = state.tree_for_text.is_none();
        let needs_clang = state.clang_for_text.is_none()
            && state.semantic_compdb_context_for_file;

        if needs_tree && needs_clang {
            // Dispatch clang first (non-blocking), then parse tree-sitter (fast, local),
            // then collect clang result with bounded deadline.
            let clang_handle = self
                .parser_manager
                .dispatch_clang(&state.current, state.path);
            let tree_result = self
                .parser_manager
                .parse_tree_sitter(&state.current, state.path);
            match tree_result {
                Ok(value) => {
                    state.tree_for_text = Some(value);
                }
                Err(err) => {
                    state.telemetry_samples.push(PolicyExecutionSample::failed(
                        policy_name,
                        policy_started.elapsed(),
                        false,
                    ));
                    PolicyTelemetry::record_batch(state.telemetry_samples.clone());
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
                    state.clang_for_text = Some(value);
                    state.clang_structural_edits_since_parse = 0;
                }
                Err(err) => {
                    state.telemetry_samples.push(PolicyExecutionSample::failed(
                        policy_name,
                        policy_started.elapsed(),
                        false,
                    ));
                    PolicyTelemetry::record_batch(state.telemetry_samples.clone());
                    warn!(policy = policy_name, error = %err, "clang parse failed");
                    return Err(anyhow!("{policy_name}: {err}"));
                }
            }
        } else if needs_tree {
            let parsed = match self
                .parser_manager
                .parse_tree_sitter(&state.current, state.path)
            {
                Ok(value) => value,
                Err(err) => {
                    state.telemetry_samples.push(PolicyExecutionSample::failed(
                        policy_name,
                        policy_started.elapsed(),
                        false,
                    ));
                    PolicyTelemetry::record_batch(state.telemetry_samples.clone());
                    warn!(policy = policy_name, error = %err, "tree-sitter parse failed");
                    return Err(anyhow!("{policy_name}: {err}"));
                }
            };
            state.tree_for_text = Some(parsed);
        } else if needs_clang {
            let parsed = match self
                .parser_manager
                .parse_clang(&state.current, state.path)
            {
                Ok(value) => value,
                Err(err) => {
                    state.telemetry_samples.push(PolicyExecutionSample::failed(
                        policy_name,
                        policy_started.elapsed(),
                        false,
                    ));
                    PolicyTelemetry::record_batch(state.telemetry_samples.clone());
                    warn!(policy = policy_name, error = %err, "clang parse failed");
                    return Err(anyhow!("{policy_name}: {err}"));
                }
            };
            state.clang_for_text = Some(parsed);
            state.clang_structural_edits_since_parse = 0;
        }
        if needs_clang && state.semantic_for_text.is_some() {
            state.semantic_for_text = None;
            state.semantic_summary = None;
            state.cached_certainty = None;
        }
        if state.semantic_for_text.is_none() {
            let semantic = SemanticFileContext::from_parses_with_cache(
                &state.current,
                state.path,
                state.tree_for_text.as_ref(),
                state.clang_for_text.as_deref(),
                Some(&self.query_cache),
            );
            state.semantic_summary = Some(semantic.summary());
            state.semantic_for_text = Some(semantic);
        }
        if state.comment_lines_for_text.is_none() {
            state.comment_lines_for_text =
                Some(Self::comment_lines_from_tree(state.tree_for_text.as_ref(), &self.query_cache));
        }
        Ok(())
    }

    fn prepare_policy_stage(
        &self,
        state: &mut PolicyPipelineRunState<'_>,
        policy_name: &str,
        _policy_started: Instant,
    ) -> Option<PreparedPolicyStage> {
        let capability = PolicyCapabilityMatrix::for_policy(policy_name);
        let policy_certainty = if let Some(cached) = state.cached_certainty {
            cached
        } else {
            let confidence = ConfidenceContext::from_parsers_and_semantic(
                state.tree_for_text.as_ref(),
                state.clang_for_text.as_deref(),
                state.semantic_for_text.as_ref(),
            );
            let measurement = Self::extract_raw_observation(
                &confidence,
                state.semantic_summary,
                state.semantic_compdb_context_for_file,
                state.clang_structural_edits_since_parse,
            );
            let filtered = self.apply_certainty_filter(
                state.path,
                measurement,
                state.content_hash,
            );
            state.cached_certainty = Some(filtered);
            state.semantic_fidelity_score = crate::engine::fuzzy_inference::fuzzy_semantic_fidelity(
                state.semantic_context_kind,
                Some(&filtered),
            );
            filtered
        };
        let guidance_mode = self
            .policy_settings
            .get(policy_name)
            .map(|settings| SemanticContract::policy_guidance_mode(policy_name, settings))
            .unwrap_or(PolicyGuidanceMode::SoftGuideline);
        Some(PreparedPolicyStage {
            capability,
            policy_certainty,
            guidance_mode,
        })
    }

    fn execute_policy_stage(
        &self,
        state: &mut PolicyPipelineRunState<'_>,
        policy: &dyn Policy,
        prepared: PreparedPolicyStage,
        policy_started: Instant,
    ) -> Result<ExecutedPolicyStage> {
        let policy_name = policy.name();
        let parser_trust = ParserTrust {
            semantic_rewrite: prepared.policy_certainty.trust_for_semantic_rewrite(),
        };
        let context = PolicyContext::new(&state.current, state.path)
            .with_tree_sitter_tree(state.tree_for_text.as_ref())
            .with_clang_parse_result(state.clang_for_text.as_deref())
            .with_semantic_file_context(state.semantic_for_text.as_ref())
            .with_project_graph_snapshot(state.options.project_graph_snapshot.as_deref())
            .with_parser_trust(parser_trust)
            .with_query_cache(Some(&self.query_cache));
        let semantic_query = context.semantic_query();
        let project_query = context.project_query();
        let mut result = policy.apply(&context);
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
            state.telemetry_samples.push(PolicyExecutionSample::failed(
                policy_name,
                policy_started.elapsed(),
                true,
            ));
            PolicyTelemetry::record_batch(state.telemetry_samples.clone());
            warn!(policy = policy_name, reason = %fatal, "policy produced fatal warning");
            return Err(anyhow!("{policy_name}: {fatal}"));
        }
        result = Self::apply_semantic_guidance_mode(
            &state.current,
            result,
            &semantic_query,
            &prepared.capability,
            SemanticGuidanceConfig {
                policy_name,
                guidance_mode: prepared.guidance_mode,
                exact_compdb_for_file: state.exact_compdb_for_file,
                semantic_context_kind: state.semantic_context_kind,
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
                let regions = state
                    .semantic_for_text
                    .as_ref()
                    .map(|s| s.regions.as_slice())
                    .unwrap_or(&[]);
                let mut exc = Self::semantic_error_lines(regions);
                if let Some(tree) = state.tree_for_text.as_ref() {
                    exc.extend(ts_traversal::tree_error_stats(tree).error_lines);
                }
                exc
            } else if prepared.capability.structural_safe {
                // Structural-safe policies cannot break code structure by definition.
                // Per-policy checkpoint (tree-sitter re-parse) already validates after each edit.
                // Bypassing the understanding gate here allows edits that FIX parse errors
                // (e.g., TAB→space, alternative tokens) instead of blocking them.
                BTreeSet::new()
            } else {
                // Other syntactic policies: exclude tree-sitter error lines
                state
                    .tree_for_text
                    .as_ref()
                    .map(|t| ts_traversal::tree_error_stats(t).error_lines)
                    .unwrap_or_default()
            };
            if !excluded.is_empty() {
                let before_count = result.edits.len();
                result.edits = result.edits.iter()
                    .filter(|e| e.line == 0 || !excluded.contains(&e.line))
                    .cloned()
                    .collect();
                let dropped = before_count.saturating_sub(result.edits.len());
                if dropped > 0 {
                    result.warnings.push(format!(
                        "understanding gate: suppressed {} edit(s) on parser-error line(s) for '{}'",
                        dropped, policy_name
                    ));
                    if result.edits.is_empty() {
                        result.text = state.current.to_string();
                    } else {
                        let resynthesized = Self::apply_synthesized_edits_best_effort(
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
        let cluster_controls =
            PolicyClusterTelemetry::adaptive_controls(policy_name, context_cluster);

        let mut confidence_sample = None;
        let mut confidence_outcome = None;
        let mut confidence_score = None;
        let mut confidence_threshold = None;
        let mut dropped_line_count = 0usize;
        if let Some(settings) = self.policy_settings.get(policy_name) {
            let guard_violations = EditGuard::validate(
                policy_name,
                &settings.touch_contract,
                result.edits.as_slice(),
                state.tree_for_text.as_ref(),
                Some(&self.query_cache),
                Some(&prepared.policy_certainty),
                prepared.capability.structural_safe,
            );
            if !guard_violations.is_empty() {
                let mut violations = result.violations;
                violations.extend(guard_violations);
                result = PolicyResult {
                    text: state.current.to_string(),
                    violations,
                    edits: Vec::new(),
                    warnings: result.warnings,
                };
            }
            if self.confidence_enabled && !result.edits.is_empty() {
                let is_semantic = prepared.capability.semantic_rewrite
                    && state.semantic_compdb_context_for_file;
                if is_semantic
                    && state.tree_for_text.is_some()
                    && state.clang_for_text.is_none()
                {
                    result.warnings.push(
                        "policy confidence mode downgraded to 'tree-sitter' due unavailable clang semantic context".to_string(),
                    );
                }
                let score = if is_semantic {
                    prepared.policy_certainty.semantic
                } else {
                    prepared.policy_certainty.structural
                };
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
                let adjusted_decision = Self::apply_cluster_enforcement_bias(
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
            semantic: state.semantic_for_text.as_ref(),
            summary: state.semantic_summary_or_default(),
            semantic_fidelity_score: state.semantic_fidelity_score,
            previous_contract_failures: &state.options.previous_contract_failures,
            capability: &prepared.capability,
            cluster_radius_cap: cluster_controls.max_impact_radius_cap,
            trust: prepared.capability.policy_trust(&prepared.policy_certainty)
                * self.context_modifier_for_policy(state, policy_name, None),
        });

        Ok(ExecutedPolicyStage {
            result,
            capability: prepared.capability,
            policy_certainty: prepared.policy_certainty,
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
        state: &mut PolicyPipelineRunState<'_>,
        policy_name: &str,
        mut executed: ExecutedPolicyStage,
    ) -> CoordinatedPolicyStage {
        let parser_trust = ParserTrust {
            semantic_rewrite: executed.policy_certainty.trust_for_semantic_rewrite(),
        };
        let context = PolicyContext::new(&state.current, state.path)
            .with_tree_sitter_tree(state.tree_for_text.as_ref())
            .with_clang_parse_result(state.clang_for_text.as_deref())
            .with_semantic_file_context(state.semantic_for_text.as_ref())
            .with_project_graph_snapshot(state.options.project_graph_snapshot.as_deref())
            .with_parser_trust(parser_trust)
            .with_query_cache(Some(&self.query_cache));
        let project_query = context.project_query();
        let edit_block_kind = Self::dominant_block_kind_for_edits(state, &executed.result.edits);
        let context_mod = self.context_modifier_for_policy(state, policy_name, Some(edit_block_kind));
        let candidate_confidence = executed.confidence_score.unwrap_or(
            executed
                .capability
                .effective_certainty(&executed.policy_certainty),
        ) * context_mod;
        let proposed_candidates = state.proposer_controller.propose(
            policy_name,
            &executed.result,
            &project_query,
            state.comment_lines_for_text.as_ref(),
            &executed.convergence_signal,
            &executed.capability,
            candidate_confidence,
            &executed.policy_certainty,
        );
        let all_candidates = proposed_candidates.clone();
        let guardian_assessment = ProposerController::assess(
            proposed_candidates.as_slice(), &executed.capability, Some(&executed.policy_certainty));
        let disallowed_zone_lines = guardian_assessment.blocked_zone_lines;
        let guardian_hard_blocked_lines = guardian_assessment.blocked_hard_constraint_lines;
        let mut guardian_suppressed_lines = disallowed_zone_lines.clone();
        guardian_suppressed_lines.extend(guardian_hard_blocked_lines.iter().copied());
        let mut candidates = guardian_assessment.allowed;
        if !guardian_suppressed_lines.is_empty() {
            executed.result = if executed.capability.structural_safe {
                Self::apply_line_suppression_structural(
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
            state.selected_candidates.as_slice(),
            &executed.policy_certainty,
            state.options.retry_scope_stage,
        );
        let mut hard_blocked_lines = guardian_hard_blocked_lines.clone();
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
        executed.convergence_signal.solver_hard_blocked_lines = hard_blocked_lines.len();
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
                };
                candidates.clear();
                executed.result.warnings.push(format!(
                    "global conflict solver: reverted semantic rewrite '{}' because {} conflicting line(s) would cause partial propagation",
                    policy_name,
                    solve_result.dropped_lines.len()
                ));
            } else if policy_catalog()
                .behavior_for_name(policy_name)
                .keeps_non_local_conflict_batch
                && Self::has_non_local_text_shape_change(
                    &state.current,
                    executed.result.text.as_str(),
                )
                && solve_result.dropped_lines.len() <= crate::engine::fuzzy_inference::fuzzy_batch_dropped_cap(
                    executed.policy_certainty.trust_for_general()
                )
            {
                executed.result.warnings.push(format!(
                    "global conflict solver: kept non-local '{}' batch ({} conflicting line(s)) to avoid unsafe partial rollback",
                    policy_name,
                    solve_result.dropped_lines.len(),
                ));
            } else {
                executed.result = if executed.capability.structural_safe {
                    Self::apply_line_suppression_structural(
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
        let accepted_candidates = solve_result
            .accepted
            .iter()
            .filter(|candidate| kept_lines.contains(&candidate.line))
            .cloned()
            .collect::<Vec<_>>();
        let convergence_dropped = solve_result
            .accepted
            .len()
            .saturating_sub(accepted_candidates.len());
        executed.dropped_line_count = executed
            .dropped_line_count
            .saturating_add(convergence_dropped);
        let candidate_trace = Self::build_policy_candidate_trace(
            all_candidates.as_slice(),
            &executed.policy_certainty,
            state.options.retry_scope_stage,
            &disallowed_zone_lines,
            &hard_blocked_lines,
            &solve_result.dropped_lines,
            &kept_lines,
        );
        let conflict_violations = state
            .conflict_detector
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
        state: &mut PolicyPipelineRunState<'_>,
        input: CommitPolicyInput<'_>,
    ) {
        let CommitPolicyInput {
            policy_name,
            policy_started,
            coordinated,
            is_semantic_rewrite,
        } = input;
        Self::append_selected_candidates(
            &mut state.selected_candidates,
            coordinated.accepted_candidates.as_slice(),
        );
        state
            .internal_candidates
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
            candidate_trace: coordinated.candidate_trace,
        });

        let summary = state.semantic_summary_or_default();
        let mut sample = PolicyExecutionSample::success(
            policy_name,
            policy_started.elapsed(),
            coordinated.result.edits.len(),
            coordinated.result.violations.len(),
        );
        if let Some(confidence) = coordinated.confidence_sample {
            sample = sample.with_confidence(confidence);
        }
        state.telemetry_samples.push(sample);
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
            internal_candidate_count = state.internal_candidates.len(),
            "policy applied"
        );
        state.current = Arc::from(coordinated.result.text);
        state.all_violations.extend(coordinated.result.violations);
        state.all_violations.extend(coordinated.conflict_violations);
        state.all_edits.extend(coordinated.result.edits);
        state.all_warnings.extend(coordinated.result.warnings);
        if coordinated.text_changed {
            state.invalidate_semantic_state(is_semantic_rewrite);
        }
    }

    fn stabilize_output_text(
        input_text: &str,
        output_text: Arc<str>,
        edits: &[Edit],
        warnings: &mut Vec<String>,
    ) -> String {
        if edits.is_empty() && !text_scan::TEXT_SCAN.strings_equal(&output_text, input_text) {
            warnings.push(
                "pipeline guard: reverted untracked text delta because no final edits survived"
                    .to_string(),
            );
            return input_text.to_string();
        }
        output_text.to_string()
    }

    fn normalize_edit_coverage(
        policy_name: &str,
        before_text: &str,
        mut result: PolicyResult,
    ) -> PolicyResult {
        if text_scan::TEXT_SCAN.strings_equal(&result.text, before_text) {
            if !result.edits.is_empty() {
                result.edits.clear();
                result.warnings.push(format!(
                    "policy output guard: cleared stale edit records for '{}'",
                    policy_name
                ));
            }
            return result;
        }
        if !result.edits.is_empty()
            && Self::apply_synthesized_edits(before_text, result.edits.as_slice()).as_deref()
                == Some(result.text.as_str())
        {
            return result;
        }

        let synthesized =
            Self::synthesize_line_edits(before_text, result.text.as_str(), policy_name);
        if synthesized.is_empty() {
            return result;
        }
        let declared_lines = Self::edit_lines(result.edits.as_slice());
        let actual_lines = Self::edit_lines(synthesized.as_slice());
        if result.edits.is_empty()
            || !actual_lines
                .iter()
                .all(|line| declared_lines.contains(line))
            || declared_lines.len() != actual_lines.len()
        {
            result.warnings.push(format!(
                "policy output guard: normalized edit coverage for '{}' (declared_lines={}, actual_lines={})",
                policy_name,
                declared_lines.len(),
                actual_lines.len()
            ));
            result.edits = synthesized;
        }
        result
    }

    fn synthesize_line_edits(before: &str, after: &str, policy_name: &str) -> Vec<Edit> {
        let before_lines = text_scan::split_lines_as_slices(before, true);
        let after_lines = text_scan::split_lines_as_slices(after, true);
        let common_len = before_lines.len().min(after_lines.len());
        let mut prefix = 0usize;
        while prefix < common_len
            && text_scan::TEXT_SCAN
                .slices_equal(before_lines[prefix].as_bytes(), after_lines[prefix].as_bytes())
        {
            prefix = prefix.saturating_add(1);
        }

        let mut before_tail = before_lines.len();
        let mut after_tail = after_lines.len();
        while before_tail > prefix
            && after_tail > prefix
            && text_scan::TEXT_SCAN.slices_equal(
                before_lines[before_tail - 1].as_bytes(),
                after_lines[after_tail - 1].as_bytes(),
            )
        {
            before_tail = before_tail.saturating_sub(1);
            after_tail = after_tail.saturating_sub(1);
        }

        let before_diff = &before_lines[prefix..before_tail];
        let after_diff = &after_lines[prefix..after_tail];
        let max_lines = before_diff.len().max(after_diff.len());
        let mut edits = Vec::<Edit>::new();
        for index in 0..max_lines {
            let left = before_diff.get(index).copied().unwrap_or("");
            let right = after_diff.get(index).copied().unwrap_or("");
            if left == right {
                continue;
            }
            edits.push(Edit {
                policy: policy_name.into(),
                line: prefix + index + 1,
                before: left.to_string(),
                after: right.to_string(),
            });
        }
        edits
    }

    fn build_convergence_profiles(
        policy_settings: &HashMap<String, PolicyConfig>,
    ) -> HashMap<String, ConvergencePolicyProfile> {
        let mut profiles = HashMap::new();
        for (name, settings) in policy_settings {
            let mut domain = name.clone();
            let mut priority = ConvergenceController::default_priority_for(name.as_str());
            let mut impact_radius = ConvergenceController::default_impact_radius_for(name.as_str());
            let mut semantic_priority_weight_bp =
                ConvergenceController::default_semantic_priority_weight_bp_for(name.as_str());
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
                    .get("semantic_priority_weight_bp")
                    .and_then(|item| item.as_integer())
                {
                    if value >= 0 {
                        semantic_priority_weight_bp = value.min(1_000) as u16;
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
                    semantic_priority_weight_bp = (value.clamp(0.0, 1.0) * 1_000.0).round() as u16;
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
                    semantic_priority_weight_bp,
                    risk_tier,
                ),
            );
        }
        profiles
    }

    fn build_convergence_signal(input: ConvergenceSignalInput<'_>) -> ConvergencePolicySignal {
        let ConvergenceSignalInput {
            result,
            semantic,
            summary,
            semantic_fidelity_score,
            previous_contract_failures,
            capability,
            cluster_radius_cap,
            trust,
        } = input;
        let mut semantic_confidence_bp =
            crate::engine::fuzzy_inference::fuzzy_semantic_confidence_bonus(0, trust);
        let mut impact_radius = 0usize;
        if let Some(context) = semantic {
            if context.clang_success {
                semantic_confidence_bp = semantic_confidence_bp.saturating_add(
                    crate::engine::fuzzy_inference::fuzzy_semantic_confidence_bonus(1, trust),
                );
            }
            if !context.tree_has_error {
                semantic_confidence_bp = semantic_confidence_bp.saturating_add(
                    crate::engine::fuzzy_inference::fuzzy_semantic_confidence_bonus(2, trust),
                );
            }
            if context.diagnostic_summary.error_total() == 0 {
                semantic_confidence_bp = semantic_confidence_bp.saturating_add(
                    crate::engine::fuzzy_inference::fuzzy_semantic_confidence_bonus(3, trust),
                );
            } else {
                let error_cap = crate::engine::fuzzy_inference::fuzzy_diagnostic_error_cap(trust);
                let penalty = context.diagnostic_summary.error_total().min(error_cap) as u16 * 20;
                semantic_confidence_bp = semantic_confidence_bp.saturating_sub(penalty);
            }
            if summary.usr_backed_declaration_count > 0 {
                semantic_confidence_bp = semantic_confidence_bp.saturating_add(
                    crate::engine::fuzzy_inference::fuzzy_semantic_confidence_bonus(4, trust),
                );
            }
            let ref_radius = crate::engine::fuzzy_inference::fuzzy_reference_count_radius(
                summary.reference_count,
                trust,
            );
            impact_radius = impact_radius.max(ref_radius);
        } else {
            semantic_confidence_bp = semantic_confidence_bp.saturating_sub(
                crate::engine::fuzzy_inference::fuzzy_contract_failure_deduction(0, trust),
            );
        }
        let fidelity_deduction = crate::engine::fuzzy_inference::fuzzy_fidelity_deduction(
            semantic_fidelity_score,
            trust,
        );
        if fidelity_deduction > 0 {
            semantic_confidence_bp = semantic_confidence_bp.saturating_sub(fidelity_deduction);
        }
        if previous_contract_failures.contains(&SemanticInvariantClause::SymbolIdentity)
            || previous_contract_failures
                .contains(&SemanticInvariantClause::DeclarationReferenceIntegrity)
        {
            semantic_confidence_bp = semantic_confidence_bp.saturating_sub(
                crate::engine::fuzzy_inference::fuzzy_contract_failure_deduction(2, trust),
            );
            impact_radius = impact_radius.max(2);
        }
        if previous_contract_failures.contains(&SemanticInvariantClause::ScopeIntegrity) {
            semantic_confidence_bp = semantic_confidence_bp.saturating_sub(
                crate::engine::fuzzy_inference::fuzzy_contract_failure_deduction(3, trust),
            );
            impact_radius = impact_radius.max(2);
        }
        if previous_contract_failures.contains(&SemanticInvariantClause::EditSafety) {
            semantic_confidence_bp = semantic_confidence_bp.saturating_sub(
                crate::engine::fuzzy_inference::fuzzy_contract_failure_deduction(4, trust),
            );
        }
        let edit_radius = crate::engine::fuzzy_inference::fuzzy_edit_count_radius(
            result.edits.len(),
            trust,
        );
        impact_radius = impact_radius.max(edit_radius);
        let mut resolved_radius = impact_radius.min(8);
        if let Some(cap) = cluster_radius_cap {
            resolved_radius = resolved_radius.min(cap.max(1));
        }
        let (semantic_impact_ranges_by_line, semantic_symbol_ids_by_line) =
            Self::build_semantic_impact_maps(result, semantic, resolved_radius, trust);
        ConvergencePolicySignal {
            semantic_confidence_bp: semantic_confidence_bp.min(1_000),
            impact_radius: resolved_radius,
            capability_semantic_rewrite: capability.semantic_rewrite,
            capability_macro_sensitive: capability.macro_sensitive,
            capability_whitespace_safe: capability.whitespace_safe,
            solver_dropped_lines: 0,
            solver_hard_blocked_lines: 0,
            semantic_impact_ranges_by_line,
            semantic_symbol_ids_by_line,
        }
    }

    fn build_semantic_impact_maps(
        result: &PolicyResult,
        semantic: Option<&SemanticFileContext>,
        base_radius: usize,
        trust: f64,
    ) -> (SemanticImpactRangesByLine, SemanticImpactSymbolsByLine) {
        let impact_cap = crate::engine::fuzzy_inference::fuzzy_convergence_impact_cap(trust);
        let scope_cap = crate::engine::fuzzy_inference::fuzzy_convergence_scope_cap(trust);
        let symbol_cap = crate::engine::fuzzy_inference::fuzzy_convergence_symbol_cap(trust);
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

        let mut ranges_by_line = BTreeMap::<usize, Vec<(usize, usize)>>::new();
        let mut symbols_by_line = BTreeMap::<usize, Vec<u64>>::new();
        let Some(semantic) = semantic else {
            for line in edit_lines {
                ranges_by_line.insert(
                    line,
                    vec![(
                        line.saturating_sub(fallback_radius).max(1),
                        line.saturating_add(fallback_radius).max(1),
                    )],
                );
            }
            return (ranges_by_line, symbols_by_line);
        };

        let mut symbol_lines = HashMap::<u64, (usize, usize)>::new();
        let mut symbol_ids_by_line = HashMap::<usize, Vec<u64>>::new();
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

        for line in edit_lines {
            let mut ranges = Vec::<(usize, usize)>::new();
            let mut containing = semantic
                .scopes
                .iter()
                .filter(|scope| line >= scope.start_line && line <= scope.end_line)
                .map(|scope| (scope.start_line.max(1), scope.end_line.max(1)))
                .filter(|(start, end)| start <= end)
                .collect::<Vec<_>>();
            containing.sort_by(|left, right| {
                let left_width = left.1.saturating_sub(left.0);
                let right_width = right.1.saturating_sub(right.0);
                left_width.cmp(&right_width).then(left.0.cmp(&right.0))
            });
            for range in containing {
                let width = range.1.saturating_sub(range.0).saturating_add(1);
                if width > scope_cap {
                    continue;
                }
                ranges.push(range);
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

    fn normalize_impact_ranges(ranges: &mut Vec<(usize, usize)>) {
        if ranges.is_empty() {
            return;
        }
        ranges.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
        let mut merged = Vec::<(usize, usize)>::with_capacity(ranges.len());
        for (start, end) in ranges.iter().copied() {
            let start = start.max(1);
            let end = end.max(start);
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

    fn hash_semantic_stable_id(value: &str) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    fn apply_confidence_decision(
        policy_name: &str,
        before_text: &str,
        result: PolicyResult,
        decision: ConfidenceGateDecision,
    ) -> PolicyResult {
        let message_line = result.edits.first().map(|item| item.line).unwrap_or(1);
        let reason_text = decision.rendered_reason_summary();

        match decision.outcome {
            PolicyDecisionOutcome::Apply => {
                if decision.base_enforcement != decision.effective_enforcement {
                    let mut violations = result.violations;
                    violations.push(Violation {
                        policy: "confidence_guard".into(),
                        message: format!(
                            "Adaptive tier for '{}': {:?}->{:?} ({})",
                            policy_name,
                            decision.base_enforcement,
                            decision.effective_enforcement,
                            reason_text
                        ),
                        line: message_line,
                        column: Some(1),
                    });
                    PolicyResult {
                        text: result.text,
                        violations,
                        edits: result.edits,
                        warnings: result.warnings,
                    }
                } else {
                    result
                }
            }
            PolicyDecisionOutcome::ApplyPartial => {
                let dropped_count = decision.dropped_lines.len();
                let mut suppressed =
                    Self::apply_line_suppression(before_text, result, &decision.dropped_lines);
                suppressed.violations.push(Violation {
                    policy: "confidence_guard".into(),
                    message: format!(
                        "Adaptive partial apply for '{}' (dropped_lines={}, score={:.2}, threshold={:.2}, reasons={})",
                        policy_name, dropped_count, decision.score, decision.threshold, reason_text
                    ),
                    line: message_line,
                    column: Some(1),
                });
                suppressed
            }
            PolicyDecisionOutcome::Block => {
                let mode_label = "blocked";
                let mut violations = result.violations;
                violations.push(Violation {
                    policy: "confidence_guard".into(),
                    message: format!(
                        "Adaptive decision {} for '{}' (score={:.2}, threshold={:.2}, effective={:?}, reasons={})",
                        mode_label,
                        policy_name,
                        decision.score,
                        decision.threshold,
                        decision.effective_enforcement,
                        reason_text
                    ),
                    line: message_line,
                    column: Some(1),
                });
                PolicyResult {
                    text: before_text.to_string(),
                    violations,
                    edits: Vec::new(),
                    warnings: result.warnings,
                }
            }
        }
    }

    fn apply_cluster_enforcement_bias(
        mut decision: ConfidenceGateDecision,
        bias: ClusterEnforcementBias,
    ) -> ConfidenceGateDecision {
        match bias {
            ClusterEnforcementBias::Neutral => decision,
            ClusterEnforcementBias::Relax => {
                if decision.base_enforcement != Enforcement::Must
                    && decision.outcome == PolicyDecisionOutcome::Block
                {
                    decision.outcome = PolicyDecisionOutcome::Apply;
                    Self::push_reason_code(
                        &mut decision.reason_codes,
                        ConfidenceReasonCode::ClusterAdaptiveRelaxed,
                    );
                }
                if decision.effective_enforcement != Enforcement::Must {
                    decision.effective_enforcement =
                        Self::relax_enforcement(decision.effective_enforcement);
                }
                Self::push_reason_code(
                    &mut decision.reason_codes,
                    ConfidenceReasonCode::ClusterAdaptiveRelaxed,
                );
                decision
            }
            ClusterEnforcementBias::Harden => {
                decision.effective_enforcement =
                    Self::harden_enforcement(decision.effective_enforcement);
                Self::push_reason_code(
                    &mut decision.reason_codes,
                    ConfidenceReasonCode::ClusterAdaptiveHardened,
                );
                decision
            }
        }
    }

    fn push_reason_code(codes: &mut Vec<ConfidenceReasonCode>, code: ConfidenceReasonCode) {
        if !codes.contains(&code) {
            codes.push(code);
        }
    }

    fn relax_enforcement(value: Enforcement) -> Enforcement {
        match value {
            Enforcement::Must => Enforcement::Must,
            Enforcement::Hard => Enforcement::Soft,
            Enforcement::Soft => Enforcement::Advisory,
            Enforcement::Advisory => Enforcement::Advisory,
        }
    }

    fn harden_enforcement(value: Enforcement) -> Enforcement {
        match value {
            Enforcement::Must => Enforcement::Must,
            Enforcement::Hard => Enforcement::Must,
            Enforcement::Soft => Enforcement::Hard,
            Enforcement::Advisory => Enforcement::Soft,
        }
    }

    fn apply_semantic_guidance_mode(
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
                        && semantic_query.is_safe_preprocessor_or_global_edit(line, 1))
                        || (capability.allows_zone(PolicyZone::Comments)
                            && !semantic_query.has_diagnostic_error_on_line(line))
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
                    Self::apply_line_suppression_structural(before_text, result, &unsafe_lines)
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
                }
            }
        }
    }

    fn apply_scope_filter(
        before_text: &str,
        result: PolicyResult,
        allowed_lines: Option<&BTreeSet<usize>>,
        config: ScopeFilterConfig<'_>,
    ) -> PolicyResult {
        let Some(allowed_lines) = allowed_lines else {
            return result;
        };
        if allowed_lines.is_empty() || result.edits.is_empty() {
            return result;
        }
        let blocked_lines = result
            .edits
            .iter()
            .filter_map(|edit| {
                (edit.line > 0 && edit.before != edit.after && !allowed_lines.contains(&edit.line))
                    .then_some(edit.line)
            })
            .collect::<BTreeSet<_>>();
        if blocked_lines.is_empty() {
            return result;
        }
        let dropped = blocked_lines.len();
        if config.capability.semantic_rewrite {
            let mut reverted = result;
            reverted.text = before_text.to_string();
            reverted.edits.clear();
            reverted.warnings.push(format!(
                "retry_scope({scope_stage}) reverted semantic rewrite for '{}' because {} out-of-scope line(s) were required",
                config.policy_name,
                dropped,
                scope_stage = config.scope_stage
            ));
            return reverted;
        }
        let mut filtered = if config.capability.structural_safe {
            Self::apply_line_suppression_structural(before_text, result, &blocked_lines)
        } else {
            Self::apply_line_suppression(before_text, result, &blocked_lines)
        };
        filtered.warnings.push(format!(
            "retry_scope({scope_stage}) dropped {} out-of-scope line(s) for '{}'",
            dropped,
            config.policy_name,
            scope_stage = config.scope_stage
        ));
        filtered
    }

    fn apply_line_suppression(
        before_text: &str,
        result: PolicyResult,
        disabled_lines: &std::collections::BTreeSet<usize>,
    ) -> PolicyResult {
        Self::apply_line_suppression_impl(before_text, result, disabled_lines)
    }

    fn apply_line_suppression_structural(
        before_text: &str,
        result: PolicyResult,
        disabled_lines: &std::collections::BTreeSet<usize>,
    ) -> PolicyResult {
        Self::apply_line_suppression_impl(before_text, result, disabled_lines)
    }

    fn apply_line_suppression_impl(
        before_text: &str,
        result: PolicyResult,
        disabled_lines: &std::collections::BTreeSet<usize>,
    ) -> PolicyResult {
        if disabled_lines.is_empty() {
            return result;
        }
        let PolicyResult {
            text: result_text,
            violations: result_violations,
            edits: result_edits,
            mut warnings,
        } = result;
        let kept_violations = result_violations
            .into_iter()
            .filter(|item| !disabled_lines.contains(&item.line))
            .collect::<Vec<_>>();
        let kept_edits = result_edits
            .into_iter()
            .filter(|item| !disabled_lines.contains(&item.line))
            .collect::<Vec<_>>();

        let before_lines = text_scan::split_lines_as_slices(before_text, true);
        let after_lines = text_scan::split_lines_as_slices(result_text.as_str(), true);
        let before_line_count = before_lines.len();
        let after_line_count = after_lines.len();
        let max_count = before_line_count.min(after_line_count);
        let suppressed_line_touched = disabled_lines.iter().any(|line_no| {
            let index = line_no.saturating_sub(1);
            if index < max_count {
                return !text_scan::TEXT_SCAN.slices_equal(
                    before_lines[index].as_bytes(),
                    after_lines[index].as_bytes(),
                );
            }
            if index < before_line_count {
                return true;
            }
            index < after_line_count
        });
        if !suppressed_line_touched {
            return PolicyResult {
                text: result_text,
                violations: kept_violations,
                edits: kept_edits,
                warnings,
            };
        }
        let has_non_local_line_edits = before_line_count != after_line_count;
        if has_non_local_line_edits {
            let synthesized_policy = kept_edits
                .first()
                .map(|edit| edit.policy.as_str())
                .unwrap_or("line_suppression_guard");
            let synthesized =
                Self::synthesize_line_edits(before_text, result_text.as_str(), synthesized_policy);
            let filtered = synthesized
                .into_iter()
                .filter(|edit| !disabled_lines.contains(&edit.line))
                .collect::<Vec<_>>();
            let adjusted_text = Self::apply_synthesized_edits(before_text, &filtered)
                .or_else(|| Self::apply_synthesized_edits_best_effort(before_text, &filtered));
            let Some(adjusted_text) = adjusted_text else {
                warnings.push(
                    "line suppression escalated to full rollback due non-local line edits"
                        .to_string(),
                );
                return PolicyResult {
                    text: before_text.to_string(),
                    violations: kept_violations,
                    edits: Vec::new(),
                    warnings,
                };
            };
            let synthesized = Self::synthesize_line_edits(
                before_text,
                adjusted_text.as_str(),
                synthesized_policy,
            );
            let leaked_disabled_lines = synthesized
                .iter()
                .any(|edit| disabled_lines.contains(&edit.line));
            if !leaked_disabled_lines {
                warnings.push(format!(
                    "line suppression applied best-effort non-local rollback for {} blocked line(s)",
                    disabled_lines.len()
                ));
                return PolicyResult {
                    text: adjusted_text,
                    violations: kept_violations,
                    edits: synthesized,
                    warnings,
                };
            }
            warnings.push(
                "line suppression escalated to full rollback due non-local line edits".to_string(),
            );
            return PolicyResult {
                text: before_text.to_string(),
                violations: kept_violations,
                edits: Vec::new(),
                warnings,
            };
        }

        if kept_edits.is_empty() {
            return PolicyResult {
                text: before_text.to_string(),
                violations: kept_violations,
                edits: kept_edits,
                warnings,
            };
        }

        let mut before_lines = before_lines;
        let mut after_lines = after_lines;
        for line_no in disabled_lines {
            let index = line_no.saturating_sub(1);
            if index < max_count {
                after_lines[index] = before_lines[index];
            }
        }
        before_lines.clear();
        let text = after_lines.concat();
        PolicyResult {
            text,
            violations: kept_violations,
            edits: kept_edits,
            warnings,
        }
    }

    fn has_non_local_text_shape_change(before_text: &str, after_text: &str) -> bool {
        text_scan::TEXT_SCAN.has_line_count_changed(before_text, after_text)
    }

    fn edit_lines(edits: &[Edit]) -> BTreeSet<usize> {
        edits
            .iter()
            .filter_map(|edit| (edit.line > 0 && edit.before != edit.after).then_some(edit.line))
            .collect::<BTreeSet<_>>()
    }

    fn line_hint<I>(lines: I, line_count: usize, max_lines: usize) -> String
    where
        I: Iterator<Item = usize>,
    {
        let mut sample = lines
            .take(max_lines)
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        if line_count > sample.len() {
            sample.push(format!("+{}", line_count - sample.len()));
        }
        sample.join(",")
    }

    fn apply_synthesized_edits(before_text: &str, edits: &[Edit]) -> Option<String> {
        if edits.is_empty() {
            return Some(before_text.to_string());
        }
        let mut lines: Vec<Cow<'_, str>> = text_scan::TEXT_SCAN
            .split_lines_as_slices(before_text, true)
            .into_iter()
            .map(Cow::Borrowed)
            .collect();
        let mut ordered = edits
            .iter()
            .filter(|edit| edit.line > 0)
            .collect::<Vec<_>>();
        ordered.sort_by_key(|edit| edit.line);
        let mut offset = 0isize;
        for edit in ordered {
            let base_index = edit.line.saturating_sub(1) as isize + offset;
            if base_index < 0 {
                return None;
            }
            let index = base_index as usize;
            let insertion = edit.before.is_empty() && !edit.after.is_empty();
            let deletion = !edit.before.is_empty() && edit.after.is_empty();
            if insertion {
                if index > lines.len() {
                    return None;
                }
                lines.insert(index, Cow::Owned(edit.after.clone()));
                offset = offset.saturating_add(1);
                continue;
            }
            if index >= lines.len() {
                return None;
            }
            if lines[index] != edit.before {
                return None;
            }
            if deletion {
                lines.remove(index);
                offset = offset.saturating_sub(1);
            } else {
                lines[index] = Cow::Owned(edit.after.clone());
            }
        }
        let mut result = String::with_capacity(before_text.len());
        for line in &lines {
            result.push_str(line);
        }
        Some(result)
    }

    fn semantic_error_lines(regions: &[SemanticRegion]) -> BTreeSet<usize> {
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

    fn apply_synthesized_edits_best_effort(before_text: &str, edits: &[Edit]) -> Option<String> {
        if edits.is_empty() {
            return Some(before_text.to_string());
        }
        let mut lines: Vec<Cow<'_, str>> = text_scan::TEXT_SCAN
            .split_lines_as_slices(before_text, true)
            .into_iter()
            .map(Cow::Borrowed)
            .collect();
        let mut ordered = edits
            .iter()
            .filter(|edit| edit.line > 0)
            .collect::<Vec<_>>();
        ordered.sort_by_key(|edit| edit.line);
        let mut offset = 0isize;
        for edit in ordered {
            let base_index = edit.line.saturating_sub(1) as isize + offset;
            if base_index < 0 {
                continue;
            }
            let index = base_index as usize;
            let insertion = edit.before.is_empty() && !edit.after.is_empty();
            let deletion = !edit.before.is_empty() && edit.after.is_empty();
            if insertion {
                if index > lines.len() {
                    continue;
                }
                lines.insert(index, Cow::Owned(edit.after.clone()));
                offset = offset.saturating_add(1);
                continue;
            }
            if index >= lines.len() {
                continue;
            }
            if deletion {
                if lines[index] == edit.before {
                    lines.remove(index);
                    offset = offset.saturating_sub(1);
                }
            } else if lines[index] == edit.before {
                lines[index] = Cow::Owned(edit.after.clone());
            }
        }
        let mut result = String::with_capacity(before_text.len());
        for line in &lines {
            result.push_str(line);
        }
        Some(result)
    }

    fn comment_lines_from_tree(
        tree: Option<&Tree>,
        query_cache: &TsQueryCache,
    ) -> BTreeSet<usize> {
        let Some(tree) = tree else {
            return BTreeSet::new();
        };
        let Ok(query) = query_cache.get_or_compile("(comment) @c") else {
            return BTreeSet::new();
        };
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), "".as_bytes());
        let mut lines = BTreeSet::<usize>::new();
        while let Some(m) = {
            matches.advance();
            matches.get()
        } {
            for capture in m.captures {
                let start = capture.node.start_position().row.saturating_add(1);
                let end = capture.node.end_position().row.saturating_add(1).max(start);
                for line in start..=end {
                    lines.insert(line);
                }
            }
        }
        lines
    }

    pub(crate) fn extract_raw_observation(
        confidence: &ConfidenceContext,
        semantic_summary: Option<SemanticSummary>,
        semantic_compdb_context_for_file: bool,
        clang_staleness: usize,
    ) -> [f64; 4] {
        let structural = if confidence.tree_available {
            1.0 - confidence.tree_error_ratio
        } else {
            0.0
        };

        let error_count = semantic_summary
            .as_ref()
            .map(|s| s.diagnostic_error_count)
            .unwrap_or(0);
        let semantic = crate::engine::fuzzy_inference::fuzzy_raw_semantic_observation(
            semantic_compdb_context_for_file,
            confidence.clang_success,
            confidence.semantic_usr_ratio,
            confidence.tree_available,
            error_count,
        );

        let coverage = confidence.semantic_usr_ratio;

        let richness = if let Some(summary) = semantic_summary {
            let combined = (summary.scope_count + summary.reference_count) as f64;
            1.0 / (1.0 + (-0.05 * (combined - 40.0)).exp())
        } else {
            0.0
        };

        if clang_staleness > 0 {
            let tree_scope_count = semantic_summary.map(|s| s.scope_count).unwrap_or(0);
            let clang_decl_count = semantic_summary.map(|s| s.declaration_count).unwrap_or(0);
            let agreement = crate::engine::fuzzy_inference::fuzzy_parser_cross_validation(
                tree_scope_count,
                clang_decl_count,
                confidence.tree_error_ratio,
                error_count,
                clang_staleness,
            );
            return [structural, semantic * agreement, coverage * agreement, richness];
        }

        [structural, semantic, coverage, richness]
    }

    fn apply_certainty_filter(
        &self,
        path: &Path,
        parser_measurement: [f64; 4],
        content_hash: u64,
    ) -> PolicyCertainty {
        let key = path.to_string_lossy();
        let edit_outcome = self.certainty_filter_store.last_edit_outcome(&key);
        let fallback = self.certainty_filter_store.current_edit_success_estimate(&key);
        let measurement: [f64; 5] = [
            parser_measurement[0],
            parser_measurement[1],
            parser_measurement[2],
            parser_measurement[3],
            edit_outcome.unwrap_or(fallback),
        ];
        let result = self.certainty_filter_store.observe(&key, measurement, content_hash);
        let structural = result.structural();
        let semantic = result.semantic();
        let coverage = result.coverage();
        let richness = result.richness();
        let coverage_weight = crate::engine::fuzzy_inference::fuzzy_coverage_weight(coverage);
        let overall = (coverage_weight * semantic + (1.0 - coverage_weight) * structural).clamp(0.0, 1.0);
        PolicyCertainty {
            overall,
            structural,
            semantic,
            coverage,
            richness,
            semantic_variance: result.semantic_variance(),
            structural_variance: result.structural_variance(),
            coverage_variance: result.coverage_variance(),
            richness_variance: result.richness_variance(),
            edit_success: result.edit_success(),
            edit_success_variance: result.edit_success_variance(),
            stable_model_prob: result.model_probs[0],
            transitional_model_prob: result.model_probs[1],
            noisy_model_prob: result.model_probs[2],
            observation_count: result.observation_count,
            raw_observation: Some(measurement),
        }
    }

    #[cfg(test)]
    fn requires_exact_compdb_for_wide_structural(policy_name: &str) -> bool {
        policy_catalog()
            .behavior_for_name(policy_name)
            .requires_exact_compdb_for_wide_structural
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
        certainty: &PolicyCertainty,
        scope_stage: crate::engine::run_options::RetryScopeStage,
        disallowed_zone_lines: &BTreeSet<usize>,
        hard_blocked_lines: &BTreeSet<usize>,
        dropped_lines: &BTreeSet<usize>,
        kept_lines: &BTreeSet<usize>,
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
                    utility: GlobalConflictSolver::utility_score(candidate, certainty, scope_stage),
                    risk_tier: candidate.risk_tier,
                    impact_radius: candidate.impact_radius,
                    symbol_footprint_count: candidate.symbol_footprint.len(),
                    range_footprint_count: candidate.range_footprint.len(),
                    hard_constraints_touched: candidate
                        .hard_constraints_touched
                        .iter()
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
    use std::collections::{BTreeSet, HashMap};
    use std::sync::Arc;

    use toml::Table;

    use crate::config::policy_config::PolicyConfig;
    use crate::engine::conflict_solver::GlobalConflictSolver;
    use crate::engine::catalog::{
        PolicyCapabilities, PolicyCapabilityMatrix, PolicyCertainty,
    };
    use crate::engine::edit_candidate::{CandidateRiskTier, PolicyEditCandidate};
    use crate::engine::pipeline::PolicyPipeline;
    use crate::engine::run_options::RetryScopeStage;
    use crate::engine::zone::PolicyZone;
    use crate::engine::semantic_contract::{PolicyGuidanceMode, SemanticInvariantClause};
    use crate::model::edit::Edit;
    use crate::model::policy_result::PolicyResult;
    use crate::model::context_query::SemanticContextQuery;
    use crate::model::violation::Violation;
    use crate::parser::clang_result::ClangDiagnosticEntry;
    use crate::parser::clang_result::ClangDiagnosticSeverity;
    use crate::parser::clang_result::ClangDiagnosticSummary;
    use crate::parser::clang_types::ClangSymbolKind;
    use crate::parser::manager::SemanticCompdbContextKind;
    use crate::parser::file_context::{
        SemanticDeclaration, SemanticFileContext, SemanticIdProvenance, SemanticReference,
        SemanticScope, SemanticScopeKind,
    };
    use crate::parser::node_kind;
    #[test]
    fn semantic_guideline_drops_unsafe_lines_only() {
        let semantic = SemanticFileContext {
            scopes: vec![SemanticScope {
                kind: SemanticScopeKind::Preprocessor,
                node_kind: node_kind::PREPROC_IF,
                start_offset: 0,
                end_offset: 16,
                start_line: 2,
                end_line: 2,
            }],
            ..SemanticFileContext::default()
        };
        let query = SemanticContextQuery::from_semantic_file_context(Some(&semantic));
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
        };
        let capability = PolicyCapabilityMatrix::for_policy("sample");

        let adjusted = PolicyPipeline::apply_semantic_guidance_mode(
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
    fn semantic_hard_invariant_blocks_unsafe_lines() {
        let semantic = SemanticFileContext {
            diagnostic_entries: vec![ClangDiagnosticEntry {
                line: 4,
                column: 1,
                severity: ClangDiagnosticSeverity::Error,
            }],
            declarations: vec![SemanticDeclaration {
                stable_id: "usr:c:@F@unsafe#".to_string(),
                provenance: SemanticIdProvenance::Usr,
                name: "unsafe".to_string(),
                kind: ClangSymbolKind::Function,
                line: 4,
                column: 1,
                usr: Some("c:@F@unsafe#".to_string()),
                scope_usr: None,
            }],
            ..SemanticFileContext::default()
        };
        let query = SemanticContextQuery::from_semantic_file_context(Some(&semantic));
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
        };
        let capability = PolicyCapabilityMatrix::for_policy("sample");

        let adjusted = PolicyPipeline::apply_semantic_guidance_mode(
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
    fn semantic_guideline_allows_preprocessor_structural_edits() {
        let semantic = SemanticFileContext {
            scopes: vec![SemanticScope {
                kind: SemanticScopeKind::Preprocessor,
                node_kind: node_kind::PREPROC_INCLUDE,
                start_offset: 0,
                end_offset: 20,
                start_line: 2,
                end_line: 2,
            }],
            ..SemanticFileContext::default()
        };
        let query = SemanticContextQuery::from_semantic_file_context(Some(&semantic));
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
        };
        let capability = PolicyCapabilityMatrix::for_policy("include_order");

        let adjusted = PolicyPipeline::apply_semantic_guidance_mode(
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
    fn scope_filter_drops_edits_outside_retry_scope() {
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
                    ..PolicyCapabilityMatrix::for_policy("sample")
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
    fn scope_filter_reverts_semantic_rewrite_when_out_of_scope_lines_exist() {
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
                    ..PolicyCapabilityMatrix::for_policy("naming_conventions")
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
    fn scope_filter_skips_saturated_non_local_structural_batches() {
        let before = "l1\nl2\nl3\n";
        let mut result = PolicyResult {
            text: "x1\nx2\nx3\nx4\nx5\nx6\n".to_string(),
            violations: Vec::new(),
            edits: Vec::new(),
            warnings: Vec::new(),
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
                    ..PolicyCapabilityMatrix::for_policy("class_layout")
                },
            },
        );
        assert!(filtered.warnings.iter().any(|warning| warning.contains("dropped") && warning.contains("out-of-scope")),
            "scope filter should drop out-of-scope lines without full revert");
    }

    #[test]
    fn wide_structural_policies_require_exact_compdb() {
        assert!(PolicyPipeline::requires_exact_compdb_for_wide_structural(
            "clang_format"
        ));
        assert!(PolicyPipeline::requires_exact_compdb_for_wide_structural(
            "include_order"
        ));
        assert!(PolicyPipeline::requires_exact_compdb_for_wide_structural(
            "class_layout"
        ));
        assert!(PolicyPipeline::requires_exact_compdb_for_wide_structural(
            "compact_declarations"
        ));
    }

    #[test]
    fn convergence_profile_reads_impact_radius_and_semantic_weight() {
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
            "semantic_priority_weight_bp".to_string(),
            toml::Value::Integer(650),
        );
        policy_table.insert("convergence".to_string(), toml::Value::Table(convergence));
        let policy = PolicyConfig::from_policy_table(&policy_table).expect("policy config");
        let profiles = PolicyPipeline::build_convergence_profiles(&HashMap::from([(
            "policy_a".to_string(),
            policy,
        )]));
        let profile = profiles.get("policy_a").expect("profile");
        assert_eq!(profile.domain, "layout");
        assert_eq!(profile.priority, 777);
        assert_eq!(profile.impact_radius, 3);
        assert_eq!(profile.semantic_priority_weight_bp, 650);
    }

    #[test]
    fn convergence_signal_increases_radius_for_large_context_and_batch() {
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
        };
        let semantic = SemanticFileContext {
            clang_success: true,
            tree_has_error: false,
            diagnostic_summary: ClangDiagnosticSummary::default(),
            declarations: vec![],
            references: (0..2_100)
                .map(|offset| SemanticReference {
                    stable_id: "usr:test".to_string(),
                    provenance: SemanticIdProvenance::Usr,
                    decl_path: "sample.cpp".to_string(),
                    decl_kind: ClangSymbolKind::Variable,
                    offset,
                    line: 1,
                    column: 1,
                })
                .collect(),
            ..SemanticFileContext::default()
        };
        let summary = semantic.summary();
        let capability = PolicyCapabilityMatrix::for_policy("compact_declarations");
        let signal = PolicyPipeline::build_convergence_signal(ConvergenceSignalInput {
            result: &result,
            semantic: Some(&semantic),
            summary,
            semantic_fidelity_score: 1.0,
            previous_contract_failures: &BTreeSet::new(),
            capability: &capability,
            cluster_radius_cap: None,
            trust: 0.5,
        });
        assert!(signal.semantic_confidence_bp >= 700);
        assert!(signal.impact_radius >= 2);
    }

    #[test]
    fn convergence_signal_builds_scope_and_symbol_impact_maps() {
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
        };
        let semantic = SemanticFileContext {
            clang_success: true,
            tree_has_error: false,
            declarations: vec![SemanticDeclaration {
                stable_id: "usr:c:@F@value#".to_string(),
                provenance: SemanticIdProvenance::Usr,
                name: "value".to_string(),
                kind: ClangSymbolKind::Variable,
                line: 10,
                column: 5,
                usr: Some("c:@F@value#".to_string()),
                scope_usr: None,
            }],
            references: vec![SemanticReference {
                stable_id: "usr:c:@F@value#".to_string(),
                provenance: SemanticIdProvenance::Usr,
                decl_path: "sample.cpp".to_string(),
                decl_kind: ClangSymbolKind::Variable,
                offset: 0,
                line: 12,
                column: 3,
            }],
            scopes: vec![SemanticScope {
                kind: SemanticScopeKind::Function,
                node_kind: node_kind::FUNCTION_DEFINITION,
                start_offset: 0,
                end_offset: 100,
                start_line: 8,
                end_line: 20,
            }],
            diagnostic_summary: ClangDiagnosticSummary::default(),
            ..SemanticFileContext::default()
        };
        let capability = PolicyCapabilityMatrix::for_policy("compact_declarations");
        let signal = PolicyPipeline::build_convergence_signal(ConvergenceSignalInput {
            result: &result,
            semantic: Some(&semantic),
            summary: semantic.summary(),
            semantic_fidelity_score: 1.0,
            previous_contract_failures: &BTreeSet::new(),
            capability: &capability,
            cluster_radius_cap: None,
            trust: 0.5,
        });

        let line10_ranges = signal
            .semantic_impact_ranges_by_line
            .get(&10)
            .expect("line 10 ranges");
        assert!(line10_ranges
            .iter()
            .any(|(start, end)| *start <= 10 && *end >= 12));

        let line12_ranges = signal
            .semantic_impact_ranges_by_line
            .get(&12)
            .expect("line 12 ranges");
        assert!(line12_ranges
            .iter()
            .any(|(start, end)| *start <= 10 && *end >= 12));

        let ids_line10 = signal
            .semantic_symbol_ids_by_line
            .get(&10)
            .expect("line 10 symbol ids");
        let ids_line12 = signal
            .semantic_symbol_ids_by_line
            .get(&12)
            .expect("line 12 symbol ids");
        assert!(ids_line10.iter().any(|id| ids_line12.contains(id)));
    }

    #[test]
    fn convergence_signal_penalizes_previous_contract_failures() {
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
        };
        let semantic = SemanticFileContext {
            clang_success: true,
            tree_has_error: false,
            diagnostic_summary: ClangDiagnosticSummary::default(),
            ..SemanticFileContext::default()
        };
        let capability = PolicyCapabilityMatrix::for_policy("compact_declarations");
        let base = PolicyPipeline::build_convergence_signal(ConvergenceSignalInput {
            result: &result,
            semantic: Some(&semantic),
            summary: semantic.summary(),
            semantic_fidelity_score: 1.0,
            previous_contract_failures: &BTreeSet::new(),
            capability: &capability,
            cluster_radius_cap: None,
            trust: 0.5,
        });
        let failures = BTreeSet::from([
            SemanticInvariantClause::EditSafety,
            SemanticInvariantClause::DeclarationReferenceIntegrity,
        ]);
        let penalized = PolicyPipeline::build_convergence_signal(ConvergenceSignalInput {
            result: &result,
            semantic: Some(&semantic),
            summary: semantic.summary(),
            semantic_fidelity_score: 1.0,
            previous_contract_failures: &failures,
            capability: &capability,
            cluster_radius_cap: None,
            trust: 0.5,
        });
        assert!(penalized.semantic_confidence_bp < base.semantic_confidence_bp);
        assert!(penalized.impact_radius >= base.impact_radius);
    }

    #[test]
    fn trust_low_semantic_ci_yields_low_trust_for_semantic_rewrite() {
        let capability = PolicyCapabilityMatrix::for_policy("naming_conventions");
        let certainty = PolicyCertainty {
            overall: 0.80,
            structural: 0.90,
            semantic: 0.35,
            ..Default::default()
        };
        assert!(capability.policy_trust(&certainty) < 0.20);
    }

    #[test]
    fn trust_high_certainty_low_variance_yields_high_trust() {
        let capability = PolicyCapabilityMatrix::for_policy("naming_conventions");
        let certainty = PolicyCertainty {
            overall: 0.74,
            structural: 0.70,
            semantic: 0.88,
            coverage: 0.60,
            semantic_variance: 0.002,
            structural_variance: 0.002,
            coverage_variance: 0.002,
            stable_model_prob: 0.70,
            edit_success: 0.80,
            edit_success_variance: 0.002,
            ..Default::default()
        };
        assert!(capability.policy_trust(&certainty) > 0.40);
    }

    #[test]
    fn trust_high_variance_yields_lower_trust() {
        let capability = PolicyCapabilityMatrix::for_policy("naming_conventions");
        let low_var = PolicyCertainty {
            semantic: 0.70,
            coverage: 0.60,
            semantic_variance: 0.002,
            coverage_variance: 0.002,
            stable_model_prob: 0.50,
            edit_success: 0.50,
            edit_success_variance: 0.01,
            ..Default::default()
        };
        let high_var = PolicyCertainty {
            semantic_variance: 0.03,
            coverage_variance: 0.03,
            ..low_var
        };
        assert!(capability.policy_trust(&low_var) > capability.policy_trust(&high_var));
    }

    #[test]
    fn line_suppression_keeps_local_blank_line_edits_when_line_counts_match() {
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
        };
        let suppressed =
            PolicyPipeline::apply_line_suppression(before, result, &BTreeSet::from([3usize]));
        assert_eq!(suppressed.text, "int x = 0;\n    \nint y = 1;\n");
        assert_eq!(suppressed.edits.len(), 1);
    }

    #[test]
    fn line_suppression_best_effort_handles_non_local_line_count_changes() {
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
    fn line_suppression_no_threshold_revert_for_wide_non_local_batches() {
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
        };
        let suppressed =
            PolicyPipeline::apply_line_suppression(before.as_str(), result, &BTreeSet::from([3usize]));
        assert!(!suppressed.warnings.iter().any(|w| w.contains("wide non-local batch")),
            "no threshold-based wide batch revert should occur");
    }

    #[test]
    fn line_suppression_structural_safe_skips_wide_batch_escalation() {
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
        };
        let suppressed = PolicyPipeline::apply_line_suppression_structural(
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
    fn candidate_conflict_model_prefers_lower_risk_when_confidence_is_close() {
        let existing = PolicyEditCandidate {
            policy: "naming_conventions".into(),
            line: 42,
            confidence: 0.86,
            risk_tier: CandidateRiskTier::High,
            impact_radius: 3,
            symbol_footprint: vec![11],
            range_footprint: vec![(40, 44)],
            hard_constraints_touched: BTreeSet::from([SemanticInvariantClause::EditSafety]),
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
            symbol_footprint: vec![11],
            range_footprint: vec![(42, 42)],
            hard_constraints_touched: BTreeSet::new(),
            zone: PolicyZone::Code,
            after_fingerprint: 2,
            style_gain: 1.2,
        };
        let certainty = PolicyCertainty {
            overall: 0.50,
            structural: 0.65,
            semantic: 0.45,
            ..Default::default()
        };
        let result = GlobalConflictSolver::solve(
            &[incoming],
            &[existing],
            &certainty,
            RetryScopeStage::LineLocal,
        );
        assert!(result.accepted.is_empty());
        assert!(result.dropped_lines.contains(&42));
    }

    #[test]
    fn stabilize_output_text_reverts_untracked_delta_without_edits() {
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
    fn stabilize_output_text_keeps_delta_when_edits_exist() {
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
    fn normalize_edit_coverage_expands_block_edit_metadata() {
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
    fn normalize_edit_coverage_clears_stale_edits_when_text_unchanged() {
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
    fn normalize_edit_coverage_keeps_exact_multiline_declared_edits() {
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
