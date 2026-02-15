from __future__ import annotations

from collections.abc import Mapping

from ...engine.context import EditGuard, PolicyConfidenceEngine, TouchContract
from ...types import (
    AppConfig,
    Edit,
    ParseContext,
    PolicyDecisionOutcome,
    PolicyResult,
    Violation,
)
from ....policies.policy_base import Policy


class PolicyRuntime:
    def __init__(
        self,
        *,
        config: AppConfig,
        policy_settings: Mapping[str, Mapping[str, object]],
        edit_guard: EditGuard,
        confidence_engine: PolicyConfidenceEngine,
    ) -> None:
        self._config = config
        self._policy_settings = policy_settings
        self._edit_guard = edit_guard
        self._confidence_engine = confidence_engine

    def guard_policy_edits(
        self,
        *,
        policy: Policy,
        context: ParseContext,
        edits: list[Edit],
    ) -> list[Violation]:
        contract = self._policy_touch_contract(policy)
        if contract == TouchContract.ANY or not edits:
            return []
        if context.tree_sitter_tree is None:
            return [
                Violation(
                    policy="edit_guard",
                    message=(
                        f"Blocked edits from '{policy.name}': touch contract '{contract.value}' "
                        "requires tree-sitter context"
                    ),
                    line=int(edits[0].line),
                    column=1,
                )
            ]
        return self._edit_guard.validate(
            policy_name=policy.name,
            contract=contract,
            edits=edits,
            parse_context=context,
        )

    def apply_confidence_decision(
        self,
        *,
        policy: Policy,
        context: ParseContext,
        result: PolicyResult,
        before_text: str,
        retry_attempt: int,
        threshold_override: float,
        policy_filter: set[str],
    ) -> PolicyResult | None:
        if not self._config.confidence_blocking_enabled:
            return None
        if not result.edits:
            return None
        blocked_policies = self._config.confidence_blocking_policies if policy_filter is None else policy_filter
        decision = self._confidence_engine.evaluate(
            policy_name=policy.name,
            policy_parse_mode=policy.parse_mode,
            code_context=getattr(context, "code_context", None),
            edits=result.edits,
            retry_attempt=retry_attempt,
            threshold_override=threshold_override,
            policy_filter=blocked_policies,
        )
        if decision is None:
            return None
        if decision.outcome == PolicyDecisionOutcome.APPLY:
            if decision.base_enforcement != decision.effective_enforcement:
                return PolicyResult(
                    text=result.text,
                    violations=[
                        *result.violations,
                        Violation(
                            policy="confidence_guard",
                            message=(
                                f"Adaptive tier for '{policy.name}': "
                                f"{decision.base_enforcement.value}->{decision.effective_enforcement.value} "
                                f"({', '.join(decision.reason_codes)})"
                            ),
                            line=int(result.edits[0].line),
                            column=1,
                        ),
                    ],
                    edits=result.edits,
                    profile=result.profile,
                    parse_modes=result.parse_modes,
                    warnings=result.warnings,
                )
            return None

        if decision.outcome == PolicyDecisionOutcome.APPLY_PARTIAL:
            suppressed = self.apply_line_suppression(before_text, result, set(decision.dropped_lines))
            dropped_count = len(decision.dropped_lines)
            return PolicyResult(
                text=suppressed.text,
                violations=[
                    *suppressed.violations,
                    Violation(
                        policy="confidence_guard",
                        message=(
                            f"Adaptive partial apply for '{policy.name}' "
                            f"(dropped_lines={dropped_count}, score={decision.score:.2f}, "
                            f"threshold={decision.threshold:.2f}, reasons={','.join(decision.reason_codes)})"
                        ),
                        line=int(result.edits[0].line),
                        column=1,
                    ),
                ],
                edits=suppressed.edits,
                profile=suppressed.profile,
                parse_modes=suppressed.parse_modes,
                warnings=suppressed.warnings,
            )

        if decision.outcome in {PolicyDecisionOutcome.BLOCK, PolicyDecisionOutcome.ADVISORY_ONLY}:
            mode_label = "blocked" if decision.outcome == PolicyDecisionOutcome.BLOCK else "advisory-only"
            return PolicyResult(
                text=before_text,
                violations=[
                    *result.violations,
                    Violation(
                        policy="confidence_guard",
                        message=(
                            f"Adaptive decision {mode_label} for '{policy.name}' "
                            f"(score={decision.score:.2f}, threshold={decision.threshold:.2f}, "
                            f"effective={decision.effective_enforcement.value}, "
                            f"reasons={','.join(decision.reason_codes)})"
                        ),
                        line=int(result.edits[0].line),
                        column=1,
                    ),
                ],
                edits=[],
                profile=result.profile,
                parse_modes=result.parse_modes,
                warnings=result.warnings,
            )

        return None

    def _policy_touch_contract(self, policy: Policy) -> TouchContract:
        settings = self._policy_settings.get(policy.name, {})
        if "touch_contract" not in settings:
            raise ValueError(
                f"policy '{policy.name}' missing required 'touch_contract' in policy config"
            )
        raw = settings.get("touch_contract")
        return TouchContract.from_value(raw)

    @staticmethod
    def apply_line_suppression(before_text: str, result: PolicyResult, disabled_lines: set[int]) -> PolicyResult:
        if not disabled_lines:
            return result
        kept_violations = [item for item in result.violations if int(item.line) not in disabled_lines]
        kept_edits = [item for item in result.edits if int(item.line) not in disabled_lines]
        if len(kept_edits) == len(result.edits):
            return PolicyResult(
                text=result.text,
                violations=kept_violations,
                edits=kept_edits,
                profile=result.profile,
                parse_modes=result.parse_modes,
                warnings=result.warnings,
            )

        before_lines = before_text.splitlines(keepends=True)
        after_lines = result.text.splitlines(keepends=True)
        max_count = min(len(before_lines), len(after_lines))
        for line_no in disabled_lines:
            idx = int(line_no) - 1
            if 0 <= idx < max_count:
                after_lines[idx] = before_lines[idx]
        text = "".join(after_lines)
        return PolicyResult(
            text=text,
            violations=kept_violations,
            edits=kept_edits,
            profile=result.profile,
            parse_modes=result.parse_modes,
            warnings=result.warnings,
        )


__all__ = ["PolicyRuntime"]
