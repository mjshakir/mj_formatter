from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Any

from ..core.clang_decls import ClangDeclCollector
from ..core.edit import Edit
from ..core.parse_context import ParseContext
from ..core.policy_result import PolicyResult
from ..core.violation import Violation
from .policy_base import Policy


class FunctionVoidParamsPolicy(Policy):
    name = "function_void_params"
    description = "Require (void) for empty parameter lists and no space before parens"
    parse_mode = "clang"

    def apply(self, context: ParseContext) -> PolicyResult:
        text = context.text
        if not text:
            return PolicyResult(text=text, violations=[], edits=[])

        require_void = bool(self._config.get("require_void", True))
        no_space = bool(self._config.get("no_space_before_paren", True))

        replacements: list[tuple[int, int, str]] = []
        violations: list[Violation] = []

        if context.clang_ast is not None:
            collector = ClangDeclCollector(context.clang_ast, context.path)
            for decl in collector.functions():
                if decl.params_start is None or decl.params_end is None:
                    continue
                params_text = text[decl.params_start:decl.params_end]
                if not self._is_empty_param_list(params_text):
                    continue
                new_params = "(void)" if require_void else "()"
                if params_text != new_params:
                    replacements.append((decl.params_start, decl.params_end, new_params))
                    violations.append(
                        Violation(
                            policy=self.name,
                            message=f"Empty parameter list for '{decl.name}'",
                            line=decl.line,
                            column=decl.column,
                        )
                    )
                if no_space:
                    space_start, space_end = self._space_before_paren(text, decl.params_start)
                    if space_start is not None:
                        replacements.append((space_start, space_end, ""))
            return self._apply_replacements(text, replacements, violations)

        if context.tree_sitter_tree is not None:
            return self._apply_tree_sitter(context, require_void, no_space)

        return self._apply_regex(text, require_void, no_space)

    def _apply_tree_sitter(
        self,
        context: ParseContext,
        require_void: bool,
        no_space: bool,
    ) -> PolicyResult:
        text = context.text
        data = text.encode("utf-8")
        root = getattr(context.tree_sitter_tree, "root_node", None)
        if root is None:
            return PolicyResult(text=text, violations=[], edits=[])

        replacements: list[tuple[int, int, str]] = []
        violations: list[Violation] = []
        stack = [root]
        while stack:
            node = stack.pop()
            if node.type == "function_declarator":
                param_list = None
                for child in node.children:
                    if child.type == "parameter_list":
                        param_list = child
                        break
                if param_list is not None:
                    params_text = data[param_list.start_byte:param_list.end_byte].decode("utf-8", errors="ignore")
                    if self._is_empty_param_list(params_text):
                        new_params = "(void)" if require_void else "()"
                        if params_text != new_params:
                            replacements.append((param_list.start_byte, param_list.end_byte, new_params))
                            violations.append(
                                Violation(
                                    policy=self.name,
                                    message="Empty parameter list",
                                    line=param_list.start_point[0] + 1,
                                    column=param_list.start_point[1] + 1,
                                )
                            )
                        if no_space:
                            space_start, space_end = self._space_before_paren(text, param_list.start_byte)
                            if space_start is not None:
                                replacements.append((space_start, space_end, ""))
            stack.extend(reversed(node.children))

        return self._apply_replacements(text, replacements, violations)

    def _apply_regex(self, text: str, require_void: bool, no_space: bool) -> PolicyResult:
        violations: list[Violation] = []
        edits: list[Edit] = []
        lines = text.splitlines(keepends=True)
        updated_lines = []
        pattern = re.compile(r"\b([A-Za-z_]\w*(?:::\w+)*)\s*\(\s*\)")
        for idx, line in enumerate(lines):
            new_line = line
            for match in list(pattern.finditer(line)):
                name = match.group(1)
                repl = f"{name}({'void' if require_void else ''})"
                if repl.endswith("()") and not require_void:
                    repl = f"{name}()"
                if match.group(0) != repl:
                    new_line = new_line.replace(match.group(0), repl)
                    violations.append(
                        Violation(
                            policy=self.name,
                            message=f"Empty parameter list for '{name}'",
                            line=idx + 1,
                            column=match.start() + 1,
                        )
                    )
            if no_space:
                new_line = re.sub(r"([A-Za-z_]\w*(?:::\w+)*)\s+\(", r"\1(", new_line)
            if new_line != line:
                edits.append(
                    Edit(
                        policy=self.name,
                        line=idx + 1,
                        before=line.rstrip("\r\n"),
                        after=new_line.rstrip("\r\n"),
                    )
                )
            updated_lines.append(new_line)
        updated = "".join(updated_lines)
        return PolicyResult(text=updated, violations=violations, edits=edits)

    def _is_empty_param_list(self, params_text: str) -> bool:
        inner = params_text.strip()
        if not inner.startswith("(") or not inner.endswith(")"):
            return False
        content = inner[1:-1].strip()
        return content == "" or content == "void"

    def _space_before_paren(self, text: str, paren_start: int) -> tuple[int | None, int | None]:
        idx = paren_start - 1
        while idx >= 0 and text[idx] == " ":
            idx -= 1
        space_start = idx + 1
        if space_start < paren_start:
            return space_start, paren_start
        return None, None

    def _apply_replacements(
        self,
        text: str,
        replacements: list[tuple[int, int, str]],
        violations: list[Violation],
    ) -> PolicyResult:
        if not replacements:
            return PolicyResult(text=text, violations=violations, edits=[])
        data = text.encode("utf-8")
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
