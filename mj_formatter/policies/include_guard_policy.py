from __future__ import annotations

from pathlib import Path
from typing import Any

from ..core.types import Edit
from ..core.types import ParseContext
from ..core.types import PolicyResult
from ..core.types import Violation
from ..core.utilities import warn_once
from .policy_base import Policy

_Directive = tuple[str, int, str]


class IncludeGuardPolicy(Policy):
    name = "include_guards"
    description = "Ensure include guards or #pragma once"
    parse_mode = "tree_sitter"

    def __init__(self, config: dict[str, object]) -> None:
        super().__init__(config)
        self._mode = self._required_mode("mode")
        self._header_extensions = self._required_extensions("header_extensions")

    def apply(self, context: ParseContext) -> PolicyResult:
        path = Path(context.path)
        if path.suffix.lower() not in self._header_extensions:
            return PolicyResult(text=context.text, violations=[], edits=[])

        root = getattr(getattr(context, "tree_sitter_tree", None), "root_node", None)
        if root is None:
            warn_once(
                "include_guard_parser_unavailable",
                "include_guards: tree-sitter unavailable, skipping policy",
            )
            return PolicyResult(text=context.text, violations=[], edits=[])

        mode = self._mode
        text = context.text
        lines = text.splitlines(keepends=True)
        if not lines:
            return PolicyResult(text=text, violations=[], edits=[])
        line_ending = self._detect_line_ending(text)

        directives = self._extract_directives(root, text)
        has_pragma = any(self._is_pragma_once(directive) for directive in directives) or any(
            self._is_pragma_once_line(line) for line in lines[:20]
        )
        guard = self._detect_guard(directives)

        updated = text
        edits: list[Edit] = []
        violations: list[Violation] = []

        if mode in {"pragma_once", "both"} and not has_pragma:
            insert_idx = self._find_guard_insert_index(lines)
            pragma_line = f"#pragma once{line_ending}"
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
            guard_lines = [f"#ifndef {guard_macro}{line_ending}", f"#define {guard_macro}{line_ending}"]
            body = guard_lines + body
            if body and not body[-1].endswith(("\n", "\r")):
                body[-1] += line_ending
            body.append(f"#endif  // {guard_macro}{line_ending}")
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

    def _extract_directives(self, root: Any, text: str) -> list[_Directive]:
        data = text.encode("utf-8", errors="ignore")
        directives: list[_Directive] = []
        for child in getattr(root, "children", []):
            kind = str(getattr(child, "type", "") or "")
            if not kind.startswith("preproc_"):
                continue
            start = int(getattr(child, "start_byte", 0))
            end = int(getattr(child, "end_byte", 0))
            if end <= start:
                continue
            snippet = data[start:end].decode("utf-8", errors="ignore")
            directives.append((kind, int(getattr(child, "start_point", (0, 0))[0]) + 1, snippet))
        directives.sort(key=lambda item: item[1])
        return directives

    def _is_pragma_once(self, directive: _Directive) -> bool:
        kind, _, text = directive
        if kind != "preproc_pragma":
            return False
        tokens = self._directive_tokens(text)
        if len(tokens) < 2:
            return False
        return tokens[0].lower() == "pragma" and tokens[1].lower() == "once"

    def _is_pragma_once_line(self, line: str) -> bool:
        tokens = self._directive_tokens(line)
        if len(tokens) < 2:
            return False
        return tokens[0].lower() == "pragma" and tokens[1].lower() == "once"

    def _detect_guard(self, directives: list[_Directive]) -> str | None:
        for directive in directives:
            kind, _, text = directive
            if kind != "preproc_ifdef":
                continue
            macro = self._guard_from_ifdef_block(text)
            if macro:
                return macro

        ifndef_macro = None
        ifndef_line = 0
        for directive in directives:
            kind, line, text = directive
            if kind != "preproc_ifndef":
                continue
            macro = self._macro_for_directive(text, "ifndef")
            if macro is None:
                continue
            ifndef_macro = macro
            ifndef_line = line
            break

        if ifndef_macro is None:
            return None

        has_define = False
        has_endif = False
        for directive in directives:
            kind, line, text = directive
            if line <= ifndef_line:
                continue
            if kind == "preproc_def":
                macro = self._macro_for_directive(text, "define")
                if macro == ifndef_macro:
                    has_define = True
                    continue
            if kind == "preproc_endif":
                has_endif = True

        if has_define and has_endif:
            return ifndef_macro
        return None

    def _guard_from_ifdef_block(self, text: str) -> str | None:
        ifndef_macro = None
        define_macro = None
        has_endif = False
        for line in str(text or "").splitlines():
            if ifndef_macro is None:
                ifndef_macro = self._macro_for_directive(line, "ifndef")
            if define_macro is None:
                define_macro = self._macro_for_directive(line, "define")
            tokens = self._directive_tokens(line)
            if tokens and tokens[0].lower() == "endif":
                has_endif = True
        if ifndef_macro and define_macro == ifndef_macro and has_endif:
            return ifndef_macro
        return None

    def _macro_for_directive(self, text: str, directive_name: str) -> str | None:
        tokens = self._directive_tokens(text)
        if len(tokens) < 2:
            return None
        if tokens[0].lower() != directive_name.lower():
            return None
        candidate = tokens[1]
        if self._is_identifier(candidate):
            return candidate
        return None

    def _directive_tokens(self, text: str) -> list[str]:
        normalized = " ".join(str(text or "").strip().split())
        if not normalized:
            return []
        parts = normalized.split(" ")
        if not parts:
            return []
        head = parts[0]
        if head == "#":
            parts = parts[1:]
        elif head.startswith("#"):
            parts[0] = head[1:]
        else:
            return []
        return [part.strip() for part in parts if part.strip()]

    def _is_identifier(self, value: str) -> bool:
        if not value:
            return False
        head = value[0]
        if not (head.isalpha() or head == "_"):
            return False
        for char in value[1:]:
            if not (char.isalnum() or char == "_"):
                return False
        return True

    def _derive_guard_macro(self, path: Path) -> str:
        rel = str(path).replace("\\", "/")
        chars: list[str] = []
        last_was_sep = False
        for char in rel:
            if char.isalnum():
                chars.append(char.upper())
                last_was_sep = False
            else:
                if not last_was_sep:
                    chars.append("_")
                    last_was_sep = True
        macro = "".join(chars).strip("_")
        if not macro:
            macro = "HEADER"
        if macro[0].isdigit():
            macro = f"H_{macro}"
        return f"{macro}_"

    def _find_guard_insert_index(self, lines: list[str]) -> int:
        idx = 0
        while idx < len(lines):
            line = lines[idx]
            stripped = line.strip()
            if stripped.startswith("#!") or self._is_comment_or_blank(stripped):
                idx += 1
                continue
            break
        return idx

    def _is_comment_or_blank(self, stripped: str) -> bool:
        if not stripped:
            return True
        if stripped.startswith("/*") or stripped.startswith("*") or stripped.startswith("*/"):
            return True
        if stripped.startswith("//"):
            return True
        return False

    def _required_mode(self, key: str) -> str:
        value = self._config.get(key)
        if value is None:
            raise ValueError(f"include_guards: missing required config key '{key}'")
        mode = str(value).strip().lower()
        if mode not in {"pragma_once", "include_guard", "both"}:
            raise ValueError(f"include_guards: invalid mode '{mode}'")
        return mode

    def _required_extensions(self, key: str) -> set[str]:
        value = self._config.get(key)
        if not isinstance(value, (list, tuple)):
            raise ValueError(f"include_guards: missing required list config key '{key}'")
        items = {str(item).strip().lower() for item in value if str(item).strip()}
        if not items:
            raise ValueError(f"include_guards: config key '{key}' cannot be empty")
        return items
