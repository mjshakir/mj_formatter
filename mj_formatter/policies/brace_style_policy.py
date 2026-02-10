from __future__ import annotations

from typing import Any

from ..core.edit import Edit
from ..core.parse_context import ParseContext
from ..core.policy_result import PolicyResult
from ..core.violation import Violation
from .policy_base import Policy


class BraceStylePolicy(Policy):
    name = "brace_style"
    description = "Enforce brace style (e.g., K&R)"
    parse_mode = "tree_sitter"

    _node_types = {
        "function_definition",
        "class_specifier",
        "struct_specifier",
        "namespace_definition",
        "if_statement",
        "for_statement",
        "while_statement",
        "switch_statement",
    }

    def apply(self, context: ParseContext) -> PolicyResult:
        style = str(self._config.get("style", "kr")).lower()
        if style not in {"kr", "allman", "stroustrup"}:
            style = "kr"

        if context.tree_sitter_tree is None:
            return PolicyResult(text=context.text, violations=[], edits=[])

        text = context.text
        root = getattr(context.tree_sitter_tree, "root_node", None)
        if root is None:
            return PolicyResult(text=text, violations=[], edits=[])

        replacements: list[tuple[int, int, str]] = []
        violations: list[Violation] = []

        data = text.encode("utf-8")
        stack = [root]
        while stack:
            node = stack.pop()
            if node.type in self._node_types:
                body = self._find_body_node(node)
                if body is None:
                    stack.extend(reversed(node.children))
                    continue
                brace_start = body.start_byte
                line_start = text.rfind("\n", 0, brace_start) + 1
                line_end = text.find("\n", brace_start)
                if line_end == -1:
                    line_end = len(text)
                on_same_line = line_start <= brace_start < line_end and text[line_start:brace_start].strip() != ""
                if style in {"kr", "stroustrup"} and not on_same_line:
                    # move brace to same line
                    replace_start = line_start - 1
                    if replace_start < 0:
                        replace_start = line_start
                    replacement = " {"
                    replacements.append((replace_start, brace_start, replacement))
                    violations.append(
                        Violation(
                            policy=self.name,
                            message="Brace should be on same line",
                            line=node.start_point[0] + 1,
                            column=node.start_point[1] + 1,
                        )
                    )
                if style == "allman" and on_same_line:
                    # move brace to next line
                    indent = text[line_start : line_start + len(text[line_start:line_end])].split("\n")[0]
                    leading = ""
                    for ch in indent:
                        if ch in {" ", "\t"}:
                            leading += ch
                        else:
                            break
                    last_non_space = brace_start - 1
                    while last_non_space >= line_start and text[last_non_space].isspace():
                        last_non_space -= 1
                    replace_start = last_non_space + 1
                    replacement = "\n" + leading + "{"
                    replacements.append((replace_start, brace_start + 1, replacement))
                    violations.append(
                        Violation(
                            policy=self.name,
                            message="Brace should be on next line",
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
        if updated != text:
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

    def _find_body_node(self, node: Any) -> Any | None:
        for child in node.children:
            if child.type in {"compound_statement", "field_declaration_list", "declaration_list"}:
                return child
        return None
