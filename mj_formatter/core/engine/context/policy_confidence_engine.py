from __future__ import annotations

from collections.abc import Mapping, Sequence, Set
from statistics import fmean
from typing import Any

from ...types import AppConfig, ConfidenceGateDecision, Edit, PolicyDecisionOutcome, PolicyEnforcement


class PolicyConfidenceEngine:
    def __init__(self, config: AppConfig, policy_settings: Mapping[str, Mapping[str, object]]) -> None:
        self._enabled = bool(config.confidence_blocking_enabled)
        self._base_threshold = float(config.confidence_blocking_min)
        self._default_enforcement = PolicyEnforcement.from_value(config.confidence_default_enforcement)
        self._strict_delta = float(config.confidence_strict_delta)
        self._relaxed_delta = float(config.confidence_relaxed_delta)
        self._context_bonus_cap = float(config.confidence_context_bonus_cap)
        self._policy_settings = policy_settings
        self._profile_cache: dict[str, dict[str, object]] = {}

    def evaluate(
        self,
        *,
        policy_name: str,
        policy_parse_mode: str,
        code_context: Any | None,
        edits: Sequence[Edit],
        retry_attempt: int = 0,
        threshold_override: float | None = None,
        policy_filter: Set[str] | None = None,
    ) -> ConfidenceGateDecision | None:
        if not self._enabled or not edits:
            return None
        if policy_filter and policy_name not in policy_filter:
            return None
        if code_context is None:
            return None
        profile = self._confidence_profile(policy_name)

        base_enforcement = self._resolve_enforcement(policy_name)
        base_threshold = self._resolve_base_threshold(policy_name, threshold_override)
        raw_score = self._resolve_raw_score(code_context)
        score = min(1.0, raw_score + self._context_bonus(code_context, policy_parse_mode, profile))
        consensus = self._mean_consensus(code_context, fallback=score)
        effective_enforcement = self._effective_enforcement(
            base_enforcement=base_enforcement,
            score=score,
            consensus=consensus,
            retry_attempt=retry_attempt,
            code_context=code_context,
            profile=profile,
        )
        threshold = self._effective_threshold(base_threshold, effective_enforcement)
        reason_codes = self._reason_codes(
            base_enforcement=base_enforcement,
            effective_enforcement=effective_enforcement,
            score=score,
            threshold=threshold,
            consensus=consensus,
            retry_attempt=retry_attempt,
            profile=profile,
        )

        # Keep existing behavior for missing confidence scores; parser checks still protect correctness.
        if score <= 0.0:
            return None
        if effective_enforcement == PolicyEnforcement.MUST:
            return ConfidenceGateDecision(
                outcome=PolicyDecisionOutcome.APPLY,
                score=score,
                threshold=threshold,
                base_enforcement=base_enforcement,
                effective_enforcement=effective_enforcement,
                reason_codes=reason_codes,
                reason=(
                    f"policy decision apply for '{policy_name}' "
                    f"(must, score={score:.2f}, threshold={threshold:.2f})"
                ),
            )
        if score >= threshold:
            return ConfidenceGateDecision(
                outcome=PolicyDecisionOutcome.APPLY,
                score=score,
                threshold=threshold,
                base_enforcement=base_enforcement,
                effective_enforcement=effective_enforcement,
                reason_codes=reason_codes,
                reason=(
                    f"policy decision apply for '{policy_name}' "
                    f"(score={score:.2f}, threshold={threshold:.2f})"
                ),
            )

        dropped_lines = self._lines_to_drop_for_partial(
            edits=edits,
            code_context=code_context,
            score=score,
            threshold=threshold,
            effective_enforcement=effective_enforcement,
            retry_attempt=retry_attempt,
            profile=profile,
        )
        total_lines = len({int(item.line) for item in edits if int(item.line) > 0})
        drop_count = len(dropped_lines)

        outcome = PolicyDecisionOutcome.BLOCK
        if effective_enforcement == PolicyEnforcement.HARD:
            if total_lines > 0 and drop_count < total_lines:
                outcome = PolicyDecisionOutcome.APPLY_PARTIAL
            else:
                outcome = PolicyDecisionOutcome.BLOCK
        elif effective_enforcement == PolicyEnforcement.SOFT:
            if total_lines > 0 and drop_count < total_lines:
                outcome = PolicyDecisionOutcome.APPLY_PARTIAL
            else:
                outcome = PolicyDecisionOutcome.ADVISORY_ONLY
        elif effective_enforcement == PolicyEnforcement.ADVISORY:
            outcome = PolicyDecisionOutcome.ADVISORY_ONLY

        if outcome == PolicyDecisionOutcome.APPLY_PARTIAL and drop_count == 0:
            outcome = PolicyDecisionOutcome.APPLY
        if outcome == PolicyDecisionOutcome.APPLY_PARTIAL and total_lines <= 0:
            outcome = PolicyDecisionOutcome.BLOCK

        return ConfidenceGateDecision(
            outcome=outcome,
            score=score,
            threshold=threshold,
            base_enforcement=base_enforcement,
            effective_enforcement=effective_enforcement,
            reason_codes=reason_codes,
            reason=(
                f"policy decision {outcome.value} for '{policy_name}': "
                f"score={score:.2f}, threshold={threshold:.2f}, "
                f"base={base_enforcement.value}, effective={effective_enforcement.value}"
            ),
            dropped_lines=frozenset(dropped_lines),
        )

    def _resolve_enforcement(self, policy_name: str) -> PolicyEnforcement:
        settings = self._policy_settings.get(policy_name, {}) or {}
        raw = settings.get("enforcement", settings.get("confidence_enforcement", self._default_enforcement.value))
        return PolicyEnforcement.from_value(raw)

    def _resolve_base_threshold(self, policy_name: str, threshold_override: float | None) -> float:
        if threshold_override is not None:
            return max(0.0, min(1.0, float(threshold_override)))
        settings = self._policy_settings.get(policy_name, {}) or {}
        raw = settings.get("confidence_blocking_min")
        if raw is None:
            return max(0.0, min(1.0, self._base_threshold))
        try:
            return max(0.0, min(1.0, float(raw)))
        except Exception:
            return max(0.0, min(1.0, self._base_threshold))

    def _effective_threshold(self, base_threshold: float, enforcement: PolicyEnforcement) -> float:
        match enforcement:
            case PolicyEnforcement.MUST:
                return 0.0
            case PolicyEnforcement.HARD:
                return min(1.0, base_threshold + self._strict_delta)
            case PolicyEnforcement.SOFT:
                return base_threshold
            case PolicyEnforcement.ADVISORY:
                return max(0.0, base_threshold - self._relaxed_delta)
            case _:
                return base_threshold

    def _effective_enforcement(
        self,
        *,
        base_enforcement: PolicyEnforcement,
        score: float,
        consensus: float,
        retry_attempt: int,
        code_context: Any,
        profile: Mapping[str, object],
    ) -> PolicyEnforcement:
        if base_enforcement == PolicyEnforcement.MUST:
            return PolicyEnforcement.MUST
        levels = [
            PolicyEnforcement.ADVISORY,
            PolicyEnforcement.SOFT,
            PolicyEnforcement.HARD,
            PolicyEnforcement.MUST,
        ]
        index = levels.index(base_enforcement)
        shift = 0
        if score < self._profile_float(profile, "low_score_downgrade_threshold"):
            shift -= 1
        if (
            score > self._profile_float(profile, "high_score_upgrade_threshold")
            and consensus > self._profile_float(profile, "high_consensus_upgrade_threshold")
        ):
            shift += 1
        if consensus < self._profile_float(profile, "low_consensus_downgrade_threshold"):
            shift -= 1
        if retry_attempt > 0:
            shift -= 1
        project_consensus = getattr(code_context, "semantic_project_consensus_scores", {}) or {}
        if project_consensus:
            values = [float(item) for item in project_consensus.values()]
            if (
                values
                and fmean(values) > self._profile_float(profile, "project_consensus_upgrade_threshold")
                and score > self._profile_float(profile, "project_score_upgrade_threshold")
            ):
                shift += 1
        new_index = max(0, min(len(levels) - 1, index + shift))
        return levels[new_index]

    def _mean_consensus(self, code_context: Any, fallback: float) -> float:
        summary = float(getattr(code_context, "semantic_consensus_summary", 0.0) or 0.0)
        scores = getattr(code_context, "semantic_consensus_scores", {}) or {}
        if not scores and summary > 0.0:
            return max(0.0, min(1.0, summary))
        if not scores:
            return fallback
        values = [float(item) for item in scores.values()]
        if not values:
            return fallback
        mean_score = max(0.0, min(1.0, fmean(values)))
        if summary <= 0.0:
            return mean_score
        return max(0.0, min(1.0, (0.70 * mean_score) + (0.30 * summary)))

    def _reason_codes(
        self,
        *,
        base_enforcement: PolicyEnforcement,
        effective_enforcement: PolicyEnforcement,
        score: float,
        threshold: float,
        consensus: float,
        retry_attempt: int,
        profile: Mapping[str, object],
    ) -> tuple[str, ...]:
        codes: list[str] = []
        if base_enforcement != effective_enforcement:
            codes.append(f"tier_adjusted:{base_enforcement.value}->{effective_enforcement.value}")
        if score < threshold:
            codes.append("low_confidence")
        if consensus < self._profile_float(profile, "reason_low_consensus_threshold"):
            codes.append("low_consensus")
        if retry_attempt > 0:
            codes.append("retry_safety_harden")
        if not codes:
            codes.append("stable")
        return tuple(codes)

    def _lines_to_drop_for_partial(
        self,
        *,
        edits: Sequence[Edit],
        code_context: Any,
        score: float,
        threshold: float,
        effective_enforcement: PolicyEnforcement,
        retry_attempt: int,
        profile: Mapping[str, object],
    ) -> set[int]:
        if not edits:
            return set()
        risk_limit = self._profile_float(profile, "risk_limit_soft")
        if effective_enforcement == PolicyEnforcement.HARD:
            risk_limit = self._profile_float(profile, "risk_limit_hard")
        elif effective_enforcement == PolicyEnforcement.SOFT:
            risk_limit = self._profile_float(profile, "risk_limit_soft")
        elif effective_enforcement == PolicyEnforcement.ADVISORY:
            risk_limit = self._profile_float(profile, "risk_limit_advisory")

        # When confidence is far from threshold, keep only safer line edits.
        delta = max(0.0, threshold - score)
        risk_limit = max(
            self._profile_float(profile, "risk_limit_floor"),
            risk_limit - min(self._profile_float(profile, "delta_risk_penalty_cap"), delta),
        )
        if retry_attempt > 0:
            risk_limit = max(
                self._profile_float(profile, "risk_limit_floor"),
                risk_limit - self._profile_float(profile, "retry_risk_penalty"),
            )

        dropped: set[int] = set()
        for edit in edits:
            line = int(edit.line)
            if line <= 0:
                continue
            confidence = self._line_confidence(code_context, line, profile)
            risk = 1.0 - confidence
            if risk > risk_limit:
                dropped.add(line)
        return dropped

    def _line_confidence(self, code_context: Any, line: int, profile: Mapping[str, object]) -> float:
        index_score = self._line_confidence_from_index(code_context, line, profile)
        block_score = self._line_confidence_from_blocks(code_context, line, profile)
        if index_score is None and block_score is None:
            return self._profile_float(profile, "line_confidence_default")
        if index_score is None:
            return max(0.0, min(1.0, float(block_score)))
        if block_score is None:
            return max(0.0, min(1.0, float(index_score)))
        return max(
            0.0,
            min(
                1.0,
                (self._profile_float(profile, "line_index_weight") * float(index_score))
                + (self._profile_float(profile, "line_block_weight") * float(block_score)),
            ),
        )

    def _line_confidence_from_index(self, code_context: Any, line: int, profile: Mapping[str, object]) -> float | None:
        confidence_by_line = getattr(code_context, "semantic_line_confidence", {}) or {}
        if not confidence_by_line:
            return None
        if line in confidence_by_line:
            return max(0.0, min(1.0, float(confidence_by_line.get(line, 0.0))))

        best: float | None = None
        for delta in self._profile_int_tuple(profile, "line_neighbor_deltas"):
            for candidate in (line - delta, line + delta):
                value = confidence_by_line.get(candidate)
                if value is None:
                    continue
                adjusted = float(value) * (1.0 - (self._profile_float(profile, "line_neighbor_decay") * float(delta)))
                adjusted = max(0.0, min(1.0, adjusted))
                if best is None or adjusted > best:
                    best = adjusted
        return best

    def _line_confidence_from_blocks(self, code_context: Any, line: int, profile: Mapping[str, object]) -> float | None:
        blocks = getattr(code_context, "hybrid_blocks", ()) or ()
        if not blocks:
            return None
        best = self._profile_float(profile, "line_confidence_default")
        for block in blocks:
            open_line = int(getattr(block, "open_line", 0) or 0)
            close_line = int(getattr(block, "close_line", 0) or 0)
            if open_line <= 0 or close_line <= 0:
                continue
            if open_line <= line <= close_line:
                best = max(
                    best,
                    float(getattr(block, "confidence", self._profile_float(profile, "block_confidence_default")))
                    or self._profile_float(profile, "block_confidence_default"),
                )
        return max(0.0, min(1.0, best))

    def _context_bonus(self, code_context: Any, policy_parse_mode: str, profile: Mapping[str, object]) -> float:
        bonus = 0.0
        if policy_parse_mode == "clang" and getattr(code_context, "semantic_context", None) is not None:
            bonus += self._profile_float(profile, "context_bonus_clang")

        ref_counts = getattr(code_context, "semantic_reference_counts", {}) or {}
        total_refs = sum(max(0, int(value)) for value in ref_counts.values())
        if total_refs >= self._profile_int(profile, "context_refs_threshold_low"):
            bonus += self._profile_float(profile, "context_bonus_refs_low")
        if total_refs >= self._profile_int(profile, "context_refs_threshold_high"):
            bonus += self._profile_float(profile, "context_bonus_refs_high")

        scope_purity = getattr(code_context, "semantic_scope_purity", {}) or {}
        if scope_purity:
            values = [float(value) for value in scope_purity.values()]
            mean_scope = fmean(values) if values else 0.0
            if mean_scope >= self._profile_float(profile, "context_scope_threshold_low"):
                bonus += self._profile_float(profile, "context_bonus_scope_low")
            if mean_scope >= self._profile_float(profile, "context_scope_threshold_high"):
                bonus += self._profile_float(profile, "context_bonus_scope_high")
        hybrid_context = float(getattr(code_context, "hybrid_context_score", 0.0) or 0.0)
        if hybrid_context >= self._profile_float(profile, "context_hybrid_threshold"):
            bonus += self._profile_float(profile, "context_bonus_hybrid")
        coverage_score = float(getattr(code_context, "semantic_coverage_score", 0.0) or 0.0)
        if coverage_score >= self._profile_float(profile, "context_coverage_threshold"):
            bonus += self._profile_float(profile, "context_bonus_coverage")

        return min(self._context_bonus_cap, bonus)

    def _resolve_raw_score(self, code_context: Any) -> float:
        semantic_score = float(getattr(code_context, "semantic_hybrid_confidence", 0.0) or 0.0)
        context_score = float(getattr(code_context, "hybrid_context_score", 0.0) or 0.0)
        summary_score = float(getattr(code_context, "semantic_consensus_summary", 0.0) or 0.0)

        semantic_anchor = max(semantic_score, summary_score)
        if semantic_anchor <= 0.0:
            return max(0.0, min(1.0, max(context_score, summary_score)))
        if context_score <= 0.0:
            return max(0.0, min(1.0, semantic_anchor))
        blended = (0.70 * semantic_anchor) + (0.30 * context_score)
        return max(0.0, min(1.0, blended))

    def _confidence_profile(self, policy_name: str) -> Mapping[str, object]:
        cached = self._profile_cache.get(policy_name)
        if cached is not None:
            return cached
        settings = self._policy_settings.get(policy_name, {}) or {}
        profile = settings.get("confidence_profile")
        if not isinstance(profile, Mapping):
            raise ValueError(
                f"policy '{policy_name}' missing required 'confidence_profile' config for confidence engine"
            )
        profile_copy = {str(key): value for key, value in profile.items()}
        self._profile_cache[policy_name] = profile_copy
        return profile_copy

    def _profile_float(self, profile: Mapping[str, object], key: str) -> float:
        value = profile.get(key)
        if value is None:
            raise ValueError(f"confidence_profile missing required key '{key}'")
        try:
            return float(value)
        except Exception as exc:
            raise ValueError(f"confidence_profile key '{key}' must be float-compatible") from exc

    def _profile_int(self, profile: Mapping[str, object], key: str) -> int:
        value = profile.get(key)
        if value is None:
            raise ValueError(f"confidence_profile missing required key '{key}'")
        try:
            return int(value)
        except Exception as exc:
            raise ValueError(f"confidence_profile key '{key}' must be int-compatible") from exc

    def _profile_int_tuple(self, profile: Mapping[str, object], key: str) -> tuple[int, ...]:
        value = profile.get(key)
        if not isinstance(value, (list, tuple)):
            raise ValueError(f"confidence_profile key '{key}' must be a list of integers")
        items = tuple(int(item) for item in value)
        if not items:
            raise ValueError(f"confidence_profile key '{key}' cannot be empty")
        return items


__all__ = ["ConfidenceGateDecision", "PolicyConfidenceEngine"]
