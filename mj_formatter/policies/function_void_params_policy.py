from __future__ import annotations

from typing import Any

from ..core.parsing import ClangDeclCollector
from ..core.types import Edit
from ..core.types import ParseContext
from ..core.types import PolicyResult
from ..core.types import Violation
from ..core.utilities import warn_once
from .policy_base import Policy


class FunctionVoidParamsPolicy(Policy):
    name = "function_void_params"
    description = "Require (void) for empty parameter lists and no space before parens"
    parse_mode = "clang"
    requires_code_context = True

    def __init__(self, config: dict[str, object]) -> None:
        super().__init__(config)
        self._prefer_clang = self._required_bool("prefer_clang")
        self._use_tree_sitter = self._required_bool("use_tree_sitter")
        self._require_void = self._required_bool("require_void")
        self._no_space_before_paren = self._required_bool("no_space_before_paren")
        if not self._prefer_clang and not self._use_tree_sitter:
            raise ValueError(
                "function_void_params: invalid config (both prefer_clang and use_tree_sitter are false)"
            )
        self.parse_mode = "clang" if self._prefer_clang else "tree_sitter"

    def apply(self, context: ParseContext) -> PolicyResult:
        text = context.text
        if not text:
            return PolicyResult(text=text, violations=[], edits=[])

        replacements: list[tuple[int, int, str]] = []
        violations: list[Violation] = []

        code_context = context.code_context
        if code_context is not None and getattr(code_context, "clang_functions", None):
            clang_functions = list(code_context.clang_functions)
        elif context.clang_ast is not None:
            collector = ClangDeclCollector(context.clang_ast, context.path)
            clang_functions = collector.functions()
        else:
            clang_functions = []

        if clang_functions:
            for decl in clang_functions:
                if decl.name.startswith("operator"):
                    continue
                if decl.params_start is None or decl.params_end is None:
                    continue
                params_text = text[decl.params_start:decl.params_end]
                if not self._is_empty_param_list(params_text):
                    continue
                new_params = "(void)" if self._require_void else "()"
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
                if self._no_space_before_paren:
                    space_start, space_end = self._space_before_paren(text, decl.params_start)
                    if space_start is not None:
                        replacements.append((space_start, space_end, ""))
            return self._apply_replacements(text, replacements, violations)

        if self._use_tree_sitter and context.tree_sitter_tree is not None:
            return self._apply_tree_sitter(
                context,
                self._require_void,
                self._no_space_before_paren,
            )

        warn_once(
            "function_void_params_parser_unavailable",
            "function_void_params: parser context unavailable, skipping policy (enable clang and/or tree-sitter-languages)",
        )
        return PolicyResult(text=text, violations=[], edits=[])

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
                decl_head = data[node.start_byte:node.end_byte].decode("utf-8", errors="ignore")
                if "operator" in decl_head:
                    stack.extend(reversed(node.children))
                    continue
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

    def _required_bool(self, key: str) -> bool:
        value = self._config.get(key)
        if not isinstance(value, bool):
            raise ValueError(f"function_void_params: missing required boolean config key '{key}'")
        return value
