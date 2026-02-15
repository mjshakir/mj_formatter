from __future__ import annotations

import os
from pathlib import Path
from collections.abc import Sequence
from bisect import bisect_left
from typing import Any

from ..core.types import CodeContext, SemanticContext, SemanticSymbol
from ..core.types import Edit
from ..core.types import ParseContext
from ..core.parsing import ParserManager
from ..core.types import PolicyResult
from ..core.types import Violation
from ..core.types import _PreambleItem, _SourceBlock
from ..core.utilities import warn_once
from .policy_base import Policy


class ClassLayoutPolicy(Policy):
    name = "class_layout"
    description = "Enforce cpp method implementation order from header declarations"
    parse_mode = "tree_sitter"
    requires_code_context = True

    def __init__(self, config: dict[str, object]) -> None:
        super().__init__(config)
        self._source_extensions = self._required_str_tuple("source_extensions")
        self._header_extensions = self._required_str_tuple("header_extensions")
        self._header_search_roots = self._required_str_tuple("header_search_roots")
        self._header_cache: dict[str, tuple[int, int, list[tuple[str, str]], dict[str, str]]] = {}
        self._parser_manager = ParserManager()

    def apply(self, context: ParseContext) -> PolicyResult:
        path = Path(context.path)
        if path.suffix.lower() not in self._source_extensions:
            return PolicyResult(text=context.text, violations=[], edits=[])

        if context.code_context is None:
            warn_once("class_layout_no_context", "class_layout: code context unavailable, skipping policy")
            return PolicyResult(text=context.text, violations=[], edits=[])

        header = self._find_header(context.path, self._header_extensions)
        if header is None:
            return PolicyResult(text=context.text, violations=[], edits=[])

        order, class_kinds = self._get_header_order(header)
        if not order or not class_kinds:
            return PolicyResult(text=context.text, violations=[], edits=[])

        updated, changed = self._reorder_source_blocks(context.text, context.code_context, order, class_kinds)
        if not changed:
            return PolicyResult(text=context.text, violations=[], edits=[])

        edits = [
            Edit(
                policy=self.name,
                line=1,
                before="",
                after="",
            )
        ]
        violations = [
            Violation(
                policy=self.name,
                message="Reordered implementations to match header declaration order",
                line=1,
                column=1,
            )
        ]
        return PolicyResult(text=updated, violations=violations, edits=edits)

    def _find_header(self, source_path: str, header_exts: Sequence[str]) -> Path | None:
        src = Path(source_path)
        root = Path(self._config.get("root", ".")).resolve()
        include_dirs = self._header_search_roots
        stem = src.stem
        candidates: list[Path] = []
        for ext in header_exts:
            candidates.append(src.with_suffix(ext))
        for rel in include_dirs:
            base = (root / str(rel)).resolve()
            for ext in header_exts:
                candidates.append(base / f"{stem}{ext}")
        for item in candidates:
            if item.exists():
                return item
        return None

    def _required_str_tuple(self, key: str) -> tuple[str, ...]:
        value = self._config.get(key)
        if not isinstance(value, (list, tuple)):
            raise ValueError(f"class_layout: missing required list config key '{key}'")
        items = tuple(str(item).strip() for item in value if str(item).strip())
        if not items:
            raise ValueError(f"class_layout: config key '{key}' cannot be empty")
        return items

    def _get_header_order(self, header: Path) -> tuple[list[tuple[str, str]], dict[str, str]]:
        key = str(header.resolve())
        try:
            stat = os.stat(header)
        except OSError:
            return [], {}
        cached = self._header_cache.get(key)
        if cached and cached[0] == stat.st_mtime_ns and cached[1] == stat.st_size:
            return cached[2], cached[3]

        text = header.read_text(encoding="utf-8")
        tree, _, warning = self._parser_manager.parse_tree_sitter(text, str(header))
        if tree is None:
            if warning:
                warn_once("class_layout_header_parse", f"class_layout: header parse skipped: {warning}")
            return [], {}
        order, class_kinds = self._extract_member_order_tree(text, tree)
        self._header_cache[key] = (stat.st_mtime_ns, stat.st_size, order, class_kinds)
        return order, class_kinds

    def _extract_member_order_tree(self, text: str, tree: Any) -> tuple[list[tuple[str, str]], dict[str, str]]:
        data = text.encode("utf-8", errors="ignore")
        root = getattr(tree, "root_node", None)
        if root is None:
            return [], {}

        class_kinds: dict[str, str] = {}
        order: list[tuple[str, str]] = []

        stack = [root]
        while stack:
            node = stack.pop()
            node_type = getattr(node, "type", "")
            if node_type in {"class_specifier", "struct_specifier"}:
                class_name = self._node_text(self._first_descendant(node, {"type_identifier"}), data)
                if not class_name:
                    stack.extend(reversed(getattr(node, "children", [])))
                    continue
                class_kind = "class" if node_type == "class_specifier" else "struct"
                class_kinds[class_name] = class_kind
                default_access = "private" if class_kind == "class" else "public"
                access = default_access
                body = self._first_child(node, {"field_declaration_list"})
                if body is None:
                    stack.extend(reversed(getattr(node, "children", [])))
                    continue
                for child in getattr(body, "children", []):
                    child_type = getattr(child, "type", "")
                    if child_type == "access_specifier":
                        token = self._node_text(child, data).replace(":", "").strip().lower()
                        if token in {"public", "protected", "private"}:
                            access = token
                        continue
                    method_name = self._member_function_name(child, data)
                    if not method_name:
                        continue
                    full_name = f"{class_name}::{method_name}"
                    order.append((full_name, access))
                    order.append((method_name, access))
                continue
            stack.extend(reversed(getattr(node, "children", [])))

        return order, class_kinds

    def _member_function_name(self, node: Any, data: bytes) -> str | None:
        destructor = self._first_descendant(node, {"destructor_name"})
        if destructor is not None:
            name = self._node_text(destructor, data)
            if name:
                return name
        func_decl = self._first_descendant(node, {"function_declarator"})
        if func_decl is None:
            return None
        name_node = self._rightmost_name_node(func_decl)
        if name_node is None:
            return None
        return self._node_text(name_node, data) or None

    def _reorder_source_blocks(
        self,
        text: str,
        code_context: CodeContext,
        order: list[tuple[str, str]],
        class_kinds: dict[str, str],
    ) -> tuple[str, bool]:
        blocks = self._collect_source_blocks(text, code_context)
        ordered_names = {name for name, _ in order}
        candidates = [
            item
            for item in blocks
            if item.class_name in class_kinds or item.short_name in ordered_names or item.full_name in ordered_names
        ]
        if len(candidates) < 2:
            return text, False

        by_name: dict[str, list[_SourceBlock]] = {}
        for block in candidates:
            by_name.setdefault(block.full_name, []).append(block)

        first_candidate = min(item.start for item in candidates)
        preamble_items = self._collect_preamble_items(text, first_candidate)

        used: set[tuple[int, int]] = set()
        current_access: str | None = None
        ctor_header_emitted = False
        emitted_parts: list[str] = self._emit_preamble(preamble_items)

        for target_name, access in order:
            matched = self._take_block(target_name, by_name, used)
            if matched is None:
                continue
            is_ctor = self._is_ctor_or_dtor(matched, class_kinds)
            merged_with_header = False
            if is_ctor and not ctor_header_emitted:
                emitted_parts.append(self._constructor_header(matched, class_kinds) + "\n" + matched.text.rstrip("\n"))
                ctor_header_emitted = True
                merged_with_header = True
            if not is_ctor and access != current_access:
                emitted_parts.append(self._access_header(access) + "\n" + matched.text.rstrip("\n"))
                current_access = access
                merged_with_header = True
            if not merged_with_header:
                emitted_parts.append(matched.text.rstrip("\n"))

        # Preserve unmapped class member definitions at end in source order.
        for block in sorted(candidates, key=lambda item: item.start):
            key = (block.start, block.end)
            if key in used:
                continue
            emitted_parts.append(block.text.rstrip("\n"))

        if not emitted_parts:
            return text, False

        first = min([item.start for item in candidates] + [item.start for item in preamble_items]) if preamble_items else first_candidate
        last = max(item.end for item in candidates)
        replacement = "\n\n".join(part for part in emitted_parts if part).rstrip("\n")
        tail = text[last:]
        if not tail.startswith("\n"):
            replacement += "\n"
        original = text[first:last]
        if original == replacement:
            return text, False
        return text[:first] + replacement + text[last:], True

    def _collect_preamble_items(self, text: str, limit: int) -> list[_PreambleItem]:
        if limit <= 0:
            return []
        out: list[_PreambleItem] = []
        pos = 0
        for line in text[:limit].splitlines(keepends=True):
            next_pos = pos + len(line)
            stripped = line.strip()
            if stripped.startswith("#define"):
                out.append(_PreambleItem(kind="macro", start=pos, end=next_pos, text=line.rstrip("\r\n")))
            elif (
                stripped
                and not stripped.startswith("#")
                and not stripped.startswith("//")
                and not stripped.startswith("/*")
                and stripped.endswith(";")
                and "(" not in stripped
                and ")" not in stripped
                and "{" not in stripped
                and "}" not in stripped
            ):
                out.append(_PreambleItem(kind="global", start=pos, end=next_pos, text=line.rstrip("\r\n")))
            pos = next_pos
        return out

    def _emit_preamble(self, items: list[_PreambleItem]) -> list[str]:
        if not items:
            return []
        macros = [item.text for item in items if item.kind == "macro" and item.text.strip()]
        globals_ = [item.text for item in items if item.kind == "global" and item.text.strip()]
        out: list[str] = []
        if macros:
            out.append(f"{self._section_header('user defined macros')}\n\n" + "\n".join(macros))
        if globals_:
            out.append(f"{self._section_header('Global Veriables')}\n" + "\n".join(globals_))
        return out

    def _section_header(self, title: str) -> str:
        sep = "//" + "-" * 62
        return f"{sep}\n// {title}\n{sep}"

    def _collect_source_blocks(self, text: str, code_context: CodeContext) -> list[_SourceBlock]:
        function_symbols: list[SemanticSymbol] = list(getattr(code_context, "semantic_function_symbols", ()) or ())
        if not function_symbols:
            semantic = code_context.semantic_context
            if isinstance(semantic, SemanticContext):
                function_kinds = {"function_decl", "cxx_method", "constructor", "destructor", "function_template"}
                for symbol in semantic.symbols:
                    if symbol.kind in function_kinds and symbol.scope_kind == "function":
                        function_symbols.append(symbol)
        function_symbols.sort(key=lambda item: int(item.start))
        function_starts = [int(item.start) for item in function_symbols]

        out: list[_SourceBlock] = []
        for block in code_context.hybrid_blocks:
            if block.kind != "function":
                continue
            start = int(block.start)
            end = int(block.end)
            if end <= start or end > len(text):
                continue
            full_name = ""
            short_name = ""
            class_name: str | None = None
            if function_starts:
                idx = bisect_left(function_starts, start)
                if idx > 0:
                    idx -= 1
                while idx < len(function_symbols):
                    symbol = function_symbols[idx]
                    symbol_start = int(symbol.start)
                    if symbol_start > end:
                        break
                    if start <= symbol_start <= end:
                        class_name = symbol.scope_name
                        short_name = symbol.name
                        full_name = f"{symbol.scope_name}::{symbol.name}" if symbol.scope_name else symbol.name
                        break
                    idx += 1
            if not full_name:
                label = block.short_label.split("(", 1)[0].strip()
                if "::" in label:
                    parts = [item for item in label.split("::") if item]
                    if len(parts) >= 2:
                        short_name = parts[-1]
                        class_name = parts[-2]
                        full_name = f"{class_name}::{short_name}"
                    else:
                        short_name = label
                        full_name = label
                        class_name = None
                else:
                    short_name = label
                    full_name = label
                    class_name = None
            out.append(
                _SourceBlock(
                    start=start,
                    end=end,
                    text=text[start:end],
                    full_name=full_name,
                    short_name=short_name,
                    class_name=class_name,
                )
            )
        out.sort(key=lambda item: item.start)
        return out

    def _take_block(
        self,
        target_name: str,
        by_name: dict[str, list[_SourceBlock]],
        used: set[tuple[int, int]],
    ) -> _SourceBlock | None:
        candidates = by_name.get(target_name, [])
        for item in candidates:
            key = (item.start, item.end)
            if key in used:
                continue
            used.add(key)
            return item
        if "::" not in target_name:
            suffix = f"::{target_name}"
            for name, entries in by_name.items():
                if not name.endswith(suffix):
                    continue
                for item in entries:
                    key = (item.start, item.end)
                    if key in used:
                        continue
                    used.add(key)
                    return item
        return None

    def _is_ctor_or_dtor(self, block: _SourceBlock, class_kinds: dict[str, str]) -> bool:
        if not block.class_name:
            return False
        cls = block.class_name
        if cls not in class_kinds:
            return False
        return block.short_name == cls or block.short_name == f"~{cls}"

    def _constructor_header(self, block: _SourceBlock, class_kinds: dict[str, str]) -> str:
        kind = class_kinds.get(block.class_name or "", "class")
        title = "Struct Constructors" if kind == "struct" else "Class Constructors"
        sep = "//" + "-" * 62
        return f"{sep}\n// {title}\n{sep}"

    def _access_header(self, access: str) -> str:
        title = {
            "public": "Public functions",
            "protected": "Protected functions",
            "private": "Private functions",
        }.get(access.lower(), "Functions")
        sep = "//" + "-" * 62
        return f"{sep}\n// {title}\n{sep}"

    def _first_child(self, node: Any, types: set[str]) -> Any | None:
        for child in getattr(node, "children", []):
            if getattr(child, "type", "") in types:
                return child
        return None

    def _first_descendant(self, node: Any, types: set[str]) -> Any | None:
        stack = [node]
        while stack:
            current = stack.pop()
            if current is not node and getattr(current, "type", "") in types:
                return current
            stack.extend(reversed(getattr(current, "children", [])))
        return None

    def _rightmost_name_node(self, node: Any) -> Any | None:
        best = None
        stack = [node]
        while stack:
            current = stack.pop()
            ctype = getattr(current, "type", "")
            if ctype in {"identifier", "field_identifier", "type_identifier"}:
                if best is None or int(current.start_byte) > int(best.start_byte):
                    best = current
            if ctype in {"parameter_list", "template_parameter_list"}:
                continue
            stack.extend(reversed(getattr(current, "children", [])))
        return best

    def _node_text(self, node: Any | None, data: bytes) -> str:
        if node is None:
            return ""
        start = int(getattr(node, "start_byte", 0))
        end = int(getattr(node, "end_byte", 0))
        if end <= start:
            return ""
        return data[start:end].decode("utf-8", errors="ignore").strip()
