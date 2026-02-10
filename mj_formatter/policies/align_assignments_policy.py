from __future__ import annotations

from ..core.edit import Edit
from ..core.parse_context import ParseContext
from ..core.policy_result import PolicyResult
from ..core.violation import Violation
from .policy_base import Policy
from dataclasses import dataclass


class AlignAssignmentsPolicy(Policy):
    name = "align_assignments"
    description = "Align consecutive assignments by '='"
    _operators = set("=<>!+-*/%&|^")
    _default_ignore = ("for", "if", "while", "switch")

    def apply(self, context: ParseContext) -> PolicyResult:
        if "=" not in context.text:
            return PolicyResult(text=context.text, violations=[], edits=[])
        lines = context.text.splitlines(keepends=True)
        if not lines:
            return PolicyResult(text=context.text, violations=[], edits=[])

        operator = str(self._config.get("operator", "="))
        ignore_in = tuple(self._config.get("ignore_in", self._default_ignore) or self._default_ignore)
        groups = self._find_groups(
            AlignAssignmentsPolicy.FindGroupsArgs(
                lines=lines,
                operator=operator,
                ignore_in=ignore_in,
            )
        )
        if not groups:
            return PolicyResult(text=context.text, violations=[], edits=[])

        violations: list[Violation] = []
        edits: list[Edit] = []
        changed = False

        for group in groups:
            max_len = max(entry[2] for entry in group)
            for idx, line_body, left_len, assign_index in group:
                prefix = line_body[:assign_index].rstrip()
                suffix = line_body[assign_index + len(operator) :].lstrip()
                padding = " " * (max_len - len(prefix))
                rebuilt = f"{prefix}{padding} {operator} {suffix}"
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

    @dataclass(frozen=True)
    class FindGroupsArgs:
        lines: list[str]
        operator: str
        ignore_in: tuple[str, ...]

    def _find_groups(self, args: "AlignAssignmentsPolicy.FindGroupsArgs") -> list[list[tuple[int, str, int, int]]]:
        groups: list[list[tuple[int, str, int, int]]] = []
        current: list[tuple[int, str, int, int]] = []

        for idx, line in enumerate(args.lines):
            line_body = line.rstrip("\r\n")
            if line_body.strip() == "":
                if len(current) >= 2:
                    groups.append(current)
                current = []
                continue
            if self._is_control_statement(line_body, args.ignore_in):
                if len(current) >= 2:
                    groups.append(current)
                current = []
                continue

            assign_index = self._find_assignment(line_body, args.operator)
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

    def _find_assignment(self, line: str, operator: str) -> int | None:
        code = line.split("//", 1)[0]
        for idx in range(len(code)):
            if not code.startswith(operator, idx):
                continue
            prev = code[idx - 1] if idx > 0 else ""
            nxt = code[idx + len(operator)] if idx + len(operator) < len(code) else ""
            if prev in self._operators or nxt in self._operators:
                continue
            prefix = code[:idx].rstrip()
            if prefix.endswith("operator"):
                continue
            return idx
        return None

    def _is_control_statement(self, line: str, ignore_in: tuple[str, ...]) -> bool:
        stripped = line.lstrip()
        for keyword in ignore_in:
            if stripped.startswith(keyword + " "):
                return True
            if stripped.startswith(keyword + "("):
                return True
        return False
