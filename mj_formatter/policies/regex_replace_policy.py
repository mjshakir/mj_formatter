from __future__ import annotations

import re

from ..core.types import Edit
from ..core.types import ParseContext
from ..core.types import PolicyResult
from ..core.types import Violation
from .policy_base import Policy


class RegexReplacePolicy(Policy):
    name = "regex_replace"
    description = "Apply regex replacements"
    parse_mode = "tree_sitter"

    def apply(self, context: ParseContext) -> PolicyResult:
        rules = self._config.get("rules", [])
        if not isinstance(rules, list) or not rules:
            return PolicyResult(text=context.text, violations=[], edits=[])

        text = context.text
        violations: list[Violation] = []
        edits: list[Edit] = []
        changed = False

        for rule in rules:
            if not isinstance(rule, dict):
                continue
            pattern = str(rule.get("pattern", ""))
            repl = str(rule.get("replace", ""))
            if not pattern:
                continue
            flags = self._parse_flags(rule.get("flags"))
            regex = re.compile(pattern, flags=flags)
            new_text, count = regex.subn(repl, text)
            if count:
                changed = True
                text = new_text
                violations.append(
                    Violation(
                        policy=self.name,
                        message=f"Regex replace: {pattern}",
                        line=1,
                        column=1,
                    )
                )

        if not changed:
            return PolicyResult(text=context.text, violations=[], edits=[])

        edits = self._diff_edits(context.text, text)
        return PolicyResult(text=text, violations=violations, edits=edits)

    def _parse_flags(self, flags: object) -> int:
        if not flags:
            return 0
        if isinstance(flags, int):
            return flags
        value = 0
        for raw in str(flags).split("|"):
            part = raw.strip().upper()
            if part == "IGNORECASE":
                value |= re.IGNORECASE
            elif part == "MULTILINE":
                value |= re.MULTILINE
            elif part == "DOTALL":
                value |= re.DOTALL
        return value

    def _diff_edits(self, before: str, after: str) -> list[Edit]:
        before_lines = before.splitlines()
        after_lines = after.splitlines()
        edits: list[Edit] = []
        for idx, (b, a) in enumerate(zip(before_lines, after_lines)):
            if b != a:
                edits.append(Edit(policy=self.name, line=idx + 1, before=b, after=a))
        if len(after_lines) > len(before_lines):
            for idx in range(len(before_lines), len(after_lines)):
                edits.append(Edit(policy=self.name, line=idx + 1, before="", after=after_lines[idx]))
        return edits
