from __future__ import annotations

import re

from ..core.edit import Edit
from ..core.parse_context import ParseContext
from ..core.policy_result import PolicyResult
from ..core.violation import Violation
from .policy_base import Policy


class DashCommentPolicy(Policy):
    name = "dash_comment_normalizer"
    description = "Normalize dashed comment separators to canonical lengths"

    def apply(self, context: ParseContext) -> PolicyResult:
        long_length = int(self._config.get("long_length", 64))
        short_length = int(self._config.get("short_length", 28))
        long_threshold = int(self._config.get("long_threshold", 50))
        mode = str(self._config.get("mode", "threshold")).lower()
        min_length = int(self._config.get("min_length", short_length))

        pattern = re.compile(r"^(\s*)//-+(\s*)$")
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

            current_len = len(line_body.strip())
            target_len = long_length if current_len >= long_threshold else short_length

            if mode == "auto":
                title_len = self._adjacent_title_length(lines, idx)
                if title_len is not None:
                    target_len = max(current_len, title_len, min_length)
            if target_len < 2:
                continue

            indent = match.group(1)
            normalized = indent + "//" + "-" * (target_len - 2)
            if normalized != line_body:
                changed = True
                lines[idx] = normalized + ending
                violations.append(
                    Violation(
                        policy=self.name,
                        message=f"Normalize dashed comment to length {target_len}",
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

    def _adjacent_title_length(self, lines: list[str], idx: int) -> int | None:
        for offset in (-1, 1):
            neighbor = idx + offset
            if neighbor < 0 or neighbor >= len(lines):
                continue
            line_body = lines[neighbor].rstrip("\r\n").strip()
            if not line_body.startswith("//"):
                continue
            if set(line_body.replace("//", "").strip()) <= {"-"}:
                continue
            return len(line_body)
        return None
