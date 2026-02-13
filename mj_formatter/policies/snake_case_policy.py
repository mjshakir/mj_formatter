from __future__ import annotations

import re
from typing import Any

from ..core.parsing import ClangDeclCollector
from ..core.types import ParseContext
from ..core.types import PolicyResult
from ..core.types import Violation
from ..core.utilities import warn_once
from .policy_base import Policy


class SnakeCasePolicy(Policy):
    name = "snake_case"
    description = "Enforce snake_case for variables/functions"
    parse_mode = "clang"
    requires_code_context = True

    _snake_re = re.compile(r"^_?[a-z][a-z0-9_]*$")

    def __init__(self, config: dict[str, object]) -> None:
        super().__init__(config)
        self._prefer_clang = bool(self._config.get("prefer_clang", True))
        self._use_tree_sitter = bool(self._config.get("use_tree_sitter", True))
        self.parse_mode = "clang" if self._prefer_clang else "tree_sitter"

    def apply(self, context: ParseContext) -> PolicyResult:
        text = context.text
        if not text:
            return PolicyResult(text=text, violations=[], edits=[])

        apply_to = str(self._config.get("apply_to", "both")).lower()
        include_vars = apply_to in {"variables", "both"}
        include_funcs = apply_to in {"functions", "both"}
        exclude_types = bool(self._config.get("exclude_class_namespace", True))

        violations: list[Violation] = []

        code_context = context.code_context
        if code_context is not None and getattr(code_context, "clang_functions", None) is not None:
            clang_functions = list(getattr(code_context, "clang_functions", ()))
            clang_variables = list(getattr(code_context, "clang_variables", ()))
        elif context.clang_ast is not None:
            collector = ClangDeclCollector(context.clang_ast, context.path)
            clang_functions = collector.functions()
            clang_variables = collector.variables()
        else:
            clang_functions = []
            clang_variables = []

        if clang_functions or clang_variables:
            if include_funcs:
                for decl in clang_functions:
                    if exclude_types and decl.name[:1].isupper():
                        continue
                    if not self._is_snake_case(decl.name):
                        violations.append(
                            Violation(
                                policy=self.name,
                                message=f"Function '{decl.name}' is not snake_case",
                                line=decl.line,
                                column=decl.column,
                            )
                        )
            if include_vars:
                for decl in clang_variables:
                    if exclude_types and decl.name[:1].isupper():
                        continue
                    if not self._is_snake_case(decl.name):
                        violations.append(
                            Violation(
                                policy=self.name,
                                message=f"Variable '{decl.name}' is not snake_case",
                                line=decl.line,
                                column=decl.column,
                            )
                        )
            return PolicyResult(text=text, violations=violations, edits=[])

        if not self._use_tree_sitter or context.tree_sitter_tree is None:
            warn_once(
                "snake_case_parser_unavailable",
                "snake_case: parser context unavailable, skipping policy (enable clang and/or tree-sitter-languages)",
            )
            return PolicyResult(text=text, violations=[], edits=[])

        names = self._collect_identifiers_tree(context, include_funcs, include_vars)
        for name, line in names:
            if exclude_types and name[:1].isupper():
                continue
            if not self._is_snake_case(name):
                violations.append(
                    Violation(
                        policy=self.name,
                        message=f"Identifier '{name}' is not snake_case",
                        line=line,
                        column=1,
                    )
                )
        return PolicyResult(text=text, violations=violations, edits=[])

    def _is_snake_case(self, name: str) -> bool:
        if not name:
            return True
        if name.isupper() and "_" in name:
            return True
        return bool(self._snake_re.match(name))

    def _collect_identifiers_tree(
        self,
        context: ParseContext,
        include_funcs: bool,
        include_vars: bool,
    ) -> list[tuple[str, int]]:
        text = context.text
        data = text.encode("utf-8")
        root = getattr(context.tree_sitter_tree, "root_node", None)
        if root is None:
            return []
        names: list[tuple[str, int]] = []
        stack = [root]
        while stack:
            node = stack.pop()
            if include_funcs and node.type == "function_definition":
                name = self._extract_name(data, node, target_types={"identifier"})
                if name:
                    names.append((name, node.start_point[0] + 1))
            if include_vars and node.type in {"declaration", "field_declaration"}:
                name = self._extract_name(data, node, target_types={"identifier", "field_identifier"})
                if name:
                    names.append((name, node.start_point[0] + 1))
            stack.extend(reversed(node.children))
        return names

    def _extract_name(self, data: bytes, node: Any, target_types: set[str]) -> str | None:
        stack = [node]
        while stack:
            current = stack.pop()
            if current.type in target_types:
                return data[current.start_byte:current.end_byte].decode("utf-8", errors="ignore")
            if current.type in {"parameter_list", "template_parameter_list"}:
                continue
            stack.extend(reversed(current.children))
        return None
