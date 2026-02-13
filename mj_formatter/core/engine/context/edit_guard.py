from __future__ import annotations

from dataclasses import dataclass
from enum import Enum
from typing import Any

from ...types import Edit, Violation


class TouchContract(str, Enum):
    ANY = "any"
    CODE_ONLY = "code_only"
    PREPROCESSOR_ONLY = "preprocessor_only"
    WHITESPACE_ONLY = "whitespace_only"

    @classmethod
    def from_value(cls, raw: object) -> "TouchContract":
        value = str(raw or cls.ANY.value).strip().lower()
        for item in cls:
            if item.value == value:
                return item
        return cls.ANY


@dataclass(frozen=True)
class _ProtectedLines:
    comments: set[int]
    strings: set[int]
    preprocessor: set[int]


class EditGuard:
    def validate(
        self,
        *,
        policy_name: str,
        contract: TouchContract,
        edits: list[Edit],
        parse_context: Any,
    ) -> list[Violation]:
        if not edits or contract == TouchContract.ANY:
            return []

        changed_lines = {int(edit.line) for edit in edits if int(edit.line) > 0}
        if not changed_lines:
            return []

        if contract == TouchContract.WHITESPACE_ONLY:
            violations = self._check_whitespace_only(policy_name=policy_name, edits=edits)
            if violations:
                return violations

        tree = getattr(parse_context, "tree_sitter_tree", None)
        protected = self._collect_protected_lines(tree=tree)

        violations: list[Violation] = []
        if contract == TouchContract.CODE_ONLY:
            blocked = sorted(
                line
                for line in changed_lines
                if line in protected.comments or line in protected.strings or line in protected.preprocessor
            )
            if blocked:
                violations.append(
                    Violation(
                        policy="edit_guard",
                        message=(
                            f"Blocked edits from '{policy_name}': code_only contract touched protected lines "
                            f"{self._line_preview(blocked)}"
                        ),
                        line=blocked[0],
                        column=1,
                    )
                )
        elif contract == TouchContract.PREPROCESSOR_ONLY:
            blocked = sorted(line for line in changed_lines if line not in protected.preprocessor)
            if blocked:
                violations.append(
                    Violation(
                        policy="edit_guard",
                        message=(
                            f"Blocked edits from '{policy_name}': preprocessor_only contract touched non-preprocessor "
                            f"lines {self._line_preview(blocked)}"
                        ),
                        line=blocked[0],
                        column=1,
                    )
                )
        return violations

    def _check_whitespace_only(self, *, policy_name: str, edits: list[Edit]) -> list[Violation]:
        for edit in edits:
            before = str(edit.before or "")
            after = str(edit.after or "")
            if before.strip() != after.strip():
                return [
                    Violation(
                        policy="edit_guard",
                        message=(
                            f"Blocked edits from '{policy_name}': whitespace_only contract changed non-whitespace "
                            f"content on line {edit.line}"
                        ),
                        line=int(edit.line),
                        column=1,
                    )
                ]
        return []

    def _collect_protected_lines(self, *, tree: Any | None) -> _ProtectedLines:
        if tree is None:
            return _ProtectedLines(comments=set(), strings=set(), preprocessor=set())

        root = getattr(tree, "root_node", None)
        if root is None:
            return _ProtectedLines(comments=set(), strings=set(), preprocessor=set())

        comments: set[int] = set()
        strings: set[int] = set()
        preprocessor: set[int] = set()

        string_like = {
            "string_literal",
            "raw_string_literal",
            "char_literal",
            "system_lib_string",
            "concatenated_string",
        }

        stack = [root]
        while stack:
            node = stack.pop()
            node_type = str(getattr(node, "type", "") or "")
            start_line = int(getattr(node, "start_point", (0, 0))[0]) + 1
            end_line = int(getattr(node, "end_point", (0, 0))[0]) + 1
            if end_line < start_line:
                end_line = start_line
            lines = range(start_line, end_line + 1)

            if node_type == "comment":
                comments.update(lines)
            elif node_type in string_like:
                strings.update(lines)
            elif node_type.startswith("preproc_") and node_type != "preproc_call":
                preprocessor.update(lines)

            stack.extend(reversed(getattr(node, "children", [])))

        return _ProtectedLines(comments=comments, strings=strings, preprocessor=preprocessor)

    def _line_preview(self, lines: list[int], limit: int = 6) -> str:
        if len(lines) <= limit:
            return ", ".join(str(item) for item in lines)
        head = ", ".join(str(item) for item in lines[:limit])
        return f"{head}, ..."
