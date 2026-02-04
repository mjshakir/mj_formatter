from __future__ import annotations

from ..core.edit import Edit
from ..core.parse_context import ParseContext
from ..core.policy_result import PolicyResult
from ..core.violation import Violation
from .policy_base import Policy


class AlignAssignmentsPolicy(Policy):
    name = "align_assignments"
    description = "Align consecutive assignments by '='"

    def apply(self, context: ParseContext) -> PolicyResult:
        lines = context.text.splitlines(keepends=True)
        if not lines:
            return PolicyResult(text=context.text, violations=[], edits=[])

        groups = self._find_groups(lines)
        if not groups:
            return PolicyResult(text=context.text, violations=[], edits=[])

        violations: list[Violation] = []
        edits: list[Edit] = []
        changed = False

        for group in groups:
            max_len = max(entry[2] for entry in group)
            for idx, line_body, left_len, assign_index in group:
                prefix = line_body[:assign_index].rstrip()
                suffix = line_body[assign_index + 1 :].lstrip()
                padding = " " * (max_len - len(prefix))
                rebuilt = f"{prefix}{padding} = {suffix}"
                if rebuilt != line_body:
                    changed = True
                    ending = lines[idx][len(line_body) :]
                    lines[idx] = rebuilt + ending
                    violations.append(
                        Violation(
                            policy=self.name,
                            message="Align assignments",
                            line=idx + 1,
                            column=len(prefix) + 1,
                        )
                    )
                    edits.append(
                        Edit(
                            policy=self.name,
                            line=idx + 1,
                            before=line_body,
                            after=rebuilt,
                        )
                    )

        if not changed:
            return PolicyResult(text=context.text, violations=[], edits=[])

        return PolicyResult(text="".join(lines), violations=violations, edits=edits)

    def _find_groups(self, lines: list[str]) -> list[list[tuple[int, str, int, int]]]:
        groups: list[list[tuple[int, str, int, int]]] = []
        current: list[tuple[int, str, int, int]] = []

        for idx, line in enumerate(lines):
            line_body = line.rstrip("\r\n")
            if line_body.strip() == "":
                if len(current) >= 2:
                    groups.append(current)
                current = []
                continue

            assign_index = self._find_assignment(line_body)
            if assign_index is None:
                if len(current) >= 2:
                    groups.append(current)
                current = []
                continue

            prefix = line_body[:assign_index].rstrip()
            current.append((idx, line_body, len(prefix), assign_index))

        if len(current) >= 2:
            groups.append(current)

        return groups

    def _find_assignment(self, line: str) -> int | None:
        code = line.split("//", 1)[0]
        operators = set("=<>!+-*/%&|^")
        for idx, ch in enumerate(code):
            if ch != "=":
                continue
            prev = code[idx - 1] if idx > 0 else ""
            nxt = code[idx + 1] if idx + 1 < len(code) else ""
            if prev in operators or nxt in operators:
                continue
            prefix = code[:idx].rstrip()
            if prefix.endswith("operator"):
                continue
            return idx
        return None
