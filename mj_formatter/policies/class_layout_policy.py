from __future__ import annotations

import re
import os
from pathlib import Path
from dataclasses import dataclass

from ..core.edit import Edit
from ..core.parse_context import ParseContext
from ..core.policy_result import PolicyResult
from ..core.violation import Violation
from .policy_base import Policy


class ClassLayoutPolicy(Policy):
    name = "class_layout"
    description = "Enforce class access section ordering and method order matching headers"
    parse_mode = "text"

    _header_exts = (".hpp", ".h", ".hh", ".hxx")
    _source_exts = (".cpp", ".cc", ".cxx")
    _def_patterns = [
        re.compile(
            r"^(?!\s*(?:for|if|while|switch)\b)\s*(?:[\w:<>,\s*&]+)\s+([A-Za-z_]\w*(?:::[A-Za-z_]\w*)?)\s*\([^)]*\)\s*(const)?\s*\{",
            re.MULTILINE,
        ),
        re.compile(
            r"^\s*([A-Za-z_]\w*(?:::[A-Za-z_]\w*)*::~?[A-Za-z_]\w*)\s*\([^)]*\)\s*(const)?\s*\{",
            re.MULTILINE,
        ),
    ]

    def __init__(self, config: dict[str, object]) -> None:
        super().__init__(config)
        self._header_cache: dict[str, tuple[int, int, list[tuple[str, str]], dict[str, str]]] = {}

    def apply(self, context: ParseContext) -> PolicyResult:
        path = Path(context.path)
        if path.suffix.lower() in self._header_exts:
            return PolicyResult(text=context.text, violations=[], edits=[])
        if path.suffix.lower() in self._source_exts:
            return self._apply_source(context)
        return PolicyResult(text=context.text, violations=[], edits=[])

    def _apply_source(self, context: ParseContext) -> PolicyResult:
        header = self._find_header(context.path)
        if not header:
            return PolicyResult(text=context.text, violations=[], edits=[])

        order, class_kinds = self._get_header_order(header)
        if not order:
            return PolicyResult(text=context.text, violations=[], edits=[])

        text = context.text
        text, macro_changed = self._ensure_macro_header(text)
        text, global_changed = self._ensure_global_header(text)
        new_text, changed = self._reorder_definitions(
            ClassLayoutPolicy.ReorderArgs(text=text, order=order, class_kinds=class_kinds)
        )
        changed = changed or macro_changed or global_changed
        if not changed:
            return PolicyResult(text=context.text, violations=[], edits=[])

        violations = [
            Violation(
                policy=self.name,
                message="Reorder definitions to match header",
                line=1,
                column=1,
            )
        ]
        edits = [Edit(policy=self.name, line=1, before="", after="")]
        return PolicyResult(text=new_text, violations=violations, edits=edits)

    def _find_header(self, source_path: str) -> Path | None:
        src = Path(source_path)
        root = Path(self._config.get("root", ".")).resolve()
        stem = src.stem
        candidates = []
        for ext in self._header_exts:
            candidates.append(src.with_suffix(ext))
        for ext in self._header_exts:
            candidates.append(root / "include" / f"{stem}{ext}")
        for candidate in candidates:
            if candidate.exists():
                return candidate
        return None

    def _get_header_order(self, header: Path) -> tuple[list[tuple[str, str]], dict[str, str]]:
        key = str(header)
        try:
            stat = os.stat(header)
        except FileNotFoundError:
            return [], {}
        cached = self._header_cache.get(key)
        if cached and cached[0] == stat.st_mtime_ns and cached[1] == stat.st_size:
            return cached[2], cached[3]

        header_text = header.read_text(encoding="utf-8")
        order, class_kinds = self._extract_method_order(header_text)
        self._header_cache[key] = (stat.st_mtime_ns, stat.st_size, order, class_kinds)
        return order, class_kinds

    def _extract_method_order(self, header_text: str) -> tuple[list[tuple[str, str]], dict[str, str]]:
        order: list[tuple[str, str]] = []
        class_kinds: dict[str, str] = {}
        for match in re.finditer(r"\b(class|struct)\s+([A-Za-z_]\w*)", header_text):
            class_kinds[match.group(2)] = match.group(1)
        classes = set(class_kinds.keys())
        if not classes:
            return order, class_kinds
        for cls in classes:
            current_access = "public"
            for line in header_text.splitlines():
                access_match = re.match(r"^\s*(public|protected|private)\s*:\s*$", line)
                if access_match:
                    current_access = access_match.group(1)
                    continue
                ctor_match = re.match(rf"^\s*(?:explicit\s+)?(~?{cls})\s*\(", line)
                if ctor_match:
                    name = ctor_match.group(1)
                    order.append((f"{cls}::{name}", current_access))
                    order.append((name, current_access))
                    continue
                method_match = re.match(
                    rf"^\s*(?:virtual\s+)?(?:[\w:<>,\s*&]+)\s+({cls}::)?([A-Za-z_]\w*)\s*\(",
                    line,
                )
                if method_match:
                    name = method_match.group(2)
                    if not name:
                        continue
                    order.append((f"{cls}::{name}", current_access))
                    order.append((name, current_access))
        return order, class_kinds

    @dataclass(frozen=True)
    class ReorderArgs:
        text: str
        order: list[tuple[str, str]]
        class_kinds: dict[str, str]

    def _reorder_definitions(self, args: "ClassLayoutPolicy.ReorderArgs") -> tuple[str, bool]:
        blocks = self._find_definition_blocks(args.text)
        if not blocks:
            return args.text, False

        grouped: dict[str, list[tuple[int, int, str]]] = {}
        for start, end, name in blocks:
            grouped.setdefault(name, []).append((start, end, args.text[start:end]))

        ordered_blocks: list[str] = []
        used = set()
        current_access = None
        added_ctor_header = False
        pending_access: str | None = None
        for name, access in args.order:
            target = name
            if target not in grouped:
                for key in grouped:
                    if key.endswith(f"::{name}"):
                        target = key
                        break
            if target in grouped and target not in used:
                is_ctor = self._is_ctor_or_dtor(target, args.class_kinds)
                if is_ctor and not added_ctor_header:
                    ordered_blocks.append(self._constructor_header(target, args.class_kinds))
                    added_ctor_header = True
                if is_ctor:
                    pending_access = access
                else:
                    if pending_access is not None and pending_access != current_access:
                        ordered_blocks.append(self._access_header(pending_access))
                        current_access = pending_access
                    pending_access = None
                    if access != current_access:
                        ordered_blocks.append(self._access_header(access))
                        current_access = access
                for _, _, block in grouped[target]:
                    ordered_blocks.append(block)
                used.add(target)

        if not ordered_blocks:
            return args.text, False

        first = min(start for start, _, _ in blocks)
        first = self._expand_start_over_access_header(args.text, first)
        last = max(end for _, end, _ in blocks)
        original = args.text[first:last]
        reordered = self._join_blocks(ordered_blocks)
        if reordered.lstrip().startswith("//"):
            before = args.text[:first]
            if not before.endswith("\n\n"):
                reordered = "\n" + reordered

        if original == reordered:
            return args.text, False

        tail = args.text[last:]
        if reordered.endswith("\n") and tail.startswith("\n"):
            tail = tail[1:]
        return args.text[:first] + reordered + tail, True

    def _expand_start_over_access_header(self, text: str, first: int) -> int:
        lines = text.splitlines(keepends=True)
        offsets = []
        total = 0
        for line in lines:
            offsets.append(total)
            total += len(line)
        line_index = 0
        for idx, start in enumerate(offsets):
            if start <= first < start + len(lines[idx]):
                line_index = idx
                break
        header_re = re.compile(r"^//-+\s*$")
        title_re = re.compile(r"^//\s+(Public|Protected|Private)\s+functions\s*$")
        ctor_re = re.compile(r"^//\s+(Class|Struct)\s+Constructors\s*$")
        i = line_index - 1
        while i >= 0 and lines[i].strip() == "":
            i -= 1
        if i - 2 >= 0:
            if header_re.match(lines[i - 2]) and title_re.match(lines[i - 1]) and header_re.match(lines[i]):
                return offsets[i - 2]
            if header_re.match(lines[i - 2]) and ctor_re.match(lines[i - 1]) and header_re.match(lines[i]):
                return offsets[i - 2]
        return first

    def _find_definition_blocks(self, text: str) -> list[tuple[int, int, str]]:
        blocks: list[tuple[int, int, str]] = []
        patterns = self._def_patterns
        seen_starts: set[int] = set()
        for pattern in patterns:
            for match in pattern.finditer(text):
                start = match.start()
                if start in seen_starts:
                    continue
                seen_starts.add(start)
                name = match.group(1)
                brace = 0
                i = match.end() - 1
                while i < len(text):
                    ch = text[i]
                    if ch == "{":
                        brace += 1
                    elif ch == "}":
                        brace -= 1
                        if brace == 0:
                            end = i + 1
                            # Preserve any trailing inline comment after the closing brace.
                            while end < len(text) and text[end] not in "\r\n":
                                end += 1
                            blocks.append((start, end, name))
                            break
                    i += 1
        return blocks

    def _is_ctor_or_dtor(self, name: str, class_kinds: dict[str, str]) -> bool:
        for cls in class_kinds:
            if name.endswith(f"::{cls}") or name.endswith(f"::~{cls}"):
                return True
        return False

    def _constructor_header(self, name: str, class_kinds: dict[str, str]) -> str:
        kind = "class"
        for cls, cls_kind in class_kinds.items():
            if name.endswith(f"::{cls}") or name.endswith(f"::~{cls}"):
                kind = cls_kind
                break
        title = "Struct Constructors" if kind == "struct" else "Class Constructors"
        separator = "//" + "-" * 62
        return f"{separator}\n// {title}\n{separator}\n"

    def _access_header(self, access: str) -> str:
        title = {
            "public": "Public functions",
            "protected": "Protected functions",
            "private": "Private functions",
        }.get(access, "Functions")
        separator = "//" + "-" * 62
        return f"{separator}\n// {title}\n{separator}\n"

    def _ensure_macro_header(self, text: str) -> tuple[str, bool]:
        lines = text.splitlines(keepends=True)
        for idx, line in enumerate(lines):
            if line.lstrip().startswith("#define"):
                header = [
                    "//" + "-" * 62 + "\n",
                    "// user defined macros\n",
                    "//" + "-" * 62 + "\n",
                ]
                if idx >= 3 and [l.rstrip("\r\n") for l in lines[idx - 3 : idx]] == [
                    h.rstrip("\r\n") for h in header
                ]:
                    return text, False
                lines[idx:idx] = header
                return "".join(lines), True
        return text, False

    def _ensure_global_header(self, text: str) -> tuple[str, bool]:
        lines = text.splitlines(keepends=True)
        brace_depth = 0
        namespace_depths: list[int] = []
        for idx, line in enumerate(lines):
            if re.search(r"\bnamespace\b", line) and "{" in line:
                namespace_depths.append(brace_depth + 1)
            for ch in line:
                if ch == "{":
                    brace_depth += 1
                elif ch == "}":
                    brace_depth = max(0, brace_depth - 1)
                    if namespace_depths and brace_depth < namespace_depths[-1]:
                        namespace_depths.pop()
            in_namespace = bool(namespace_depths)
            if brace_depth != 0 and not in_namespace:
                continue
            stripped = line.strip()
            if not stripped or stripped.startswith("#") or stripped.startswith("//"):
                continue
            if "namespace" in stripped or stripped.startswith("using "):
                continue
            if "(" in stripped:
                continue
            if ";" not in stripped and "=" not in stripped:
                continue
            header = [
                "//" + "-" * 62 + "\n",
                "// Global Veriables\n",
                "//" + "-" * 62 + "\n",
            ]
            if self._has_recent_header(
                ClassLayoutPolicy.RecentHeaderArgs(lines=lines, idx=idx, header=header)
            ):
                return text, False
            lines[idx:idx] = header
            return "".join(lines), True
        return text, False

    @dataclass(frozen=True)
    class RecentHeaderArgs:
        lines: list[str]
        idx: int
        header: list[str]

    def _has_recent_header(self, args: "ClassLayoutPolicy.RecentHeaderArgs") -> bool:
        header_norm = [h.rstrip() for h in args.header]
        window = []
        for i in range(args.idx - 1, -1, -1):
            line = args.lines[i].rstrip()
            if line.strip() == "":
                continue
            window.append(line)
            if len(window) >= 3:
                break
        window = list(reversed(window))
        return window == header_norm

    def _join_blocks(self, blocks: list[str]) -> str:
        output: list[str] = []
        def is_header_block(text: str) -> bool:
            if not text.lstrip().startswith("//"):
                return False
            lowered = text.lower()
            return "functions" in lowered or "constructors" in lowered

        for block in blocks:
            block = block.strip("\n") + "\n"
            if not output:
                output.append(block)
                continue
            prev = output[-1]
            if is_header_block(prev):
                output.append(block)
                continue
            if is_header_block(block):
                output[-1] = prev.rstrip("\n") + "\n\n"
                output.append(block)
                continue
            output[-1] = prev.rstrip("\n") + "\n\n"
            output.append(block)
        return "".join(output)
