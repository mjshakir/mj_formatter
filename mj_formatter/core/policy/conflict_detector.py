from __future__ import annotations

from ..types import ConflictDetectorConfig, Edit, Violation


class PolicyConflictDetector:
    def __init__(self, config: ConflictDetectorConfig | None = None) -> None:
        self._config = config or ConflictDetectorConfig()
        self._line_history: dict[int, list[Edit]] = {}
        self._reported_reverts: set[tuple[int, str, str]] = set()
        self._reported_touches: set[tuple[int, tuple[str, ...]]] = set()

    def observe(self, policy_name: str, edits: list[Edit]) -> list[Violation]:
        if not self._config.enabled or not edits:
            return []
        violations: list[Violation] = []
        for edit in edits:
            history = self._line_history.setdefault(int(edit.line), [])

            if history:
                prev = history[-1]
                if prev.policy != policy_name and edit.after == prev.before and edit.before == prev.after:
                    key = (int(edit.line), prev.policy, policy_name)
                    if key not in self._reported_reverts:
                        self._reported_reverts.add(key)
                        violations.append(
                            Violation(
                                policy="policy_conflict_detector",
                                message=(
                                    f"Line {edit.line}: policy '{policy_name}' appears to revert "
                                    f"policy '{prev.policy}'"
                                ),
                                line=edit.line,
                                column=1,
                            )
                        )

                touched = sorted({item.policy for item in history} | {policy_name})
                if len(touched) >= self._config.touch_threshold:
                    touch_key = (int(edit.line), tuple(touched))
                    if touch_key not in self._reported_touches:
                        self._reported_touches.add(touch_key)
                        violations.append(
                            Violation(
                                policy="policy_conflict_detector",
                                message=(
                                    f"Line {edit.line}: touched by multiple policies ({', '.join(touched)}), "
                                    "verify style rule compatibility"
                                ),
                                line=edit.line,
                                column=1,
                            )
                        )

            history.append(edit)
        return violations
