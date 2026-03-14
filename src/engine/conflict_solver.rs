use std::collections::{BTreeMap, BTreeSet};

use crate::engine::catalog::PolicyCertainty;
use crate::engine::catalog::policy_catalog;
use crate::engine::edit_candidate::{CandidateRiskTier, PolicyEditCandidate};
use crate::engine::run_options::RetryScopeStage;
use crate::engine::zone::PolicyZone;
use crate::engine::semantic_contract::SemanticContract;

#[derive(Clone, Debug, Default)]
pub struct GlobalConflictSolveResult {
    pub accepted: Vec<PolicyEditCandidate>,
    pub dropped_lines: BTreeSet<usize>,
    pub hard_blocked_lines: BTreeSet<usize>,
}

struct ComponentSolveInput<'a> {
    admissible: &'a [PolicyEditCandidate],
    adjacency: &'a [Vec<usize>],
    component: &'a [usize],
    certainty: &'a PolicyCertainty,
    scope_stage: RetryScopeStage,
}

pub struct GlobalConflictSolver;

impl GlobalConflictSolver {
    pub fn solve(
        incoming: &[PolicyEditCandidate],
        already_selected: &[PolicyEditCandidate],
        certainty: &PolicyCertainty,
        scope_stage: RetryScopeStage,
    ) -> GlobalConflictSolveResult {
        if incoming.is_empty() {
            return GlobalConflictSolveResult::default();
        }

        let consolidated = Self::consolidate_by_line(incoming, certainty, scope_stage);
        let mut dropped_lines = BTreeSet::<usize>::new();
        let mut hard_blocked_lines = BTreeSet::<usize>::new();
        let mut admissible = Vec::<PolicyEditCandidate>::new();

        for candidate in consolidated {
            if Self::violates_hard_constraints(&candidate) {
                hard_blocked_lines.insert(candidate.line);
                dropped_lines.insert(candidate.line);
                continue;
            }
            if already_selected
                .iter()
                .any(|existing| Self::conflicts(&candidate, existing, certainty))
            {
                dropped_lines.insert(candidate.line);
                continue;
            }
            admissible.push(candidate);
        }

        let accepted = Self::select_conflict_optimal(admissible.as_slice(), certainty, scope_stage);
        for candidate in &admissible {
            let conflicts = accepted
                .iter()
                .filter(|winner| Self::conflicts(candidate, winner, certainty))
                .collect::<Vec<_>>();
            if conflicts.is_empty() {
                if accepted.iter().any(|winner| {
                    winner.line == candidate.line && winner.policy == candidate.policy
                }) {
                    continue;
                }
                dropped_lines.insert(candidate.line);
                continue;
            }
            if accepted
                .iter()
                .any(|winner| winner.line == candidate.line && winner.policy == candidate.policy)
            {
                continue;
            }
            dropped_lines.insert(candidate.line);
        }

        GlobalConflictSolveResult {
            accepted,
            dropped_lines,
            hard_blocked_lines,
        }
    }

    fn consolidate_by_line(
        incoming: &[PolicyEditCandidate],
        certainty: &PolicyCertainty,
        scope_stage: RetryScopeStage,
    ) -> Vec<PolicyEditCandidate> {
        let mut best_by_line = std::collections::HashMap::<usize, PolicyEditCandidate>::new();
        for candidate in incoming {
            match best_by_line.get(&candidate.line) {
                Some(existing) => {
                    let candidate_utility = Self::utility_score(candidate, certainty, scope_stage);
                    let existing_utility = Self::utility_score(existing, certainty, scope_stage);
                    let choose_new = candidate_utility > existing_utility + 0.000_001
                        || (candidate_utility - existing_utility).abs() <= 0.000_001
                            && candidate.confidence > existing.confidence + 0.001
                        || (candidate_utility - existing_utility).abs() <= 0.000_001
                            && (candidate.confidence - existing.confidence).abs() <= 0.001
                            && candidate.risk_tier.precedence_score()
                                > existing.risk_tier.precedence_score();
                    if choose_new {
                        best_by_line.insert(candidate.line, candidate.clone());
                    }
                }
                None => {
                    best_by_line.insert(candidate.line, candidate.clone());
                }
            }
        }
        let mut consolidated = best_by_line.into_values().collect::<Vec<_>>();
        consolidated.sort_by(|left, right| {
            left.line
                .cmp(&right.line)
                .then(left.policy.cmp(&right.policy))
        });
        consolidated
    }

