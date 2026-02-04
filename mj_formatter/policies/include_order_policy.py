from __future__ import annotations

import re
from pathlib import Path

from ..core.edit import Edit
from ..core.parse_context import ParseContext
from ..core.policy_result import PolicyResult
from ..core.violation import Violation
from .policy_base import Policy


class IncludeOrderPolicy(Policy):
    name = "include_order"
    description = "Order includes into groups with headings"

    _include_re = re.compile(r"^\s*#\s*include\s*[<\"]([^>\"]+)[>\"]")

    def apply(self, context: ParseContext) -> PolicyResult:
        lines = context.text.splitlines(keepends=True)
        if not lines:
            return PolicyResult(text=context.text, violations=[], edits=[])

        first_include = self._find_first_include(lines)
        if first_include is None:
            return PolicyResult(text=context.text, violations=[], edits=[])

        start = first_include
        end = self._find_include_region_end(lines, start)

        include_lines = lines[start:end]
        includes = self._extract_includes(include_lines)
        if not includes:
            return PolicyResult(text=context.text, violations=[], edits=[])

        line_ending = self._detect_line_ending(context.text)
        ordered_block = self._build_ordered_block(context.path, includes, line_ending)
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

    def _find_first_include(self, lines: list[str]) -> int | None:
        for idx, line in enumerate(lines):
            if self._include_re.match(line):
                return idx
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

    def _build_ordered_block(self, path: str, includes: list[dict[str, str]], line_ending: str) -> list[str]:
        is_header = Path(path).suffix.lower() in {".h", ".hpp", ".hh", ".hxx"}

        groups: dict[str, list[dict[str, str]]] = {
            "main": [],
            "standard": [],
            "third_party": [],
            "project": [],
            "local": [],
        }

        main_candidates = self._main_header_candidates(path)
        standard_headers = set(self._config.get("standard_headers", []) or self._default_standard_headers())
        project_headers = set(self._config.get("project_headers", []) or [])
        project_prefixes = tuple(self._config.get("project_prefixes", []) or [])

        for item in includes:
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

            if self._is_project_header(header, project_headers, project_prefixes):
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
                "standard": "Standard headers",
                "third_party": "Third-party headers",
                "project": "Project headers",
                "local": "Local headers",
            },
        )

        output: list[str] = []
        for group_name in order:
            items = groups.get(group_name, [])
            if not items:
                continue
            output.extend(self._group_heading(group_name, titles, groups))
            for inc in sorted(items, key=lambda i: i["header"].lower()):
                if inc["quote"] == "<":
                    output.append(f"#include <{inc['header']}>{line_ending}")
                else:
                    output.append(f"#include \"{inc['header']}\"{line_ending}")
            output.append(line_ending)

        if output:
            output.pop()
        return output

    def _group_heading(
        self,
        group_name: str,
        titles: dict[str, str],
        groups: dict[str, list[dict[str, str]]],
    ) -> list[str]:
        sep_len = int(self._config.get("separator_length", 64))
        sep = "//" + "-" * max(0, sep_len - 2)

        title = titles.get(group_name, group_name)
        if group_name == "third_party":
            labels = self._third_party_labels(groups[group_name])
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

    def _is_project_header(self, header: str, project_headers: set[str], project_prefixes: tuple[str, ...]) -> bool:
        if header in project_headers:
            return True
        for prefix in project_prefixes:
            if header.startswith(prefix):
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
