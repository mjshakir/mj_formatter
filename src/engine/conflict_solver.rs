use std::collections::{BTreeMap, BTreeSet};

use crate::engine::catalog::policy_catalog;
use crate::engine::edit_candidate::{CandidateRiskTier, PolicyEditCandidate};
use crate::engine::run_options::RetryScopeStage;
use crate::policy::zone::PolicyZone;
use crate::engine::semantic_contract::{SemanticContract, ALL_CLAUSES};

#[derive(Clone, Debug, Default)]
pub struct ConflictResult {
    pub accepted: Vec<PolicyEditCandidate>,
    pub dropped_lines: BTreeSet<usize>,
    pub hard_blocked_lines: BTreeSet<usize>,
}

struct ComponentSolveInput<'a> {
    admissible: &'a [PolicyEditCandidate],
    adjacency: &'a [Vec<usize>],
    component: &'a [usize],
    scope_stage: RetryScopeStage,
}

pub struct GlobalConflictSolver;

impl GlobalConflictSolver {
    pub fn solve(
        incoming: &[PolicyEditCandidate],
        already_selected: &[PolicyEditCandidate],
        scope_stage: RetryScopeStage,
        adaptive: &crate::engine::certainty_filter::CertaintyFilterState,
    ) -> ConflictResult {
        if incoming.is_empty() {
            return ConflictResult::default();
        }

        let consolidated = Self::consolidate_by_line(incoming, scope_stage, adaptive);
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
                .any(|existing| Self::conflicts(&candidate, existing, adaptive))
            {
                dropped_lines.insert(candidate.line);
                continue;
            }
            admissible.push(candidate);
        }

        let accepted = Self::select_conflict_optimal(admissible.as_slice(), scope_stage, adaptive);
        for candidate in &admissible {
            let conflicts = accepted
                .iter()
                .filter(|winner| Self::conflicts(candidate, winner, adaptive))
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

        ConflictResult {
            accepted,
            dropped_lines,
            hard_blocked_lines,
        }
    }

