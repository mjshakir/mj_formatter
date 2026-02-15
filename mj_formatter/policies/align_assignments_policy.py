from __future__ import annotations

from ..core.types import Edit
from ..core.types import ParseContext
from ..core.types import PolicyResult
from ..core.types import Violation
from .policy_base import Policy
import re


class AlignAssignmentsPolicy(Policy):
    name = "align_assignments"
    description = "Align consecutive assignments by '='"
    _operators = set("=<>!+-*/%&|^")

    def __init__(self, config: dict[str, object]) -> None:
        super().__init__(config)
        self._operator = self._required_str("operator")
        self._ignore_in = self._required_str_tuple("ignore_in")
        self._non_assignment_patterns = self._compile_required_patterns("non_assignment_patterns")

    def apply(self, context: ParseContext) -> PolicyResult:
        if "=" not in context.text:
            return PolicyResult(text=context.text, violations=[], edits=[])
        lines = context.text.splitlines(keepends=True)
        if not lines:
            return PolicyResult(text=context.text, violations=[], edits=[])

        groups = self._find_groups(lines=lines, operator=self._operator, ignore_in=self._ignore_in)
        if not groups:
            return PolicyResult(text=context.text, violations=[], edits=[])

        violations: list[Violation] = []
        edits: list[Edit] = []
        changed = False

        for group in groups:
            max_len = max(entry[2] for entry in group)
            for idx, line_body, left_len, assign_index in group:
                prefix = line_body[:assign_index].rstrip()
                suffix = line_body[assign_index + len(self._operator) :].lstrip()
                padding = " " * (max_len - len(prefix))
                rebuilt = f"{prefix}{padding} {self._operator} {suffix}"
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

    def _find_groups(
        self,
        *,
        lines: list[str],
        operator: str,
        ignore_in: tuple[str, ...],
    ) -> list[list[tuple[int, str, int, int]]]:
        groups: list[list[tuple[int, str, int, int]]] = []
        current: list[tuple[int, str, int, int]] = []

        for idx, line in enumerate(lines):
            line_body = line.rstrip("\r\n")
            if line_body.strip() == "":
                if len(current) >= 2:
                    groups.append(current)
                current = []
                continue
            if self._is_control_statement(line_body, ignore_in):
                if len(current) >= 2:
                    groups.append(current)
                current = []
                continue

            assign_index = self._find_assignment(line_body, operator)
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
        if self._is_non_assignment_context(code):
            return None
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

    def _is_non_assignment_context(self, code: str) -> bool:
        stripped = code.strip()
        if not stripped:
            return False
        for pattern in self._non_assignment_patterns:
            if pattern.search(stripped):
                return True
        return False

    def _is_control_statement(self, line: str, ignore_in: tuple[str, ...]) -> bool:
        stripped = line.lstrip()
        for keyword in ignore_in:
            if stripped.startswith(keyword + " "):
                return True
            if stripped.startswith(keyword + "("):
                return True
        return False

    def _required_str(self, key: str) -> str:
        value = self._config.get(key)
        if value is None:
            raise ValueError(f"align_assignments: missing required config key '{key}'")
        text = str(value).strip()
        if not text:
            raise ValueError(f"align_assignments: empty required config key '{key}'")
        return text

    def _required_str_tuple(self, key: str) -> tuple[str, ...]:
        value = self._config.get(key)
        if not isinstance(value, (list, tuple)):
            raise ValueError(f"align_assignments: missing required list config key '{key}'")
        items = tuple(str(item).strip() for item in value if str(item).strip())
        if not items:
            raise ValueError(f"align_assignments: config key '{key}' cannot be empty")
        return items

    def _compile_required_patterns(self, key: str) -> tuple[re.Pattern[str], ...]:
        value = self._config.get(key)
        if not isinstance(value, (list, tuple)):
            raise ValueError(f"align_assignments: missing required list config key '{key}'")
        patterns: list[re.Pattern[str]] = []
        for item in value:
            text = str(item).strip()
            if not text:
                continue
            patterns.append(re.compile(text))
        if not patterns:
            raise ValueError(f"align_assignments: config key '{key}' cannot be empty")
        return tuple(patterns)
