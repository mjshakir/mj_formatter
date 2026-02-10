from __future__ import annotations

import re
from pathlib import Path
from typing import Any

from ..core.edit import Edit
from ..core.parse_context import ParseContext
from ..core.policy_result import PolicyResult
from ..core.violation import Violation
from .policy_base import Policy
from dataclasses import dataclass


class IncludeOrderPolicy(Policy):
    name = "include_order"
    description = "Order includes into groups with headings"
    parse_mode = "tree_sitter"

    _include_re = re.compile(r"^\s*#\s*include\s*[<\"]([^>\"]+)[>\"]")

    def apply(self, context: ParseContext) -> PolicyResult:
        lines = context.text.splitlines(keepends=True)
        if not lines:
            return PolicyResult(text=context.text, violations=[], edits=[])

        start = None
        end = None
        includes: list[dict[str, str]] = []
        if context.tree_sitter_tree is not None:
            includes, start, end = self._extract_includes_tree(context.text, lines, context.tree_sitter_tree)

        if not includes:
            first_include = self._find_first_include(lines)
            if first_include is None:
                return PolicyResult(text=context.text, violations=[], edits=[])
            start = first_include
            end = self._find_include_region_end(lines, start)
            include_lines = lines[start:end]
            includes = self._extract_includes(include_lines)
        if not includes:
            return PolicyResult(text=context.text, violations=[], edits=[])

        include_lines = lines[start:end]
        line_ending = self._detect_line_ending(context.text)
        ordered_block = self._build_ordered_block(
            IncludeOrderPolicy.BuildOrderedBlockArgs(
                path=context.path,
                includes=includes,
                line_ending=line_ending,
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
    ) -> tuple[list[dict[str, str]], int | None, int | None]:
        root = getattr(tree, "root_node", None)
        if root is None:
            return [], None, None
        data = text.encode("utf-8")
        nodes = []
        stack = [root]
        while stack:
            node = stack.pop()
            if node.type == "preproc_include":
                nodes.append(node)
            stack.extend(reversed(node.children))
        if not nodes:
            return [], None, None

        includes: list[dict[str, str]] = []
        line_indices: list[int] = []
        for node in nodes:
            snippet = data[node.start_byte : node.end_byte].decode("utf-8", errors="ignore")
            match = self._include_re.match(snippet)
            if not match:
                continue
            header = match.group(1)
            quote = "\"" if "\"" in snippet else "<"
            includes.append({"header": header, "quote": quote})
            line_indices.append(node.start_point[0])

        if not includes:
            return [], None, None

        first = min(line_indices)
        last = max(line_indices)
        start = self._expand_include_start(lines, first)
        end = self._expand_include_end(lines, last + 1)
        return includes, start, end

    def _expand_include_start(self, lines: list[str], start: int) -> int:
        idx = start
        while idx > 0 and self._is_comment_or_blank(lines[idx - 1]):
            if lines[idx - 1].strip().startswith("#pragma once"):
                break
            idx -= 1
        return idx

    def _expand_include_end(self, lines: list[str], end: int) -> int:
        idx = end
        while idx < len(lines):
            if self._include_re.match(lines[idx]) or self._is_comment_or_blank(lines[idx]):
                idx += 1
                continue
            break
        return idx

    def _find_first_include(self, lines: list[str]) -> int | None:
        for idx, line in enumerate(lines):
            if self._include_re.match(line):
                start = idx
                while start > 0 and self._is_comment_or_blank(lines[start - 1]):
                    if lines[start - 1].strip().startswith("#pragma once"):
                        break
                    start -= 1
                return start
            if not self._is_comment_or_blank(line):
                return None
        return None

    def _find_include_region_end(self, lines: list[str], start: int) -> int:
        idx = start
        while idx < len(lines):
            if self._include_re.match(lines[idx]) or self._is_comment_or_blank(lines[idx]):
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

    def _extract_includes(self, lines: list[str]) -> list[dict[str, str]]:
        includes: list[dict[str, str]] = []
        for line in lines:
            match = self._include_re.match(line)
            if not match:
                continue
            header = match.group(1)
            quote = "\"" if "\"" in line else "<"
            includes.append({"header": header, "quote": quote})
        return includes

    @dataclass(frozen=True)
    class BuildOrderedBlockArgs:
        path: str
        includes: list[dict[str, str]]
        line_ending: str

    def _build_ordered_block(self, args: "IncludeOrderPolicy.BuildOrderedBlockArgs") -> list[str]:
        is_header = Path(args.path).suffix.lower() in {".h", ".hpp", ".hh", ".hxx"}

        groups: dict[str, list[dict[str, str]]] = {
            "main": [],
            "standard": [],
            "third_party": [],
            "project": [],
            "local": [],
        }

        main_candidates = self._main_header_candidates(args.path)
        standard_headers = set(self._config.get("standard_headers", []) or self._default_standard_headers())
        project_headers = set(self._config.get("project_headers", []) or [])
        project_prefixes = tuple(self._config.get("project_prefixes", []) or [])

        for item in args.includes:
            header = item["header"]
            quote = item["quote"]

            if not is_header and header in main_candidates:
                groups["main"].append(item)
                continue

            if quote == "<":
                if header in standard_headers:
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
                items = sorted(items, key=lambda i: i["header"].lower())
            for inc in items:
                if inc["quote"] == "<":
                    output.append(f"#include <{inc['header']}>{args.line_ending}")
                else:
                    output.append(f"#include \"{inc['header']}\"{args.line_ending}")
            output.append(args.line_ending)

        if output:
            output.pop()
        return output

    @dataclass(frozen=True)
    class GroupHeadingArgs:
        group_name: str
        titles: dict[str, str]
        groups: dict[str, list[dict[str, str]]]

    def _group_heading(self, args: "IncludeOrderPolicy.GroupHeadingArgs") -> list[str]:
        sep_len = int(self._config.get("separator_length", 64))
        sep = "//" + "-" * max(0, sep_len - 2)

        title = args.titles.get(args.group_name, args.group_name)
        if args.group_name == "third_party":
            labels = self._third_party_labels(args.groups[args.group_name])
            if labels:
                title = f"{title}: {', '.join(sorted(labels))}"

        return [f"{sep}\n", f"// {title}\n", f"{sep}\n"]

    def _third_party_labels(self, items: list[dict[str, str]]) -> set[str]:
        mapping = self._config.get("third_party_labels", {})
        labels: set[str] = set()
        for item in items:
            header = item["header"]
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

    def _default_standard_headers(self) -> list[str]:
        return [
            "algorithm",
            "array",
            "atomic",
            "bit",
            "cassert",
            "cctype",
            "cerrno",
            "cfenv",
            "cfloat",
            "chrono",
            "cinttypes",
            "climits",
            "cmath",
            "csetjmp",
            "csignal",
            "cstdarg",
            "cstddef",
            "cstdint",
            "cstdio",
            "cstdlib",
            "cstring",
            "ctime",
            "cwchar",
            "exception",
            "functional",
            "initializer_list",
            "iomanip",
            "ios",
            "iosfwd",
            "iostream",
            "istream",
            "iterator",
            "limits",
            "list",
            "map",
            "memory",
            "mutex",
            "new",
            "optional",
            "ostream",
            "queue",
            "set",
            "sstream",
            "stdexcept",
            "string",
            "string_view",
            "thread",
            "type_traits",
            "unordered_map",
            "unordered_set",
            "utility",
            "vector",
        ]
