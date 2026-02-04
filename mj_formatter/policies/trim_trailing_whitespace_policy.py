from __future__ import annotations

from ..core.edit import Edit
from ..core.parse_context import ParseContext
from ..core.policy_result import PolicyResult
from ..core.violation import Violation
from .policy_base import Policy


class TrimTrailingWhitespacePolicy(Policy):
    name = "trim_trailing_whitespace"
    description = "Remove trailing whitespace from each line"

    def apply(self, context: ParseContext) -> PolicyResult:
        lines = context.text.splitlines(keepends=True)
        violations: list[Violation] = []
        edits: list[Edit] = []
        changed = False

        for idx, line in enumerate(lines):
            line_body = line.rstrip("\r\n")
            ending = line[len(line_body):]
            trimmed = line_body.rstrip(" \t")
            if trimmed != line_body:
                changed = True
                lines[idx] = trimmed + ending
                violations.append(
                    Violation(
                        policy=self.name,
                        message="Trailing whitespace",
                        line=idx + 1,
                        column=len(trimmed) + 1,
                    )
                )
                edits.append(
                    Edit(
                        policy=self.name,
                        line=idx + 1,
                        before=line_body,
                        after=trimmed,
                    )
                )

        if not changed:
            return PolicyResult(text=context.text, violations=[], edits=[])

        return PolicyResult(text="".join(lines), violations=violations, edits=edits)
