from __future__ import annotations

import re

from ..core.types import Edit
from ..core.types import ParseContext
from ..core.types import PolicyResult
from ..core.types import Violation
from .policy_base import Policy


class OperatorOverloadSpacingPolicy(Policy):
    name = "operator_overload_spacing"
    description = "Normalize spacing for operator overload names"

    _ops = [
        "<<=",
        ">>=",
        "->*",
        "==",
        "!=",
        "<=",
        ">=",
        "&&",
        "||",
        "<<",
        ">>",
        "+=",
        "-=",
        "*=",
        "/=",
        "%=",
        "&=",
        "|=",
        "^=",
        "->",
        "()",
        "[]",
        ",",
        "=",
        "+",
        "-",
        "*",
        "/",
        "%",
        "&",
        "|",
        "^",
        "<",
        ">",
        "!",
        "~",
    ]
    _pattern = re.compile(
        r"\boperator\s*(" + "|".join(re.escape(op) for op in _ops) + r")"
    )

    def apply(self, context: ParseContext) -> PolicyResult:
        lines = context.text.splitlines(keepends=True)
        violations: list[Violation] = []
        edits: list[Edit] = []
        changed = False

        for idx, line in enumerate(lines):
            line_body = line.rstrip("\r\n")
            ending = line[len(line_body):]
            if "operator" not in line_body:
                continue

            updated = self._pattern.sub(r"operator\1", line_body)
            if updated != line_body:
                changed = True
                lines[idx] = updated + ending
                violations.append(
                    Violation(
                        policy=self.name,
                        message="Normalize operator overload spacing",
                        line=idx + 1,
                        column=1,
                    )
                )
                edits.append(
                    Edit(
                        policy=self.name,
                        line=idx + 1,
                        before=line_body,
                        after=updated,
                    )
                )

        if not changed:
            return PolicyResult(text=context.text, violations=[], edits=[])

        return PolicyResult(text="".join(lines), violations=violations, edits=edits)
