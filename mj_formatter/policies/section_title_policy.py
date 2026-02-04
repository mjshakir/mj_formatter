from __future__ import annotations

import re

from ..core.edit import Edit
from ..core.parse_context import ParseContext
from ..core.policy_result import PolicyResult
from ..core.violation import Violation
from .policy_base import Policy


class SectionTitlePolicy(Policy):
    name = "section_title_normalizer"
    description = "Normalize section title comment text"

    def apply(self, context: ParseContext) -> PolicyResult:
        mapping = self._config.get("mapping", {})
        if not isinstance(mapping, dict):
            mapping = {}
        normalized_mapping = {str(k).lower(): str(v) for k, v in mapping.items()}

        pattern = re.compile(r"^(\s*)//\s*(.+?)\s*$")
        lines = context.text.splitlines(keepends=True)
        violations: list[Violation] = []
        edits: list[Edit] = []
        changed = False

        for idx, line in enumerate(lines):
            line_body = line.rstrip("\r\n")
            ending = line[len(line_body):]
            match = pattern.match(line_body)
            if not match:
                continue

            indent, comment = match.group(1), match.group(2)
            if not comment:
                continue
            if set(comment) <= {"-"}:
                continue

            key = comment.lower()
            if key not in normalized_mapping:
                continue

            target = normalized_mapping[key]
            normalized = f"{indent}// {target}"
            if normalized != line_body:
                changed = True
                lines[idx] = normalized + ending
                violations.append(
                    Violation(
                        policy=self.name,
                        message=f"Normalize section title to '{target}'",
                        line=idx + 1,
                        column=1,
                    )
                )
                edits.append(
                    Edit(
                        policy=self.name,
                        line=idx + 1,
                        before=line_body,
                        after=normalized,
                    )
                )

        if not changed:
            return PolicyResult(text=context.text, violations=[], edits=[])

        return PolicyResult(text="".join(lines), violations=violations, edits=edits)
