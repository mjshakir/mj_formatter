from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from ..core.types import Edit
from ..core.types import ParseContext
from ..core.types import PolicyResult
from ..core.types import Violation
from ..core.utilities import warn_once
from .policy_base import Policy


class LineWrapPolicy(Policy):
    name = "line_wrap"
    description = "Wrap long lines with parser-aware argument/parameter formatting"
    parse_mode = "tree_sitter"

    @dataclass(frozen=True)
    class Candidate:
        start: int
        end: int
        line_no: int
        replacement: str

    def apply(self, context: ParseContext) -> PolicyResult:
        text = context.text
        if not text:
            return PolicyResult(text=text, violations=[], edits=[])

        max_len = self._effective_max_length(context)
        if max_len <= 0:
            return PolicyResult(text=text, violations=[], edits=[])

        wrap_style = str(self._config.get("wrap_style", "smart")).strip().lower()
        if wrap_style not in {"smart", "bin_pack", "one_per_line"}:
            wrap_style = "smart"
        keep_inline = bool(self._config.get("allow_inline_prefix_args", True))
        continuation_indent = int(self._config.get("continuation_indent", 4))
        continuation_indent = max(1, continuation_indent)
        align_to_open = bool(self._config.get("align_to_open_paren", True))
        use_editorconfig = bool(self._config.get("use_editorconfig", True))
        wrap_calls = bool(self._config.get("wrap_calls", True))
        wrap_function_declarations = bool(self._config.get("wrap_function_declarations", False))
        skip_declaration_expressions = bool(self._config.get("skip_declaration_expressions", True))

        indent_width = int(self._config.get("tab_width", 4))
        if use_editorconfig:
            indent_width = self._editorconfig_int(context, ("tab_width", "indent_size"), indent_width)
        indent_width = max(1, indent_width)

        if context.tree_sitter_tree is None:
            warn_once(
                "line_wrap_tree_unavailable",
                "line_wrap: tree-sitter unavailable; falling back to violation-only mode",
            )
            return self._violation_only(text, max_len)

        data = text.encode("utf-8")
        root = getattr(context.tree_sitter_tree, "root_node", None)
        if root is None:
            return self._violation_only(text, max_len)

        candidates: list[LineWrapPolicy.Candidate] = []
        stack = [root]
        while stack:
            node = stack.pop()
            list_node = self._list_node(
                node=node,
                wrap_calls=wrap_calls,
                wrap_function_declarations=wrap_function_declarations,
                skip_declaration_expressions=skip_declaration_expressions,
            )
            if list_node is not None:
                candidate = self._build_candidate(
                    data=data,
                    list_node=list_node,
                    max_len=max_len,
                    wrap_style=wrap_style,
                    keep_inline=keep_inline,
                    continuation_indent=continuation_indent,
                    indent_width=indent_width,
                    align_to_open=align_to_open,
                )
                if candidate is not None:
                    candidates.append(candidate)
            stack.extend(reversed(node.children))

        selected = self._dedupe_candidates(candidates)
        if not selected:
            return self._violation_only(text, max_len)

        out = data
        for item in sorted(selected, key=lambda x: x.start, reverse=True):
            out = out[: item.start] + item.replacement.encode("utf-8") + out[item.end :]
        updated = out.decode("utf-8")
        if updated == text:
            return self._violation_only(text, max_len)

        edits: list[Edit] = []
        before_lines = text.splitlines(keepends=True)
        after_lines = updated.splitlines(keepends=True)
        for idx, (before, after) in enumerate(zip(before_lines, after_lines)):
            if before != after:
                edits.append(
                    Edit(
                        policy=self.name,
                        line=idx + 1,
                        before=before.rstrip("\r\n"),
                        after=after.rstrip("\r\n"),
                    )
                )

        touched = {item.line_no for item in selected}
        violations = [
            Violation(
                policy=self.name,
                message=f"Wrapped long call/declaration line to max length {max_len}",
                line=line_no,
                column=max_len + 1,
            )
            for line_no in sorted(touched)
        ]
        return PolicyResult(text=updated, violations=violations, edits=edits)

    def _effective_max_length(self, context: ParseContext) -> int:
        max_len = int(self._config.get("max_length", 100))
        if not bool(self._config.get("use_editorconfig", True)):
            return max_len
        raw = context.editorconfig.get("max_line_length")
        if raw is None:
            return max_len
        value = raw.strip().lower()
        if value in {"off", "none", "unset"}:
            return max_len
        try:
            parsed = int(value)
        except ValueError:
            return max_len
        return max(1, parsed)

    def _editorconfig_int(self, context: ParseContext, keys: tuple[str, ...], default: int) -> int:
        for key in keys:
            raw = context.editorconfig.get(key)
            if raw is None:
                continue
            try:
                return int(raw)
            except ValueError:
                continue
        return default

    def _list_node(
        self,
        node: Any,
        wrap_calls: bool,
        wrap_function_declarations: bool,
        skip_declaration_expressions: bool,
    ) -> Any | None:
        if wrap_calls and node.type == "call_expression":
            if skip_declaration_expressions and self._has_ancestor_type(
                node,
                {
                    "declaration",
                    "field_declaration",
                    "parameter_declaration",
                    "init_declarator",
                    "function_declarator",
                    "declaration_list",
                    "field_declaration_list",
                },
            ):
                return None
            for child in node.children:
                if child.type == "argument_list":
                    return child
        if wrap_function_declarations and node.type == "function_declarator":
            for child in node.children:
                if child.type == "parameter_list":
                    return child
        return None

    def _has_ancestor_type(self, node: Any, target_types: set[str]) -> bool:
        current = getattr(node, "parent", None)
        while current is not None:
            if getattr(current, "type", "") in target_types:
                return True
            current = getattr(current, "parent", None)
        return False

    def _build_candidate(
        self,
        data: bytes,
        list_node: Any,
        max_len: int,
        wrap_style: str,
        keep_inline: bool,
        continuation_indent: int,
        indent_width: int,
        align_to_open: bool,
    ) -> Candidate | None:
        open_b = int(list_node.start_byte)
        close_b = int(list_node.end_byte)
        if close_b <= open_b + 2:
            return None

        line_start, line_end, line_no = self._line_span(data, open_b)
        line = data[line_start:line_end].decode("utf-8", errors="ignore")
        if len(line.rstrip("\r\n")) <= max_len:
            return None
        raw_list = data[open_b:close_b].decode("utf-8", errors="ignore")
        if "\n" in raw_list or "\r" in raw_list:
            return None
        if "//" in raw_list or "/*" in raw_list or "*/" in raw_list:
            return None
        if not raw_list.startswith("(") or not raw_list.endswith(")"):
            return None

        inner = raw_list[1:-1].strip()
        if not inner:
            return None
        args = [item.strip() for item in self._split_top_level(inner) if item.strip()]
        if not args:
            return None

        prefix = data[line_start:open_b].decode("utf-8", errors="ignore")
        suffix = ""
        base_indent = prefix[: len(prefix) - len(prefix.lstrip(" \t"))]
        open_col = len(prefix)
        if align_to_open:
            cont_prefix = " " * (open_col + continuation_indent)
            close_prefix = " " * open_col
        else:
            cont_prefix = base_indent + (" " * continuation_indent)
            close_prefix = base_indent

        wrapped = self._format_wrapped(
            args=args,
            prefix=prefix,
            suffix=suffix,
            cont_prefix=cont_prefix,
            close_prefix=close_prefix,
            max_len=max_len,
            wrap_style=wrap_style,
            keep_inline=keep_inline,
        )
        if wrapped is None:
            return None
        return LineWrapPolicy.Candidate(start=open_b, end=close_b, line_no=line_no, replacement=wrapped)

    def _line_span(self, data: bytes, offset: int) -> tuple[int, int, int]:
        start = data.rfind(b"\n", 0, offset) + 1
        end = data.find(b"\n", offset)
        if end < 0:
            end = len(data)
        line_no = data.count(b"\n", 0, start) + 1
        return start, end, line_no

    def _format_wrapped(
        self,
        args: list[str],
        prefix: str,
        suffix: str,
        cont_prefix: str,
        close_prefix: str,
        max_len: int,
        wrap_style: str,
        keep_inline: bool,
    ) -> str | None:
        open_prefix = "("
        close_line = close_prefix + ")" + suffix

        remaining = list(args)
        lines: list[str] = []

        if keep_inline:
            inline_acc: list[str] = []
            while remaining:
                trial = ", ".join(inline_acc + [remaining[0]])
                if len(prefix + open_prefix + trial) <= max_len:
                    inline_acc.append(remaining.pop(0))
                    continue
                break
            if inline_acc:
                first = open_prefix + ", ".join(inline_acc)
                if not remaining:
                    final_single = first + ")" + suffix
                    if len((prefix + final_single).rstrip("\r\n")) <= max_len:
                        return final_single
                lines.append(first + ",")

        if wrap_style == "one_per_line":
            grouped = [[arg] for arg in remaining]
        elif wrap_style == "bin_pack":
            grouped = self._bin_pack(remaining, cont_prefix, max_len)
        else:
            grouped = self._bin_pack(remaining, cont_prefix, max_len)
            if len(grouped) > 1:
                packed_max = max(len(cont_prefix + ", ".join(group)) for group in grouped)
                if packed_max > max_len:
                    grouped = [[arg] for arg in remaining]

        for idx, group in enumerate(grouped):
            if not group:
                continue
            line = cont_prefix + ", ".join(group)
            is_last_group = idx == len(grouped) - 1
            if not is_last_group:
                line += ","
            lines.append(line)

        if not lines:
            return None

        return "\n".join(lines + [close_line])

    def _bin_pack(self, args: list[str], prefix: str, max_len: int) -> list[list[str]]:
        if not args:
            return []
        groups: list[list[str]] = []
        current: list[str] = []
        for arg in args:
            trial = ", ".join(current + [arg])
            if not current or len(prefix + trial) <= max_len:
                current.append(arg)
                continue
            groups.append(current)
            current = [arg]
        if current:
            groups.append(current)
        return groups

    def _split_top_level(self, text: str) -> list[str]:
        items: list[str] = []
        current: list[str] = []
        depth_paren = 0
        depth_brack = 0
        depth_brace = 0
        depth_angle = 0
        in_string = False
        in_char = False
        escape = False

        for ch in text:
            current.append(ch)
            if in_string:
                if escape:
                    escape = False
                elif ch == "\\":
                    escape = True
                elif ch == "\"":
                    in_string = False
                continue
            if in_char:
                if escape:
                    escape = False
                elif ch == "\\":
                    escape = True
                elif ch == "'":
                    in_char = False
                continue

            if ch == "\"":
                in_string = True
                continue
            if ch == "'":
                in_char = True
                continue
            if ch == "(":
                depth_paren += 1
                continue
            if ch == ")":
                depth_paren = max(0, depth_paren - 1)
                continue
            if ch == "[":
                depth_brack += 1
                continue
            if ch == "]":
                depth_brack = max(0, depth_brack - 1)
                continue
            if ch == "{":
                depth_brace += 1
                continue
            if ch == "}":
                depth_brace = max(0, depth_brace - 1)
                continue
            if ch == "<":
                depth_angle += 1
                continue
            if ch == ">":
                depth_angle = max(0, depth_angle - 1)
                continue
            if (
                ch == ","
                and depth_paren == 0
                and depth_brack == 0
                and depth_brace == 0
                and depth_angle == 0
            ):
                token = "".join(current[:-1]).strip()
                items.append(token)
                current = []

        tail = "".join(current).strip()
        if tail:
            items.append(tail)
        return items

    def _dedupe_candidates(self, candidates: list[Candidate]) -> list[Candidate]:
        if not candidates:
            return []
        chosen: list[LineWrapPolicy.Candidate] = []
        occupied: list[tuple[int, int]] = []
        for item in sorted(candidates, key=lambda c: (c.start, -(c.end - c.start))):
            if any(not (item.end <= lo or item.start >= hi) for lo, hi in occupied):
                continue
            chosen.append(item)
            occupied.append((item.start, item.end))
        return chosen

    def _violation_only(self, text: str, max_len: int) -> PolicyResult:
        violations: list[Violation] = []
        for idx, line in enumerate(text.splitlines(), 1):
            if len(line.rstrip("\n\r")) > max_len:
                violations.append(
                    Violation(
                        policy=self.name,
                        message=f"Line exceeds max length {max_len}",
                        line=idx,
                        column=max_len + 1,
                    )
                )
        return PolicyResult(text=text, violations=violations, edits=[])