    pub fn utility_score(
        candidate: &PolicyEditCandidate,
        certainty: &PolicyCertainty,
        scope_stage: RetryScopeStage,
    ) -> f64 {
        let uncertainty = 1.0 - certainty.trust_for_general();
        let risk_tier_index = match candidate.risk_tier {
            CandidateRiskTier::Low => 0,
            CandidateRiskTier::Medium => 1,
            CandidateRiskTier::High => 2,
        };
        let stabilizer_bonus =
            crate::engine::fuzzy_inference::fuzzy_risk_stabilizer(risk_tier_index, uncertainty);
        let scope_narrowness = match scope_stage {
            RetryScopeStage::Full => 0.0,
            RetryScopeStage::CulpritRegion => 0.5,
            RetryScopeStage::NodeLocal | RetryScopeStage::LineLocal => 1.0,
        };
        let scope_bonus =
            crate::engine::fuzzy_inference::fuzzy_scope_bonus(risk_tier_index, scope_narrowness);
        let risk_penalty = crate::engine::fuzzy_inference::fuzzy_risk_penalty(
            risk_tier_index,
            certainty.trust_for_general(),
        );
        let (range_weight, symbol_weight) =
            crate::engine::fuzzy_inference::fuzzy_footprint_weights(certainty.trust_for_general());
        let footprint_penalty = ((candidate.range_footprint.len() as f64 * range_weight)
            + (candidate.symbol_footprint.len() as f64 * symbol_weight))
            .min(0.30);
        let style_weight =
            crate::engine::fuzzy_inference::fuzzy_style_weight(uncertainty);
        let confidence_weight =
            crate::engine::fuzzy_inference::fuzzy_confidence_weight(uncertainty);
        (candidate.style_gain * style_weight)
            + (candidate.confidence * confidence_weight)
            + stabilizer_bonus
            + scope_bonus
            - risk_penalty
            - footprint_penalty
    }

    fn violates_hard_constraints(candidate: &PolicyEditCandidate) -> bool {
        candidate.hard_constraints_touched.iter().any(|clause| {
            SemanticContract::invariant_spec(*clause)
                .map(|spec| spec.hard)
                .unwrap_or(true)
        })
    }

    fn conflicts(left: &PolicyEditCandidate, right: &PolicyEditCandidate, certainty: &PolicyCertainty) -> bool {
        if left.policy == right.policy {
            return left.line == right.line;
        }
        let either_bypasses = {
            let catalog = policy_catalog();
            catalog.bypasses_line_conflict(left.policy.as_str())
                || catalog.bypasses_line_conflict(right.policy.as_str())
        };
        if either_bypasses {
            return false;
        }
        if left.line == right.line {
            return true;
        }
        if Self::ranges_overlap(
            left.range_footprint.as_slice(),
            right.range_footprint.as_slice(),
        ) {
            return true;
        }
        if Self::symbols_overlap(
            left.symbol_footprint.as_slice(),
            right.symbol_footprint.as_slice(),
        ) {
            let line_gap = left.line.abs_diff(right.line);
            let semantic_sensitive = Self::is_semantic_rewrite_policy(left.policy.as_str())
                || Self::is_semantic_rewrite_policy(right.policy.as_str())
                || left.risk_tier == CandidateRiskTier::High
                || right.risk_tier == CandidateRiskTier::High;
            let trust = certainty.trust_for_general();
            let fuzzy_min = crate::engine::fuzzy_inference::fuzzy_conflict_neighborhood(
                trust,
                semantic_sensitive,
            );
            let neighborhood = left.impact_radius.max(right.impact_radius).max(fuzzy_min);
            if line_gap <= neighborhood {
                return true;
            }
        }
        (left.zone == PolicyZone::Preprocessor || right.zone == PolicyZone::Preprocessor)
            && left.after_fingerprint != right.after_fingerprint
            && (Self::ranges_overlap(
                left.range_footprint.as_slice(),
                right.range_footprint.as_slice(),
            ) || left.line.abs_diff(right.line) <= left.impact_radius.max(right.impact_radius))
    }

