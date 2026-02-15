from __future__ import annotations

from functools import lru_cache
from pathlib import Path
from typing import Any, Literal

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
    _REQUIRED_GROUPS: tuple[str, ...] = ("main", "standard", "third_party", "project", "local")

    def __init__(self, config: dict[str, object]) -> None:
        super().__init__(config)
        self._order_header = self._required_str_tuple("order_header")
        self._order_source = self._required_str_tuple("order_source")
        self._standard_headers = self._required_str_tuple("standard_headers")
        self._standard_prefixes = self._required_str_tuple("standard_prefixes")
        self._project_headers = self._required_str_tuple("project_headers")
        self._project_prefixes = self._required_str_tuple("project_prefixes")
        self._main_header_extensions = self._required_str_tuple("main_header_extensions")
        self._standard_path_markers = self._required_str_tuple("standard_header_path_markers")
        self._clang_include_prefix = self._required_str("clang_builtin_include_prefix")
        self._include_segment = self._required_str("include_path_segment")
        self._separator_length = self._required_int("separator_length")
        self._third_party_label_map = self._required_mapping("third_party_labels")
        group_titles_raw = self._required_mapping("group_titles")
        missing = [group for group in self._REQUIRED_GROUPS if group not in group_titles_raw]
        if missing:
            missing_fmt = ", ".join(sorted(missing))
            raise ValueError(f"include_order: missing group_titles keys: {missing_fmt}")
        self._group_titles = {str(key): str(value) for key, value in group_titles_raw.items()}

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
            path=context.path,
            includes=includes,
            line_ending=line_ending,
            clang_standard_lines=clang_standard_lines,
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

    def _extract_header_from_snippet(self, snippet: str) -> tuple[str, Literal["<", '"']] | None:
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

    def _build_ordered_block(
        self,
        *,
        path: str,
        includes: list[dict[str, object]],
        line_ending: str,
        clang_standard_lines: set[int],
    ) -> list[str]:
        is_header = Path(path).suffix.lower() in {".h", ".hpp", ".hh", ".hxx"}

        groups: dict[str, list[dict[str, object]]] = {
            "main": [],
            "standard": [],
            "third_party": [],
            "project": [],
            "local": [],
        }

        main_candidates = self._main_header_candidates(path)
        standard_headers = set(self._standard_headers)
        standard_prefixes = self._standard_prefixes
        project_headers = set(self._project_headers)
        project_prefixes = self._project_prefixes

        for item in includes:
            header = str(item.get("header", ""))
            quote = str(item.get("quote", ""))
            line = int(item.get("line", 0) or 0)

            if not is_header and header in main_candidates:
                groups["main"].append(item)
                continue

            if quote == "<":
                if self._is_standard_header(
                    header=header,
                    line=line,
                    standard_headers=standard_headers,
                    standard_prefixes=standard_prefixes,
                    clang_standard_lines=clang_standard_lines,
                ):
                    groups["standard"].append(item)
                else:
                    groups["third_party"].append(item)
                continue

            if self._is_project_header(
                header=header,
                project_headers=project_headers,
                project_prefixes=project_prefixes,
            ):
                groups["project"].append(item)
            else:
                groups["local"].append(item)

        order = self._order_header if is_header else self._order_source

        output: list[str] = []
        for group_name in order:
            items = groups.get(group_name, [])
            if not items:
                continue
            output.extend(self._group_heading(group_name=group_name, groups=groups))
            if len(items) > 1:
                items = sorted(items, key=lambda i: str(i.get("header", "")).lower())
            for inc in items:
                header = str(inc.get("header", ""))
                quote = str(inc.get("quote", ""))
                if quote == "<":
                    output.append(f"#include <{header}>{line_ending}")
                else:
                    output.append(f"#include \"{header}\"{line_ending}")
            output.append(line_ending)

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
        return self._normalized_path_cached(value)

    @staticmethod
    @lru_cache(maxsize=4096)
    def _normalized_path_cached(value: str) -> str:
        if not value:
            return ""
        try:
            return str(Path(value).resolve())
        except Exception:
            return str(value)

    def _is_probably_standard_header_from_path(self, include_path: str) -> bool:
        return self._is_probably_standard_header_from_path_cached(
            include_path,
            self._standard_path_markers,
            self._clang_include_prefix,
            self._include_segment,
        )

    @staticmethod
    @lru_cache(maxsize=4096)
    def _is_probably_standard_header_from_path_cached(
        include_path: str,
        standard_path_markers: tuple[str, ...],
        clang_include_prefix: str,
        include_segment: str,
    ) -> bool:
        normalized = include_path.replace("\\", "/").lower()
        if any(marker.lower() in normalized for marker in standard_path_markers):
            return True
        if not clang_include_prefix or not include_segment:
            return False
        return clang_include_prefix.lower() in normalized and include_segment.lower() in normalized

    def _is_standard_header(
        self,
        *,
        header: str,
        line: int,
        standard_headers: set[str],
        standard_prefixes: tuple[str, ...],
        clang_standard_lines: set[int],
    ) -> bool:
        if header in standard_headers:
            return True
        for prefix in standard_prefixes:
            if header.startswith(prefix):
                return True
        if line > 0 and line in clang_standard_lines:
            return True
        return False

    def _group_heading(
        self,
        *,
        group_name: str,
        groups: dict[str, list[dict[str, object]]],
    ) -> list[str]:
        sep = "//" + "-" * max(0, int(self._separator_length) - 2)

        title = self._group_titles.get(group_name, group_name)
        if group_name == "third_party":
            labels = self._third_party_labels(groups[group_name])
            if labels:
                title = f"{title}: {', '.join(sorted(labels))}"

        return [f"{sep}\n", f"// {title}\n", f"{sep}\n"]

    def _third_party_labels(self, items: list[dict[str, object]]) -> set[str]:
        mapping = self._third_party_label_map
        labels: set[str] = set()
        for item in items:
            header = str(item.get("header", ""))
            prefix = header.split("/", 1)[0]
            label = mapping.get(prefix, prefix) if isinstance(mapping, dict) else prefix
            labels.add(str(label))
        return labels

    def _main_header_candidates(self, path: str) -> set[str]:
        stem = Path(path).stem
        return {f"{stem}{ext}" for ext in self._main_header_extensions}

    def _is_project_header(
        self,
        *,
        header: str,
        project_headers: set[str],
        project_prefixes: tuple[str, ...],
    ) -> bool:
        if header in project_headers:
            return True
        for prefix in project_prefixes:
            if header.startswith(prefix):
                return True
        return False

    def _required_str(self, key: str) -> str:
        value = self._config.get(key)
        if value is None:
            raise ValueError(f"include_order: missing required config key '{key}'")
        text = str(value).strip()
        if not text:
            raise ValueError(f"include_order: empty required config key '{key}'")
        return text

    def _required_int(self, key: str) -> int:
        value = self._config.get(key)
        if value is None:
            raise ValueError(f"include_order: missing required config key '{key}'")
        try:
            parsed = int(value)
        except Exception as exc:
            raise ValueError(f"include_order: invalid integer for '{key}': {value!r}") from exc
        if parsed <= 0:
            raise ValueError(f"include_order: '{key}' must be > 0")
        return parsed

    def _required_mapping(self, key: str) -> dict[str, object]:
        value = self._config.get(key)
        if not isinstance(value, dict):
            raise ValueError(f"include_order: missing required mapping config key '{key}'")
        return dict(value)

    def _required_str_tuple(self, key: str) -> tuple[str, ...]:
        value = self._config.get(key)
        if not isinstance(value, (list, tuple)):
            raise ValueError(f"include_order: missing required list config key '{key}'")
        items = tuple(str(item).strip() for item in value if str(item).strip())
        return items
