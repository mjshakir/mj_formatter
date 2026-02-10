from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from ..core.edit import Edit
from ..core.parse_context import ParseContext
from ..core.policy_result import PolicyResult
from ..core.violation import Violation
from .policy_base import Policy


class NamespaceEndCommentsPolicy(Policy):
    name = "namespace_end_comments"
    description = "Add end comments for namespace/class/struct/function blocks"
    parse_mode = "tree_sitter"

    _node_map = {
        "namespace_definition": "namespace",
        "class_specifier": "class",
        "struct_specifier": "struct",
        "function_definition": "function",
        "if_statement": "if",
        "while_statement": "while",
        "for_statement": "for",
        "switch_statement": "switch",
    }

    def apply(self, context: ParseContext) -> PolicyResult:
        if context.tree_sitter_tree is None:
            return PolicyResult(text=context.text, violations=[], edits=[])

        blocks = set(self._config.get("blocks", []) or [])
        if not blocks:
            return PolicyResult(text=context.text, violations=[], edits=[])

        max_named_lines = int(self._config.get("max_named_lines", 40))
        text = context.text
        lines = text.splitlines(keepends=True)
        data = text.encode("utf-8")
        root = getattr(context.tree_sitter_tree, "root_node", None)
        if root is None:
            return PolicyResult(text=text, violations=[], edits=[])

        replacements: list[tuple[int, int, str]] = []
        violations: list[Violation] = []

        stack = [root]
        while stack:
            node = stack.pop()
            kind = self._node_map.get(node.type)
            if kind and kind in blocks:
                body = self._body_node(node)
                if body is None:
                    stack.extend(reversed(node.children))
                    continue
                end_line = body.end_point[0]
                if end_line >= len(lines):
                    stack.extend(reversed(node.children))
                    continue
                line_text = lines[end_line]
                if "//" in line_text:
                    stack.extend(reversed(node.children))
                    continue
                name = self._extract_name(node, data)
                if name and (body.end_point[0] - node.start_point[0]) > max_named_lines:
                    name = "(...)"
                comment = f" // {kind}"
                if name:
                    comment += f" {name}"
                insert_pos = self._line_end_pos(lines, end_line)
                replacements.append((insert_pos, insert_pos, comment))
                violations.append(
                    Violation(
                        policy=self.name,
                        message=f"Missing end comment for {kind}",
                        line=end_line + 1,
                        column=1,
                    )
                )
            stack.extend(reversed(node.children))

        if not replacements:
            return PolicyResult(text=text, violations=[], edits=[])

        for start, end, repl in sorted(replacements, key=lambda item: item[0], reverse=True):
            data = data[:start] + repl.encode("utf-8") + data[end:]
        updated = data.decode("utf-8")

        edits: list[Edit] = []
        if updated != text:
            for idx, (before, after) in enumerate(zip(lines, updated.splitlines(keepends=True))):
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

    def _body_node(self, node: Any) -> Any | None:
        for child in node.children:
            if child.type in {"compound_statement", "field_declaration_list", "declaration_list"}:
                return child
        return None

    def _extract_name(self, node: Any, data: bytes) -> str | None:
        for child in node.children:
            if child.type in {"identifier", "type_identifier"}:
                return data[child.start_byte:child.end_byte].decode("utf-8", errors="ignore")
        return None

    def _line_end_pos(self, lines: list[str], line_index: int) -> int:
        pos = 0
        for idx, line in enumerate(lines):
            if idx == line_index:
                return pos + len(line.rstrip("\r\n"))
            pos += len(line)
        return pos
