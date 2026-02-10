from __future__ import annotations

import re
from pathlib import Path

from ..core.edit import Edit
from ..core.parse_context import ParseContext
from ..core.policy_result import PolicyResult
from ..core.violation import Violation
from .policy_base import Policy


class IncludeGuardPolicy(Policy):
    name = "include_guards"
    description = "Ensure include guards or #pragma once"
    parse_mode = "text"

    _pragma_re = re.compile(r"^\s*#\s*pragma\s+once\b", re.IGNORECASE)
    _ifndef_re = re.compile(r"^\s*#\s*ifndef\s+([A-Za-z_]\w*)")
    _define_re = re.compile(r"^\s*#\s*define\s+([A-Za-z_]\w*)")

    def apply(self, context: ParseContext) -> PolicyResult:
        path = Path(context.path)
        if path.suffix.lower() not in {".h", ".hpp", ".hh", ".hxx"}:
            return PolicyResult(text=context.text, violations=[], edits=[])

        mode = str(self._config.get("mode", "pragma_once")).lower()
        text = context.text
        lines = text.splitlines(keepends=True)
        if not lines:
            return PolicyResult(text=text, violations=[], edits=[])

        has_pragma = any(self._pragma_re.match(line) for line in lines[:10])
        guard = self._detect_guard(lines[:20])

        updated = text
        edits: list[Edit] = []
        violations: list[Violation] = []

        if mode in {"pragma_once", "both"} and not has_pragma:
            insert_idx = self._find_guard_insert_index(lines)
            pragma_line = "#pragma once\n"
            lines.insert(insert_idx, pragma_line)
            updated = "".join(lines)
            edits.append(
                Edit(
                    policy=self.name,
                    line=insert_idx + 1,
                    before="",
                    after=pragma_line.rstrip("\r\n"),
                )
            )
            violations.append(
                Violation(
                    policy=self.name,
                    message="Missing #pragma once",
                    line=1,
                    column=1,
                )
            )

        if mode in {"include_guard", "both"} and not guard:
            guard_macro = self._derive_guard_macro(path)
            body = updated.splitlines(keepends=True)
            guard_lines = [f"#ifndef {guard_macro}\n", f"#define {guard_macro}\n"]
            body = guard_lines + body + [f"#endif  // {guard_macro}\n"]
            updated = "".join(body)
            edits.append(
                Edit(
                    policy=self.name,
                    line=1,
                    before="",
                    after=f"#ifndef {guard_macro}",
                )
            )
            violations.append(
                Violation(
                    policy=self.name,
                    message="Missing include guard",
                    line=1,
                    column=1,
                )
            )

        if updated == text:
            return PolicyResult(text=text, violations=[], edits=[])
        return PolicyResult(text=updated, violations=violations, edits=edits)

    def _detect_guard(self, lines: list[str]) -> str | None:
        guard_name = None
        for line in lines:
            match = self._ifndef_re.match(line)
            if match:
                guard_name = match.group(1)
                break
        if guard_name is None:
            return None
        for line in lines:
            match = self._define_re.match(line)
            if match and match.group(1) == guard_name:
                return guard_name
        return None

    def _derive_guard_macro(self, path: Path) -> str:
        rel = str(path).replace("\\", "/")
        rel = re.sub(r"[^A-Za-z0-9]", "_", rel)
        rel = re.sub(r"_+", "_", rel).strip("_")
        if rel and rel[0].isdigit():
            rel = f"H_{rel}"
        return f"{rel.upper()}_"

    def _find_guard_insert_index(self, lines: list[str]) -> int:
        idx = 0
        while idx < len(lines):
            line = lines[idx]
            stripped = line.strip()
            if stripped.startswith("#!") or stripped.startswith("/*") or stripped.startswith("//") or stripped == "":
                idx += 1
                continue
            break
        return idx
