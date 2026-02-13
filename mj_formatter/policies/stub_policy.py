from __future__ import annotations

from ..core.types import ParseContext
from ..core.types import PolicyResult
from ..core.types import Violation
from ..core.utilities import warn_once
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