    fn consolidate_by_line(
        incoming: &[PolicyEditCandidate],
        scope_stage: RetryScopeStage,
        adaptive: &crate::engine::certainty_filter::CertaintyFilterState,
    ) -> Vec<PolicyEditCandidate> {
        let mut best_by_line = std::collections::HashMap::<usize, PolicyEditCandidate>::new();
        for candidate in incoming {
            match best_by_line.entry(candidate.line) {
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    let candidate_utility = Self::utility_score(candidate, scope_stage, adaptive);
                    let existing_utility = Self::utility_score(e.get(), scope_stage, adaptive);
                    let choose_new = candidate_utility > existing_utility + 0.000_001
                        || (candidate_utility - existing_utility).abs() <= 0.000_001
                            && candidate.confidence > e.get().confidence + 0.001
                        || (candidate_utility - existing_utility).abs() <= 0.000_001
                            && (candidate.confidence - e.get().confidence).abs() <= 0.001
                            && candidate.risk_tier.precedence_score()
                                > e.get().risk_tier.precedence_score();
                    if choose_new {
                        *e.get_mut() = candidate.clone();
                    }
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(candidate.clone());
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
        scope_stage: RetryScopeStage,
        adaptive: &crate::engine::certainty_filter::CertaintyFilterState,
    ) -> f64 {
        let risk_tier_index = match candidate.risk_tier {
            CandidateRiskTier::Low => 0,
            CandidateRiskTier::Medium => 1,
            CandidateRiskTier::High => 2,
        };
        let stabilizer_bonus = adaptive.stabilizer_bonus(risk_tier_index);
        let scope_narrowness = match scope_stage {
            RetryScopeStage::Full => 0.0,
            RetryScopeStage::CulpritRegion => 0.5,
            RetryScopeStage::NodeLocal | RetryScopeStage::LineLocal => 1.0,
        };
        let scope_bonus = scope_narrowness * adaptive.scope_bonus(risk_tier_index);
        let risk_penalty = adaptive.risk_penalty(risk_tier_index);
        let (range_weight, symbol_weight) = adaptive.footprint_weights();
        let footprint_penalty = ((candidate.range_footprint.len() as f64 * range_weight)
            + (candidate.symbol_footprint.len() as f64 * symbol_weight))
            .min(0.30);
        let style_weight = 1.0;
        let confidence_weight = 1.0;
        (candidate.style_gain * style_weight)
            + (candidate.confidence * confidence_weight)
            + stabilizer_bonus
            + scope_bonus
            - risk_penalty
            - footprint_penalty
    }

    fn violates_hard_constraints(candidate: &PolicyEditCandidate) -> bool {
        ALL_CLAUSES.iter()
            .filter(|&&clause| (candidate.hard_constraints_touched & clause.bit()) != 0)
            .any(|&clause| {
                SemanticContract::invariant_spec(clause)
                    .map(|spec| spec.hard)
                    .unwrap_or(true)
            })
    }

    fn conflicts(left: &PolicyEditCandidate, right: &PolicyEditCandidate, adaptive: &crate::engine::certainty_filter::CertaintyFilterState) -> bool {
        if left.policy == right.policy {
            return left.line == right.line;
        }
        let either_bypasses = {
            let catalog = policy_catalog();
            catalog.bypasses_line_conflict(left.policy.as_ref())
                || catalog.bypasses_line_conflict(right.policy.as_ref())
        };
        if either_bypasses {
            return false;
        }
        if left.line == right.line {
            return true;
        }
        if Self::ranges_overlap(
            left.range_footprint.as_ref(),
            right.range_footprint.as_ref(),
        ) {
            return true;
        }
        if Self::symbols_overlap(
            left.symbol_footprint.as_ref(),
            right.symbol_footprint.as_ref(),
        ) {
            let line_gap = left.line.abs_diff(right.line);
            let semantic_sensitive = Self::is_semantic_rewrite_policy(left.policy.as_ref())
                || Self::is_semantic_rewrite_policy(right.policy.as_ref())
                || left.risk_tier == CandidateRiskTier::High
                || right.risk_tier == CandidateRiskTier::High;
            let fuzzy_min = if semantic_sensitive {
                adaptive.fuzzy_min_semantic()
            } else {
                adaptive.fuzzy_min_other()
            };
            let neighborhood = left.impact_radius.max(right.impact_radius).max(fuzzy_min);
            if line_gap <= neighborhood {
                return true;
            }
        }
        (left.zone == PolicyZone::Preprocessor || right.zone == PolicyZone::Preprocessor)
            && left.after_fingerprint != right.after_fingerprint
            && (Self::ranges_overlap(
                left.range_footprint.as_ref(),
                right.range_footprint.as_ref(),
            ) || left.line.abs_diff(right.line) <= left.impact_radius.max(right.impact_radius))
    }

    fn is_semantic_rewrite_policy(policy: &str) -> bool {
        policy_catalog().is_semantic_rewrite(policy)
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
        scope_stage: RetryScopeStage,
        adaptive: &crate::engine::certainty_filter::CertaintyFilterState,
    ) -> Vec<PolicyEditCandidate> {
        if admissible.is_empty() {
            return Vec::new();
        }
        let adjacency = Self::build_conflict_adjacency(admissible, adaptive);
        let components = Self::connected_components(&adjacency);
        let mut accepted_indexes = Vec::<usize>::new();
        for component in components {
            let component_threshold = adaptive.component_threshold();
            let solve_input = ComponentSolveInput {
                admissible,
                adjacency: &adjacency,
                component: &component,
                scope_stage,
            };
            let selected = if component.len() <= component_threshold {
                Self::solve_component_exact(&solve_input, adaptive)
            } else {
                Self::solve_component_greedy(&solve_input, adaptive)
            };
            accepted_indexes.extend(selected);
        }
        accepted_indexes.sort_unstable();
        accepted_indexes
            .into_iter()
            .filter_map(|index| admissible.get(index).cloned())
            .collect()
    }

    fn build_conflict_adjacency(admissible: &[PolicyEditCandidate], adaptive: &crate::engine::certainty_filter::CertaintyFilterState) -> Vec<Vec<usize>> {
        let mut adjacency = vec![Vec::<usize>::new(); admissible.len()];
        for left in 0..admissible.len() {
            for right in (left + 1)..admissible.len() {
                if Self::conflicts(&admissible[left], &admissible[right], adaptive) {
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

    fn solve_component_exact(input: &ComponentSolveInput<'_>, adaptive: &crate::engine::certainty_filter::CertaintyFilterState) -> Vec<usize> {
        let admissible = input.admissible;
        let adjacency = input.adjacency;
        let component = input.component;
        let scope_stage = input.scope_stage;
        let mut ordered = component.to_vec();
        ordered.sort_by(|left, right| {
            let left_candidate = &admissible[*left];
            let right_candidate = &admissible[*right];
            let left_utility = Self::utility_score(left_candidate, scope_stage, adaptive);
            let right_utility = Self::utility_score(right_candidate, scope_stage, adaptive);
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
            .map(|index| Self::utility_score(&admissible[*index], scope_stage, adaptive).max(0.0))
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

    fn solve_component_greedy(input: &ComponentSolveInput<'_>, adaptive: &crate::engine::certainty_filter::CertaintyFilterState) -> Vec<usize> {
        let admissible = input.admissible;
        let adjacency = input.adjacency;
        let component = input.component;
        let scope_stage = input.scope_stage;
        let mut ordered = component.to_vec();
        ordered.sort_by(|left, right| {
            let left_candidate = &admissible[*left];
            let right_candidate = &admissible[*right];
            let left_utility = Self::utility_score(left_candidate, scope_stage, adaptive);
            let right_utility = Self::utility_score(right_candidate, scope_stage, adaptive);
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

    use crate::engine::certainty_filter::CertaintyFilterState;
    use crate::engine::conflict_solver::GlobalConflictSolver;
    use crate::engine::edit_candidate::{CandidateRiskTier, PolicyEditCandidate};
    use crate::engine::run_options::RetryScopeStage;
    use crate::policy::zone::PolicyZone;
    use crate::engine::semantic_contract::SemanticInvariantClause;

    #[test]
    fn hard_constraint_rejected() {
        let candidate = PolicyEditCandidate {
            policy: "naming_conventions".into(),
            line: 7,
            confidence: 0.9,
            risk_tier: CandidateRiskTier::High,
            impact_radius: 2,
            symbol_footprint: vec![11u64].into(),
            range_footprint: vec![(7usize, 7usize)].into(),
            hard_constraints_touched: SemanticInvariantClause::EditSafety.bit(),
            zone: PolicyZone::Code,
            after_fingerprint: 1,
            style_gain: 1.0,
        };
        let adaptive = CertaintyFilterState::new();
        let result = GlobalConflictSolver::solve(
            &[candidate],
            &[],
            RetryScopeStage::Full,
            &adaptive,
        );
        assert!(result.accepted.is_empty());
        assert!(result.hard_blocked_lines.contains(&7));
    }

    #[test]
    fn prefers_risk_uncertain() {
        let low = PolicyEditCandidate {
            policy: "compact_declarations".into(),
            line: 12,
            confidence: 0.82,
            risk_tier: CandidateRiskTier::Low,
            impact_radius: 1,
            symbol_footprint: vec![55u64].into(),
            range_footprint: vec![(12usize, 12usize)].into(),
            hard_constraints_touched: 0,
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
            symbol_footprint: vec![55u64].into(),
            range_footprint: vec![(11usize, 13usize)].into(),
            hard_constraints_touched: 0,
            zone: PolicyZone::Code,
            after_fingerprint: 3,
            style_gain: 1.0,
        };
        let adaptive = CertaintyFilterState::new();
        let result = GlobalConflictSolver::solve(
            &[low.clone(), high],
            &[],
            RetryScopeStage::LineLocal,
            &adaptive,
        );
        assert_eq!(result.accepted.len(), 1);
        assert_eq!(result.accepted[0].policy, low.policy);
    }

    #[test]
    fn solver_global_utility() {
        let high_single = PolicyEditCandidate {
            policy: "naming_conventions".into(),
            line: 20,
            confidence: 0.90,
            risk_tier: CandidateRiskTier::Low,
            impact_radius: 3,
            symbol_footprint: vec![1u64].into(),
            range_footprint: vec![(20usize, 23usize)].into(),
            hard_constraints_touched: 0,
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
            symbol_footprint: vec![2u64].into(),
            range_footprint: vec![(21usize, 21usize)].into(),
            hard_constraints_touched: 0,
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
            symbol_footprint: vec![3u64].into(),
            range_footprint: vec![(23usize, 23usize)].into(),
            hard_constraints_touched: 0,
            zone: PolicyZone::Code,
            after_fingerprint: 13,
            style_gain: 1.5,
        };
        let adaptive = CertaintyFilterState::new();
        let result = GlobalConflictSolver::solve(
            &[high_single, left_pair.clone(), right_pair.clone()],
            &[],
            RetryScopeStage::Full,
            &adaptive,
        );
        let accepted_policies = result
            .accepted
            .iter()
            .map(|candidate| candidate.policy.as_ref())
            .collect::<BTreeSet<_>>();
        assert!(accepted_policies.contains(left_pair.policy.as_ref()));
        assert!(accepted_policies.contains(right_pair.policy.as_ref()));
        assert_eq!(result.accepted.len(), 2);
    }

    #[test]
    fn far_apart_coexist() {
        let left = PolicyEditCandidate {
            policy: "clang_format".into(),
            line: 20,
            confidence: 0.92,
            risk_tier: CandidateRiskTier::Medium,
            impact_radius: 2,
            symbol_footprint: vec![777u64].into(),
            range_footprint: vec![(20usize, 20usize)].into(),
            hard_constraints_touched: 0,
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
            symbol_footprint: vec![777u64].into(),
            range_footprint: vec![(120usize, 120usize)].into(),
            hard_constraints_touched: 0,
            zone: PolicyZone::Code,
            after_fingerprint: 202,
            style_gain: 1.0,
        };
        let adaptive = CertaintyFilterState::new();
        let result = GlobalConflictSolver::solve(
            &[left, right],
            &[],
            RetryScopeStage::Full,
            &adaptive,
        );
        assert_eq!(result.accepted.len(), 2);
    }

    #[test]
    fn rewrite_no_selfconflict() {
        let left = PolicyEditCandidate {
            policy: "naming_conventions".into(),
            line: 41,
            confidence: 0.96,
            risk_tier: CandidateRiskTier::High,
            impact_radius: 8,
            symbol_footprint: vec![99u64, 100u64].into(),
            range_footprint: vec![(41usize, 120usize)].into(),
            hard_constraints_touched: 0,
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
            symbol_footprint: vec![99u64, 101u64].into(),
            range_footprint: vec![(41usize, 120usize)].into(),
            hard_constraints_touched: 0,
            zone: PolicyZone::Code,
            after_fingerprint: 303,
            style_gain: 1.0,
        };
        let adaptive = CertaintyFilterState::new();
        let result = GlobalConflictSolver::solve(
            &[left, right],
            &[],
            RetryScopeStage::Full,
            &adaptive,
        );
        assert_eq!(result.accepted.len(), 2);
        assert!(result.dropped_lines.is_empty());
    }

    #[test]
    fn same_line_conflicts() {
        let left = PolicyEditCandidate {
            policy: "naming_conventions".into(),
            line: 9,
            confidence: 0.92,
            risk_tier: CandidateRiskTier::High,
            impact_radius: 2,
            symbol_footprint: vec![44u64].into(),
            range_footprint: vec![(9usize, 12usize)].into(),
            hard_constraints_touched: 0,
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
            symbol_footprint: vec![44u64].into(),
            range_footprint: vec![(9usize, 12usize)].into(),
            hard_constraints_touched: 0,
            zone: PolicyZone::Code,
            after_fingerprint: 2,
            style_gain: 1.0,
        };
        let adaptive = CertaintyFilterState::new();
        let result = GlobalConflictSolver::solve(
            &[left, right],
            &[],
            RetryScopeStage::Full,
            &adaptive,
        );
        assert_eq!(result.accepted.len(), 1);
    }
}
