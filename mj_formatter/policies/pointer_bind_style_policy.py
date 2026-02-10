from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Any

from ..core.edit import Edit
from ..core.parse_context import ParseContext
from ..core.policy_result import PolicyResult
from ..core.violation import Violation
from .policy_base import Policy


class PointerBindStylePolicy(Policy):
    name = "pointer_bind_style"
    description = "Enforce pointer/reference binding to type"
    parse_mode = "tree_sitter"

    _decl_re = re.compile(
        r"""
        (?P<type>
            (?:\b[A-Za-z_]\w*\b(?:\s*::\s*\b[A-Za-z_]\w*\b)*)   # namespaced type
            (?:\s*<[^>]+>)?                                    # template args
            (?:\s+(?:const|volatile|unsigned|signed|long|short|struct|class|enum))*  # qualifiers
            (?:\s+|\s*::\s*)*
        )
        \s*
        (?P<ptr>[*&]+)
        \s*
        (?P<name>[A-Za-z_]\w*)
        """,
        re.VERBOSE,
    )

    def apply(self, context: ParseContext) -> PolicyResult:
        style = str(self._config.get("style", "bind_to_type")).lower()
        if style not in {"bind_to_type", "bind_to_name"}:
            style = "bind_to_type"

        text = context.text
        if context.tree_sitter_tree is None:
            return self._apply_regex(text, style)

        root = getattr(context.tree_sitter_tree, "root_node", None)
        if root is None:
            return self._apply_regex(text, style)

        data = text.encode("utf-8")
        replacements: list[tuple[int, int, str]] = []
        violations: list[Violation] = []
        stack = [root]
        while stack:
            node = stack.pop()
            if node.type in {"declaration", "field_declaration", "parameter_declaration"}:
                snippet = data[node.start_byte:node.end_byte].decode("utf-8", errors="ignore")
                updated = self._normalize_decl(snippet, style)
                if updated != snippet:
                    replacements.append((node.start_byte, node.end_byte, updated))
                    violations.append(
                        Violation(
                            policy=self.name,
                            message="Pointer/reference spacing adjusted",
                            line=node.start_point[0] + 1,
                            column=node.start_point[1] + 1,
                        )
                    )
            stack.extend(reversed(node.children))

        if not replacements:
            return PolicyResult(text=text, violations=[], edits=[])

        for start, end, repl in sorted(replacements, key=lambda item: item[0], reverse=True):
            data = data[:start] + repl.encode("utf-8") + data[end:]
        updated = data.decode("utf-8")

        edits: list[Edit] = []
        for idx, (before, after) in enumerate(zip(text.splitlines(keepends=True), updated.splitlines(keepends=True))):
            if before != after:
                edits.append(
                    Edit(
                        policy=self.name,
                        line=idx + 1,
                        before=before.rstrip("\r\n"),
                        after=after.rstrip("\r\n"),
                    )
                )
        return PolicyResult(text=updated, violations=violations, edits=edits)

    def _apply_regex(self, text: str, style: str) -> PolicyResult:
        updated = self._normalize_decl(text, style)
        if updated == text:
            return PolicyResult(text=text, violations=[], edits=[])
        edits: list[Edit] = []
        for idx, (before, after) in enumerate(zip(text.splitlines(keepends=True), updated.splitlines(keepends=True))):
            if before != after:
                edits.append(
                    Edit(
                        policy=self.name,
                        line=idx + 1,
                        before=before.rstrip("\r\n"),
                        after=after.rstrip("\r\n"),
                    )
                )
        violation = Violation(
            policy=self.name,
            message="Pointer/reference spacing adjusted",
            line=1,
            column=1,
        )
        return PolicyResult(text=updated, violations=[violation], edits=edits)

    def _normalize_decl(self, snippet: str, style: str) -> str:
        # only normalize the declarator portion before '=' to avoid expression changes
        parts = snippet.split("=", 1)
        head = parts[0]
        tail = ""
        if len(parts) > 1:
            tail = "=" + parts[1]

        def repl(match: re.Match) -> str:
            type_part = match.group("type").rstrip()
            ptr = match.group("ptr")
            name = match.group("name")
            if style == "bind_to_type":
                return f"{type_part}{ptr} {name}"
            return f"{type_part} {ptr}{name}"

        head = self._decl_re.sub(repl, head)
        return head + tail
