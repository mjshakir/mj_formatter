from __future__ import annotations

from typing import Any

from ..core.types import Edit
from ..core.types import ParseContext
from ..core.types import PolicyResult
from ..core.types import Violation
from ..core.utilities import warn_once
from .policy_base import Policy


class PointerBindStylePolicy(Policy):
    name = "pointer_bind_style"
    description = "Enforce pointer/reference binding to type"
    parse_mode = "tree_sitter"
    requires_code_context = True

    def __init__(self, config: dict[str, object]) -> None:
        super().__init__(config)
        self._style = self._required_style("style")

    def apply(self, context: ParseContext) -> PolicyResult:
        text = context.text
        tree = context.tree_sitter_tree
        if tree is None:
            warn_once(
                "pointer_bind_style_parser_unavailable",
                "pointer_bind_style: tree-sitter unavailable, skipping policy",
            )
            return PolicyResult(text=text, violations=[], edits=[])

        root = getattr(tree, "root_node", None)
        if root is None:
            return PolicyResult(text=text, violations=[], edits=[])

        data = text.encode("utf-8", errors="ignore")
        pointer_ranges = self._semantic_pointer_ranges(context)
        replacements: list[tuple[int, int, str, int, int]] = []

        stack = [root]
        while stack:
            node = stack.pop()
            node_type = str(getattr(node, "type", "") or "")
            if node_type in {"pointer_declarator", "reference_declarator"}:
                replacement = self._replacement_for_pointer_node(
                    node=node,
                    data=data,
                    style=self._style,
                    pointer_ranges=pointer_ranges,
                )
                if replacement is not None:
                    replacements.append(replacement)
            stack.extend(reversed(getattr(node, "children", [])))

        if not replacements:
            return PolicyResult(text=text, violations=[], edits=[])

        # Remove overlaps by preferring the inner-most (latest start).
        replacements.sort(key=lambda item: (item[0], item[1]))
        filtered: list[tuple[int, int, str, int, int]] = []
        for current in replacements:
            if not filtered:
                filtered.append(current)
                continue
            prev = filtered[-1]
            if current[0] < prev[1]:
                prev_score = prev[2].count("*") + prev[2].count("&")
                current_score = current[2].count("*") + current[2].count("&")
                if current_score > prev_score:
                    filtered[-1] = current
                    continue
                if current_score == prev_score and (current[1] - current[0]) > (prev[1] - prev[0]):
                    filtered[-1] = current
                continue
            filtered.append(current)

        for start, end, repl, _, _ in sorted(filtered, key=lambda item: item[0], reverse=True):
            data = data[:start] + repl.encode("utf-8") + data[end:]
        updated = data.decode("utf-8")

        if updated == text:
            return PolicyResult(text=text, violations=[], edits=[])

        edits: list[Edit] = []
        violations: list[Violation] = []
        for idx, (before, after) in enumerate(zip(text.splitlines(keepends=True), updated.splitlines(keepends=True))):
            if before == after:
                continue
            edits.append(
                Edit(
                    policy=self.name,
                    line=idx + 1,
                    before=before.rstrip("\r\n"),
                    after=after.rstrip("\r\n"),
                )
            )
            violations.append(
                Violation(
                    policy=self.name,
                    message="Pointer/reference spacing adjusted",
                    line=idx + 1,
                    column=1,
                )
            )

        return PolicyResult(text=updated, violations=violations, edits=edits)

    def _semantic_pointer_ranges(self, context: ParseContext) -> tuple[tuple[int, int], ...]:
        code_context = getattr(context, "code_context", None)
        semantic = getattr(code_context, "semantic_context", None)
        symbols = getattr(semantic, "symbols", ()) if semantic is not None else ()
        ranges: list[tuple[int, int]] = []
        for symbol in symbols:
            scope_kind = str(getattr(symbol, "scope_kind", "") or "")
            if scope_kind not in {"param", "local", "member", "global"}:
                continue
            is_pointer = bool(getattr(symbol, "is_pointer", False))
            smart_ptr = str(getattr(symbol, "smart_ptr", "") or "")
            if not is_pointer and not smart_ptr:
                continue
            start = int(getattr(symbol, "start", 0) or 0)
            end = int(getattr(symbol, "end", 0) or 0)
            if end > start:
                ranges.append((start, end))
        ranges.sort()
        return tuple(ranges)

    def _replacement_for_pointer_node(
        self,
        *,
        node: Any,
        data: bytes,
        style: str,
        pointer_ranges: tuple[tuple[int, int], ...],
    ) -> tuple[int, int, str, int, int] | None:
        if not self._has_declaration_ancestor(node):
            return None

        name_node = self._rightmost_identifier(node)
        if name_node is None:
            return None

        name_start = int(getattr(name_node, "start_byte", 0) or 0)
        name_end = int(getattr(name_node, "end_byte", 0) or 0)
        if name_end <= name_start:
            return None

        if pointer_ranges and not self._overlaps_any((name_start, name_end), pointer_ranges):
            return None

        start = int(getattr(node, "start_byte", 0) or 0)
        if start >= name_start:
            return None

        if b"\n" in data[start:name_start] or b"\r" in data[start:name_start]:
            return None

        while start > 0 and data[start - 1:start] in {b" ", b"\t"}:
            prev = data[start - 2:start - 1] if start > 1 else b""
            if prev in {b"\n", b"\r"}:
                break
            start -= 1

        raw = data[start:name_start].decode("utf-8", errors="ignore")
        if not self._is_safe_pointer_spacing_segment(raw):
            return None
        ptr_tokens = "".join(ch for ch in raw if ch in {"*", "&"})
        if not ptr_tokens:
            return None

        if style == "bind_to_type":
            replacement = f"{ptr_tokens} "
        else:
            replacement = f" {ptr_tokens}"

        current = data[start:name_start].decode("utf-8", errors="ignore")
        if current == replacement:
            return None

        line = int(getattr(node, "start_point", (0, 0))[0]) + 1
        col = int(getattr(node, "start_point", (0, 0))[1]) + 1
        return start, name_start, replacement, line, col

    def _is_safe_pointer_spacing_segment(self, raw: str) -> bool:
        # Keep pointer_bind_style non-destructive:
        # only normalize pure whitespace + pointer/reference token runs.
        if not raw:
            return False
        for ch in raw:
            if ch in {" ", "\t", "*", "&"}:
                continue
            return False
        return any(ch in {"*", "&"} for ch in raw)

    def _has_declaration_ancestor(self, node: Any) -> bool:
        current = getattr(node, "parent", None)
        while current is not None:
            current_type = str(getattr(current, "type", "") or "")
            if current_type in {
                "declaration",
                "field_declaration",
                "parameter_declaration",
                "optional_parameter_declaration",
                "init_declarator",
            }:
                return True
            if current_type in {"function_definition", "lambda_expression"}:
                break
            current = getattr(current, "parent", None)
        return False

    def _rightmost_identifier(self, node: Any) -> Any | None:
        best: Any | None = None
        best_start = -1
        stack = [node]
        while stack:
            current = stack.pop()
            current_type = str(getattr(current, "type", "") or "")
            if current_type in {"identifier", "field_identifier"}:
                start = int(getattr(current, "start_byte", -1) or -1)
                if start >= best_start:
                    best = current
                    best_start = start
            if current_type in {"parameter_list", "template_parameter_list", "template_parameter"}:
                continue
            stack.extend(reversed(getattr(current, "children", [])))
        return best

    def _overlaps_any(self, candidate: tuple[int, int], ranges: tuple[tuple[int, int], ...]) -> bool:
        start, end = candidate
        for left, right in ranges:
            if left < end and start < right:
                return True
        return False

    def _required_style(self, key: str) -> str:
        value = self._config.get(key)
        if value is None:
            raise ValueError(f"pointer_bind_style: missing required config key '{key}'")
        style = str(value).strip().lower()
        if style not in {"bind_to_type", "bind_to_name"}:
            raise ValueError(
                "pointer_bind_style: config key 'style' must be one of "
                "'bind_to_type' or 'bind_to_name'"
            )
        return style
