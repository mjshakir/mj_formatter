from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ..core.types import Edit
from ..core.types import ParseContext
from ..core.types import PolicyResult
from ..core.types import Violation
from ..core.utilities import warn_once
from .policy_base import Policy


class IncludeOrderPolicy(Policy):
    name = "include_order"
    description = "Order includes into groups with headings"
    # Use clang for classification and tree-sitter for precise include region extraction.
    parse_mode = "clang"
    _use_tree_sitter = True

    def apply(self, context: ParseContext) -> PolicyResult:
        lines = context.text.splitlines(keepends=True)
        if not lines:
            return PolicyResult(text=context.text, violations=[], edits=[])

        if context.tree_sitter_tree is None:
            warn_once(
                "include_order_tree_unavailable",
                "include_order: tree-sitter unavailable, skipping policy",
            )
            return PolicyResult(text=context.text, violations=[], edits=[])
        if context.clang_ast is None:
            warn_once(
                "include_order_clang_unavailable",
                "include_order: clang context unavailable, skipping policy",
            )
            return PolicyResult(text=context.text, violations=[], edits=[])

        includes, start, end = self._extract_includes_tree(context.text, lines, context.tree_sitter_tree)
        if not includes or start is None or end is None:
            return PolicyResult(text=context.text, violations=[], edits=[])

        include_lines = lines[start:end]
        line_ending = self._detect_line_ending(context.text)
        clang_standard_lines = self._collect_clang_standard_header_lines(
            clang_ast=context.clang_ast,
            source_path=context.path,
        )
        ordered_block = self._build_ordered_block(
            IncludeOrderPolicy.BuildOrderedBlockArgs(
                path=context.path,
                includes=includes,
                line_ending=line_ending,
                clang_standard_lines=clang_standard_lines,
            )
        )
        original_block = "".join(include_lines)
        new_block = "".join(ordered_block)

        if original_block == new_block:
            return PolicyResult(text=context.text, violations=[], edits=[])

        updated = "".join(lines[:start]) + new_block + "".join(lines[end:])
        violation = Violation(
            policy=self.name,
            message="Includes are not ordered",
            line=start + 1,
            column=1,
        )
        edit = Edit(
            policy=self.name,
            line=start + 1,
            before=original_block.rstrip("\n\r"),
            after=new_block.rstrip("\n\r"),
        )
        return PolicyResult(text=updated, violations=[violation], edits=[edit])

    def _extract_includes_tree(
        self,
        text: str,
        lines: list[str],
        tree: Any,
    ) -> tuple[list[dict[str, object]], int | None, int | None]:
        root = getattr(tree, "root_node", None)
        if root is None:
            return [], None, None

        data = text.encode("utf-8", errors="ignore")
        include_nodes = self._select_include_cluster(root, lines)
        if not include_nodes:
            return [], None, None

        includes: list[dict[str, object]] = []
        include_line_indices: set[int] = set()
        for node in include_nodes:
            parsed = self._parse_include_node(node, data)
            if parsed is None:
                continue
            header, quote = parsed
            line_index = int(node.start_point[0])
            include_line_indices.add(line_index)
            includes.append({"header": header, "quote": quote, "line": line_index + 1})

        if not includes:
            return [], None, None

        first = min(include_line_indices)
        last = max(include_line_indices)
        start = self._expand_include_start(lines, first, include_line_indices)
        end = self._expand_include_end(lines, last + 1, include_line_indices)
        return includes, start, end

    def _select_include_cluster(self, root: Any, lines: list[str]) -> list[Any]:
        nodes: list[Any] = []
        stack = [root]
        while stack:
            node = stack.pop()
            if getattr(node, "type", "") == "preproc_include":
                nodes.append(node)
            stack.extend(reversed(getattr(node, "children", [])))

        if not nodes:
            return []

        nodes.sort(key=lambda node: (int(node.start_point[0]), int(node.start_point[1])))
        clusters: list[list[Any]] = []
        current: list[Any] = [nodes[0]]

        for node in nodes[1:]:
            prev = current[-1]
            prev_end_line = int(getattr(prev, "end_point", (int(prev.start_point[0]), 0))[0])
            next_start_line = int(node.start_point[0])
            if self._lines_are_non_code(lines, prev_end_line + 1, next_start_line):
                current.append(node)
                continue
            clusters.append(current)
            current = [node]

        if current:
            clusters.append(current)

        return clusters[0] if clusters else []

    def _lines_are_non_code(self, lines: list[str], start: int, end: int) -> bool:
        if end <= start:
            return True
        upper = min(len(lines), end)
        for idx in range(max(0, start), upper):
            if not self._is_comment_or_blank(lines[idx]):
                return False
        return True

    def _parse_include_node(self, node: Any, data: bytes) -> tuple[str, str] | None:
        for child in getattr(node, "children", []):
            child_type = str(getattr(child, "type", "") or "")
            if child_type not in {"system_lib_string", "string_literal"}:
                continue
            snippet = data[int(child.start_byte) : int(child.end_byte)].decode("utf-8", errors="ignore")
            parsed = self._extract_header_from_snippet(snippet)
            if parsed is not None:
                return parsed

        snippet = data[int(node.start_byte) : int(node.end_byte)].decode("utf-8", errors="ignore")
        return self._extract_header_from_snippet(snippet)

    def _extract_header_from_snippet(self, snippet: str) -> tuple[str, str] | None:
        text = str(snippet or "")
        for index, char in enumerate(text):
            if char == '"':
                end = text.find('"', index + 1)
                if end <= index + 1:
                    return None
                return text[index + 1 : end], '"'
            if char == "<":
                end = text.find(">", index + 1)
                if end <= index + 1:
                    return None
                return text[index + 1 : end], "<"
        return None

    def _expand_include_start(self, lines: list[str], start: int, include_lines: set[int]) -> int:
        idx = start
        while idx > 0:
            prev_idx = idx - 1
            if prev_idx in include_lines:
                idx -= 1
                continue
            prev_line = lines[prev_idx]
            if not self._is_comment_or_blank(prev_line):
                break
            if prev_line.strip().startswith("#pragma once"):
                break
            idx -= 1
        return idx

    def _expand_include_end(self, lines: list[str], end: int, include_lines: set[int]) -> int:
        idx = end
        while idx < len(lines):
            if idx in include_lines:
                idx += 1
                continue
            if self._is_comment_or_blank(lines[idx]):
                idx += 1
                continue
            break
        return idx

    def _is_comment_or_blank(self, line: str) -> bool:
        stripped = line.strip()
        if stripped == "":
            return True
        if stripped.startswith("#pragma once"):
            return True
        if stripped.startswith("//"):
            return True
        if stripped.startswith("/*") or stripped.startswith("*") or stripped.startswith("*/"):
            return True
        return False

    @dataclass(frozen=True)
    class BuildOrderedBlockArgs:
        path: str
        includes: list[dict[str, object]]
        line_ending: str
        clang_standard_lines: set[int]

    def _build_ordered_block(self, args: "IncludeOrderPolicy.BuildOrderedBlockArgs") -> list[str]:
        is_header = Path(args.path).suffix.lower() in {".h", ".hpp", ".hh", ".hxx"}

        groups: dict[str, list[dict[str, object]]] = {
            "main": [],
            "standard": [],
            "third_party": [],
            "project": [],
            "local": [],
        }

        main_candidates = self._main_header_candidates(args.path)
        standard_headers = set(self._config.get("standard_headers", []) or [])
        standard_prefixes = tuple(self._config.get("standard_prefixes", []) or ())
        project_headers = set(self._config.get("project_headers", []) or [])
        project_prefixes = tuple(self._config.get("project_prefixes", []) or ())

        for item in args.includes:
            header = str(item.get("header", ""))
            quote = str(item.get("quote", ""))
            line = int(item.get("line", 0) or 0)

            if not is_header and header in main_candidates:
                groups["main"].append(item)
                continue

            if quote == "<":
                if self._is_standard_header(
                    IncludeOrderPolicy.StandardHeaderArgs(
                        header=header,
                        line=line,
                        standard_headers=standard_headers,
                        standard_prefixes=standard_prefixes,
                        clang_standard_lines=args.clang_standard_lines,
                    )
                ):
                    groups["standard"].append(item)
                else:
                    groups["third_party"].append(item)
                continue

            if self._is_project_header(
                IncludeOrderPolicy.ProjectHeaderArgs(
                    header=header,
                    project_headers=project_headers,
                    project_prefixes=project_prefixes,
                )
            ):
                groups["project"].append(item)
            else:
                groups["local"].append(item)

        if is_header:
            order = self._config.get("order_header", ["standard", "third_party", "project", "local"])
        else:
            order = self._config.get("order_source", ["main", "standard", "third_party", "project", "local"])

        titles = self._config.get(
            "group_titles",
            {
                "main": "Main header",
                "standard": "Standard Cpp Libraries",
                "third_party": "Third-party headers",
                "project": "Project headers",
                "local": "User Defined Headers",
            },
        )

        output: list[str] = []
        for group_name in order:
            items = groups.get(group_name, [])
            if not items:
                continue
            output.extend(
                self._group_heading(
                    IncludeOrderPolicy.GroupHeadingArgs(
                        group_name=group_name,
                        titles=titles,
                        groups=groups,
                    )
                )
            )
            if len(items) > 1:
                items = sorted(items, key=lambda i: str(i.get("header", "")).lower())
            for inc in items:
                header = str(inc.get("header", ""))
                quote = str(inc.get("quote", ""))
                if quote == "<":
                    output.append(f"#include <{header}>{args.line_ending}")
                else:
                    output.append(f"#include \"{header}\"{args.line_ending}")
            output.append(args.line_ending)

        if output:
            output.pop()
        return output

    def _collect_clang_standard_header_lines(self, clang_ast: Any, source_path: str) -> set[int]:
        if clang_ast is None:
            return set()
        get_includes = getattr(clang_ast, "get_includes", None)
        if get_includes is None:
            return set()

        target = self._normalized_path(source_path)
        standard_lines: set[int] = set()
        try:
            inclusions = list(get_includes())
        except Exception:
            return set()

        for inclusion in inclusions:
            location = getattr(inclusion, "location", None)
            if location is None:
                continue
            line_no = int(getattr(location, "line", 0) or 0)
            source_file = getattr(location, "file", None)
            if line_no <= 0 or source_file is None:
                continue
            if self._normalized_path(str(source_file)) != target:
                continue
            include_obj = getattr(inclusion, "include", None)
            include_name = str(getattr(include_obj, "name", "") or "")
            if not include_name:
                continue
            if self._is_probably_standard_header_from_path(include_name):
                standard_lines.add(line_no)
        return standard_lines

    def _normalized_path(self, value: str) -> str:
        if not value:
            return ""
        try:
            return str(Path(value).resolve())
        except Exception:
            return str(value)

    def _is_probably_standard_header_from_path(self, include_path: str) -> bool:
        normalized = include_path.replace("\\", "/").lower()
        if "/include/c++/" in normalized:
            return True
        if "/c++/v1/" in normalized:
            return True
        if "/lib/clang/" in normalized and "/include/" in normalized:
            return True
        if "/include/bits/" in normalized:
            return True
        return False

    @dataclass(frozen=True)
    class StandardHeaderArgs:
        header: str
        line: int
        standard_headers: set[str]
        standard_prefixes: tuple[str, ...]
        clang_standard_lines: set[int]

    def _is_standard_header(self, args: "IncludeOrderPolicy.StandardHeaderArgs") -> bool:
        header = args.header
        if header in args.standard_headers:
            return True
        for prefix in args.standard_prefixes:
            if header.startswith(prefix):
                return True
        if args.line > 0 and args.line in args.clang_standard_lines:
            return True
        return False

    @dataclass(frozen=True)
    class GroupHeadingArgs:
        group_name: str
        titles: dict[str, str]
        groups: dict[str, list[dict[str, object]]]

    def _group_heading(self, args: "IncludeOrderPolicy.GroupHeadingArgs") -> list[str]:
        sep_len = int(self._config.get("separator_length", 64))
        sep = "//" + "-" * max(0, sep_len - 2)

        title = args.titles.get(args.group_name, args.group_name)
        if args.group_name == "third_party":
            labels = self._third_party_labels(args.groups[args.group_name])
            if labels:
                title = f"{title}: {', '.join(sorted(labels))}"

        return [f"{sep}\n", f"// {title}\n", f"{sep}\n"]

    def _third_party_labels(self, items: list[dict[str, object]]) -> set[str]:
        mapping = self._config.get("third_party_labels", {})
        labels: set[str] = set()
        for item in items:
            header = str(item.get("header", ""))
            prefix = header.split("/", 1)[0]
            label = mapping.get(prefix, prefix)
            labels.add(str(label))
        return labels

    def _main_header_candidates(self, path: str) -> set[str]:
        stem = Path(path).stem
        exts = self._config.get("main_header_extensions", [".hpp", ".h", ".hh", ".hxx"])
        return {f"{stem}{ext}" for ext in exts}

    @dataclass(frozen=True)
    class ProjectHeaderArgs:
        header: str
        project_headers: set[str]
        project_prefixes: tuple[str, ...]

    def _is_project_header(self, args: "IncludeOrderPolicy.ProjectHeaderArgs") -> bool:
        if args.header in args.project_headers:
            return True
        for prefix in args.project_prefixes:
            if args.header.startswith(prefix):
                return True
        return False
