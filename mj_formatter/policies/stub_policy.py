from __future__ import annotations

from ..core.parse_context import ParseContext
from ..core.policy_result import PolicyResult
from ..core.violation import Violation
from ..utils.warn_once import warn_once
from .policy_base import Policy


class StubPolicy(Policy):
    warning_message = "Policy not implemented"

    def apply(self, context: ParseContext) -> PolicyResult:
        warn_once(self.name, f"{self.name}: {self.warning_message}")
        if self._config.get("report_unimplemented", False):
            violation = Violation(
                policy=self.name,
                message=self.warning_message,
                line=1,
                column=1,
            )
            return PolicyResult(text=context.text, violations=[violation], edits=[])
        return PolicyResult(text=context.text, violations=[], edits=[])
