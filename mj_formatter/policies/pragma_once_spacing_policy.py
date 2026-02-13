from __future__ import annotations

from ..core.types import ParseContext
from ..core.types import PolicyResult
from ..core.types import Violation
from .policy_base import Policy


class PragmaOnceSpacingPolicy(Policy):
    name = "pragma_once_spacing"
    description = "Ensure blank lines after #pragma once"

    def apply(self, context: ParseContext) -> PolicyResult:
        desired = int(self._config.get("blank_lines_after", 1))
        if desired < 0:
            return PolicyResult(text=context.text, violations=[], edits=[])

        line_ending = self._detect_line_ending(context.text)
        has_trailing_newline = context.text.endswith("\n")
        lines = context.text.splitlines()

        try:
            index = next(i for i, line in enumerate(lines) if line.strip() == "#pragma once")
        except StopIteration:
            return PolicyResult(text=context.text, violations=[], edits=[])

        blank_count = 0
        cursor = index + 1
        while cursor < len(lines) and lines[cursor].strip() == "":
            blank_count += 1
            cursor += 1

        if blank_count == desired:
            return PolicyResult(text=context.text, violations=[], edits=[])

        # Remove existing blank lines
        del lines[index + 1 : index + 1 + blank_count]
        # Insert desired blank lines
        for _ in range(desired):
            lines.insert(index + 1, "")

        updated = line_ending.join(lines)
        if has_trailing_newline:
            updated += line_ending

        violation = Violation(
            policy=self.name,
            message=f"Expected {desired} blank line(s) after #pragma once",
            line=index + 1,
            column=1,
        )

        return PolicyResult(text=updated, violations=[violation], edits=[])