    fn is_semantic_rewrite_policy(policy: &str) -> bool {
        policy_catalog().is_semantic_rewrite_policy_name(policy)
    }

    fn ranges_overlap(left: &[(usize, usize)], right: &[(usize, usize)]) -> bool {
        if left.is_empty() || right.is_empty() {
            return false;
        }
        left.iter().any(|(left_start, left_end)| {
            right
                .iter()
                .any(|(right_start, right_end)| left_start <= right_end && right_start <= left_end)
        })
    }

    fn symbols_overlap(left: &[u64], right: &[u64]) -> bool {
        if left.is_empty() || right.is_empty() {
            return false;
        }
        let (small, large) = if left.len() <= right.len() {
            (left, right)
        } else {
            (right, left)
        };
        small.iter().any(|symbol| large.contains(symbol))
    }

    fn select_conflict_optimal(
        admissible: &[PolicyEditCandidate],
        certainty: &PolicyCertainty,
        scope_stage: RetryScopeStage,
    ) -> Vec<PolicyEditCandidate> {
        if admissible.is_empty() {
            return Vec::new();
        }
        let adjacency = Self::build_conflict_adjacency(admissible, certainty);
        let components = Self::connected_components(&adjacency);
        let mut accepted_indexes = Vec::<usize>::new();
        for component in components {
            let component_threshold = if certainty.edit_success_variance > certainty.structural_variance {
                24_usize
            } else {
                12_usize
            };
            let solve_input = ComponentSolveInput {
                admissible,
                adjacency: &adjacency,
                component: &component,
                certainty,
                scope_stage,
            };
            let selected = if component.len() <= component_threshold {
                Self::solve_component_exact(&solve_input)
            } else {
                Self::solve_component_greedy(&solve_input)
            };
            accepted_indexes.extend(selected);
        }
        accepted_indexes.sort_unstable();
        accepted_indexes
            .into_iter()
            .filter_map(|index| admissible.get(index).cloned())
            .collect()
    }

    fn build_conflict_adjacency(admissible: &[PolicyEditCandidate], certainty: &PolicyCertainty) -> Vec<Vec<usize>> {
        let mut adjacency = vec![Vec::<usize>::new(); admissible.len()];
        for left in 0..admissible.len() {
            for right in (left + 1)..admissible.len() {
                if Self::conflicts(&admissible[left], &admissible[right], certainty) {
                    adjacency[left].push(right);
                    adjacency[right].push(left);
                }
            }
        }
        for neighbors in &mut adjacency {
            neighbors.sort_unstable();
            neighbors.dedup();
        }
        adjacency
    }

    fn connected_components(adjacency: &[Vec<usize>]) -> Vec<Vec<usize>> {
        let mut components = Vec::<Vec<usize>>::new();
        let mut visited = vec![false; adjacency.len()];
        for seed in 0..adjacency.len() {
            if visited[seed] {
                continue;
            }
            let mut stack = vec![seed];
            let mut component = Vec::<usize>::new();
            visited[seed] = true;
            while let Some(node) = stack.pop() {
                component.push(node);
                for &neighbor in &adjacency[node] {
                    if !visited[neighbor] {
                        visited[neighbor] = true;
                        stack.push(neighbor);
                    }
                }
            }
            component.sort_unstable();
            components.push(component);
        }
        components.sort();
        components
    }

