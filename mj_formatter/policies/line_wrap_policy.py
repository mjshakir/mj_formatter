from __future__ import annotations

from ..core.parse_context import ParseContext
from ..core.policy_result import PolicyResult
from ..core.violation import Violation
from .policy_base import Policy


class LineWrapPolicy(Policy):
    name = "line_wrap"
    description = "Wrap lines to a max column width"
    parse_mode = "text"

    def apply(self, context: ParseContext) -> PolicyResult:
        max_len = int(self._config.get("max_length", 100))
        violations: list[Violation] = []
        for idx, line in enumerate(context.text.splitlines(), 1):
            if len(line.rstrip("\n\r")) > max_len:
                violations.append(
                    Violation(
                        policy=self.name,
                        message=f"Line exceeds max length {max_len}",
                        line=idx,
                        column=max_len + 1,
                    )
                )
        return PolicyResult(text=context.text, violations=violations, edits=[])