    fn solve_component_exact(input: &ComponentSolveInput<'_>) -> Vec<usize> {
        let admissible = input.admissible;
        let adjacency = input.adjacency;
        let component = input.component;
        let certainty = input.certainty;
        let scope_stage = input.scope_stage;
        let mut ordered = component.to_vec();
        ordered.sort_by(|left, right| {
            let left_candidate = &admissible[*left];
            let right_candidate = &admissible[*right];
            let left_utility = Self::utility_score(left_candidate, certainty, scope_stage);
            let right_utility = Self::utility_score(right_candidate, certainty, scope_stage);
            right_utility
                .total_cmp(&left_utility)
                .then_with(|| {
                    right_candidate
                        .confidence
                        .total_cmp(&left_candidate.confidence)
                })
                .then_with(|| {
                    right_candidate
                        .risk_tier
                        .precedence_score()
                        .cmp(&left_candidate.risk_tier.precedence_score())
                })
                .then_with(|| left_candidate.line.cmp(&right_candidate.line))
                .then_with(|| left_candidate.policy.cmp(&right_candidate.policy))
        });
        let weights = ordered
            .iter()
            .map(|index| Self::utility_score(&admissible[*index], certainty, scope_stage).max(0.0))
            .collect::<Vec<_>>();
        let mut suffix_upper_bound = vec![0.0f64; weights.len() + 1];
        for idx in (0..weights.len()).rev() {
            suffix_upper_bound[idx] = suffix_upper_bound[idx + 1] + weights[idx];
        }

        let mut index_by_node = BTreeMap::<usize, usize>::new();
        for (position, node) in ordered.iter().enumerate() {
            index_by_node.insert(*node, position);
        }
        let mut ordered_conflicts = vec![Vec::<usize>::new(); ordered.len()];
        for (position, node) in ordered.iter().enumerate() {
            let mut neighbors = adjacency[*node]
                .iter()
                .filter_map(|neighbor| index_by_node.get(neighbor).copied())
                .filter(|neighbor_position| *neighbor_position > position)
                .collect::<Vec<_>>();
            neighbors.sort_unstable();
            neighbors.dedup();
            ordered_conflicts[position] = neighbors;
        }

        let mut best_score = f64::MIN;
        let mut best_set = Vec::<usize>::new();
        let mut current_set = Vec::<usize>::new();
        let mut blocked = vec![false; ordered.len()];

        struct ExactComponentSearch<'a> {
            ordered: &'a [usize],
            ordered_conflicts: &'a [Vec<usize>],
            blocked: &'a mut [bool],
            current_set: &'a mut Vec<usize>,
            best_score: &'a mut f64,
            best_set: &'a mut Vec<usize>,
            suffix_upper_bound: &'a [f64],
            weights: &'a [f64],
            admissible: &'a [PolicyEditCandidate],
        }

        impl ExactComponentSearch<'_> {
            fn better_tie_break(&self, right: &[usize]) -> bool {
                let mut left_keys = self
                    .current_set
                    .iter()
                    .filter_map(|position| self.ordered.get(*position))
                    .filter_map(|node| self.admissible.get(*node))
                    .map(|candidate| (candidate.line, candidate.policy.clone()))
                    .collect::<Vec<_>>();
                let mut right_keys = right
                    .iter()
                    .filter_map(|position| self.ordered.get(*position))
                    .filter_map(|node| self.admissible.get(*node))
                    .map(|candidate| (candidate.line, candidate.policy.clone()))
                    .collect::<Vec<_>>();
                left_keys.sort();
                right_keys.sort();
                left_keys < right_keys
            }

            fn dfs(&mut self, position: usize, running_score: f64) {
                if position >= self.ordered.len() {
                    let is_better = running_score > *self.best_score + 0.000_001;
                    let is_tie = (running_score - *self.best_score).abs() <= 0.000_001;
                    if is_better || (is_tie && self.better_tie_break(self.best_set.as_slice())) {
                        *self.best_score = running_score;
                        *self.best_set = self.current_set.clone();
                    }
                    return;
                }
                if running_score + self.suffix_upper_bound[position] + 0.000_001 < *self.best_score
                {
                    return;
                }

                if !self.blocked[position] {
                    let mut toggled = Vec::<usize>::new();
                    self.blocked[position] = true;
                    toggled.push(position);
                    for neighbor in &self.ordered_conflicts[position] {
                        if !self.blocked[*neighbor] {
                            self.blocked[*neighbor] = true;
                            toggled.push(*neighbor);
                        }
                    }
                    self.current_set.push(position);
                    self.dfs(position + 1, running_score + self.weights[position]);
                    self.current_set.pop();
                    for index in toggled {
                        self.blocked[index] = false;
                    }
                }

                self.dfs(position + 1, running_score);
            }
        }

        let mut search = ExactComponentSearch {
            ordered: ordered.as_slice(),
            ordered_conflicts: ordered_conflicts.as_slice(),
            blocked: blocked.as_mut_slice(),
            current_set: &mut current_set,
            best_score: &mut best_score,
            best_set: &mut best_set,
            suffix_upper_bound: suffix_upper_bound.as_slice(),
            weights: weights.as_slice(),
            admissible,
        };
        search.dfs(0, 0.0);

        best_set
            .into_iter()
            .filter_map(|position| ordered.get(position).copied())
            .collect()
    }

    fn solve_component_greedy(input: &ComponentSolveInput<'_>) -> Vec<usize> {
        let admissible = input.admissible;
        let adjacency = input.adjacency;
        let component = input.component;
        let certainty = input.certainty;
        let scope_stage = input.scope_stage;
        let mut ordered = component.to_vec();
        ordered.sort_by(|left, right| {
            let left_candidate = &admissible[*left];
            let right_candidate = &admissible[*right];
            let left_utility = Self::utility_score(left_candidate, certainty, scope_stage);
            let right_utility = Self::utility_score(right_candidate, certainty, scope_stage);
            right_utility
                .total_cmp(&left_utility)
                .then_with(|| {
                    right_candidate
                        .confidence
                        .total_cmp(&left_candidate.confidence)
                })
                .then_with(|| {
                    right_candidate
                        .risk_tier
                        .precedence_score()
                        .cmp(&left_candidate.risk_tier.precedence_score())
                })
                .then_with(|| left_candidate.line.cmp(&right_candidate.line))
                .then_with(|| left_candidate.policy.cmp(&right_candidate.policy))
        });
        let mut selected = Vec::<usize>::new();
        let mut blocked = BTreeSet::<usize>::new();
        for index in ordered {
            if blocked.contains(&index) {
                continue;
            }
            selected.push(index);
            blocked.insert(index);
            for neighbor in &adjacency[index] {
                blocked.insert(*neighbor);
            }
        }
        selected
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::engine::conflict_solver::GlobalConflictSolver;
    use crate::engine::catalog::PolicyCertainty;
    use crate::engine::edit_candidate::{CandidateRiskTier, PolicyEditCandidate};
    use crate::engine::run_options::RetryScopeStage;
    use crate::engine::zone::PolicyZone;
    use crate::engine::semantic_contract::SemanticInvariantClause;

    #[test]
    fn hard_constraint_candidates_are_rejected() {
        let candidate = PolicyEditCandidate {
            policy: "naming_conventions".into(),
            line: 7,
            confidence: 0.9,
            risk_tier: CandidateRiskTier::High,
            impact_radius: 2,
            symbol_footprint: vec![11],
            range_footprint: vec![(7, 7)],
            hard_constraints_touched: BTreeSet::from([SemanticInvariantClause::EditSafety]),
            zone: PolicyZone::Code,
            after_fingerprint: 1,
            style_gain: 1.0,
        };
        let result = GlobalConflictSolver::solve(
            &[candidate],
            &[],
            &PolicyCertainty {
                overall: 0.9,
                structural: 0.9,
                semantic: 0.9,
                ..Default::default()
            },
            RetryScopeStage::Full,
        );
        assert!(result.accepted.is_empty());
        assert!(result.hard_blocked_lines.contains(&7));
    }

    #[test]
    fn lower_risk_candidate_is_preferred_in_uncertain_region() {
        let low = PolicyEditCandidate {
            policy: "compact_declarations".into(),
            line: 12,
            confidence: 0.82,
            risk_tier: CandidateRiskTier::Low,
            impact_radius: 1,
            symbol_footprint: vec![55],
            range_footprint: vec![(12, 12)],
            hard_constraints_touched: BTreeSet::new(),
            zone: PolicyZone::Code,
            after_fingerprint: 2,
            style_gain: 1.1,
        };
        let high = PolicyEditCandidate {
            policy: "naming_conventions".into(),
            line: 12,
            confidence: 0.85,
            risk_tier: CandidateRiskTier::High,
            impact_radius: 2,
            symbol_footprint: vec![55],
            range_footprint: vec![(11, 13)],
            hard_constraints_touched: BTreeSet::new(),
            zone: PolicyZone::Code,
            after_fingerprint: 3,
            style_gain: 1.0,
        };
        let result = GlobalConflictSolver::solve(
            &[low.clone(), high],
            &[],
            &PolicyCertainty {
                overall: 0.52,
                structural: 0.70,
                semantic: 0.50,
                ..Default::default()
            },
            RetryScopeStage::LineLocal,
        );
        assert_eq!(result.accepted.len(), 1);
        assert_eq!(result.accepted[0].policy, low.policy);
    }

    #[test]
    fn exact_component_solver_prefers_global_utility_over_greedy_pick() {
        let high_single = PolicyEditCandidate {
            policy: "naming_conventions".into(),
            line: 20,
            confidence: 0.90,
            risk_tier: CandidateRiskTier::Low,
            impact_radius: 3,
            symbol_footprint: vec![1],
            range_footprint: vec![(20, 23)],
            hard_constraints_touched: BTreeSet::new(),
            zone: PolicyZone::Code,
            after_fingerprint: 11,
            style_gain: 2.0,
        };
        let left_pair = PolicyEditCandidate {
            policy: "compact_declarations".into(),
            line: 21,
            confidence: 0.90,
            risk_tier: CandidateRiskTier::Low,
            impact_radius: 1,
            symbol_footprint: vec![2],
            range_footprint: vec![(21, 21)],
            hard_constraints_touched: BTreeSet::new(),
            zone: PolicyZone::Code,
            after_fingerprint: 12,
            style_gain: 1.5,
        };
        let right_pair = PolicyEditCandidate {
            policy: "class_layout".into(),
            line: 23,
            confidence: 0.90,
            risk_tier: CandidateRiskTier::Low,
            impact_radius: 1,
            symbol_footprint: vec![3],
            range_footprint: vec![(23, 23)],
            hard_constraints_touched: BTreeSet::new(),
            zone: PolicyZone::Code,
            after_fingerprint: 13,
            style_gain: 1.5,
        };
        let result = GlobalConflictSolver::solve(
            &[high_single, left_pair.clone(), right_pair.clone()],
            &[],
            &PolicyCertainty {
                overall: 0.92,
                structural: 0.92,
                semantic: 0.92,
                ..Default::default()
            },
            RetryScopeStage::Full,
        );
        let accepted_policies = result
            .accepted
            .iter()
            .map(|candidate| candidate.policy.as_str())
            .collect::<BTreeSet<_>>();
        assert!(accepted_policies.contains(left_pair.policy.as_str()));
        assert!(accepted_policies.contains(right_pair.policy.as_str()));
        assert_eq!(result.accepted.len(), 2);
    }

    #[test]
    fn far_apart_structural_candidates_with_same_symbol_can_both_apply() {
        let left = PolicyEditCandidate {
            policy: "clang_format".into(),
            line: 20,
            confidence: 0.92,
            risk_tier: CandidateRiskTier::Medium,
            impact_radius: 2,
            symbol_footprint: vec![777],
            range_footprint: vec![(20, 20)],
            hard_constraints_touched: BTreeSet::new(),
            zone: PolicyZone::Code,
            after_fingerprint: 101,
            style_gain: 1.0,
        };
        let right = PolicyEditCandidate {
            policy: "class_layout".into(),
            line: 120,
            confidence: 0.91,
            risk_tier: CandidateRiskTier::Medium,
            impact_radius: 2,
            symbol_footprint: vec![777],
            range_footprint: vec![(120, 120)],
            hard_constraints_touched: BTreeSet::new(),
            zone: PolicyZone::Code,
            after_fingerprint: 202,
            style_gain: 1.0,
        };
        let result = GlobalConflictSolver::solve(
            &[left, right],
            &[],
            &PolicyCertainty {
                overall: 0.9,
                structural: 0.9,
                semantic: 0.9,
                ..Default::default()
            },
            RetryScopeStage::Full,
        );
        assert_eq!(result.accepted.len(), 2);
    }

    #[test]
    fn semantic_rewrite_candidates_from_same_policy_do_not_self_conflict() {
        let left = PolicyEditCandidate {
            policy: "naming_conventions".into(),
            line: 41,
            confidence: 0.96,
            risk_tier: CandidateRiskTier::High,
            impact_radius: 8,
            symbol_footprint: vec![99, 100],
            range_footprint: vec![(41, 120)],
            hard_constraints_touched: BTreeSet::new(),
            zone: PolicyZone::Code,
            after_fingerprint: 303,
            style_gain: 1.0,
        };
        let right = PolicyEditCandidate {
            policy: "naming_conventions".into(),
            line: 47,
            confidence: 0.96,
            risk_tier: CandidateRiskTier::High,
            impact_radius: 8,
            symbol_footprint: vec![99, 101],
            range_footprint: vec![(41, 120)],
            hard_constraints_touched: BTreeSet::new(),
            zone: PolicyZone::Code,
            after_fingerprint: 303,
            style_gain: 1.0,
        };
        let result = GlobalConflictSolver::solve(
            &[left, right],
            &[],
            &PolicyCertainty {
                overall: 0.9,
                structural: 0.9,
                semantic: 0.9,
                ..Default::default()
            },
            RetryScopeStage::Full,
        );
        assert_eq!(result.accepted.len(), 2);
        assert!(result.dropped_lines.is_empty());
    }

    #[test]
    fn same_policy_same_line_candidates_conflict() {
        let left = PolicyEditCandidate {
            policy: "naming_conventions".into(),
            line: 9,
            confidence: 0.92,
            risk_tier: CandidateRiskTier::High,
            impact_radius: 2,
            symbol_footprint: vec![44],
            range_footprint: vec![(9, 12)],
            hard_constraints_touched: BTreeSet::new(),
            zone: PolicyZone::Code,
            after_fingerprint: 1,
            style_gain: 1.0,
        };
        let right = PolicyEditCandidate {
            policy: "naming_conventions".into(),
            line: 9,
            confidence: 0.91,
            risk_tier: CandidateRiskTier::High,
            impact_radius: 2,
            symbol_footprint: vec![44],
            range_footprint: vec![(9, 12)],
            hard_constraints_touched: BTreeSet::new(),
            zone: PolicyZone::Code,
            after_fingerprint: 2,
            style_gain: 1.0,
        };
        let result = GlobalConflictSolver::solve(
            &[left, right],
            &[],
            &PolicyCertainty {
                overall: 0.9,
                structural: 0.9,
                semantic: 0.9,
                ..Default::default()
            },
            RetryScopeStage::Full,
        );
        assert_eq!(result.accepted.len(), 1);
    }
}
