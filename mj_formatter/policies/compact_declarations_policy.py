from __future__ import annotations

from dataclasses import dataclass

from ..core.types import Edit
from ..core.types import ParseContext
from ..core.types import PolicyResult
from ..core.types import Violation
from ..core.utilities import warn_once
from .policy_base import Policy


class CompactDeclarationsPolicy(Policy):
    name = "compact_declarations"
    description = "Compact adjacent same-type declarations into a single declaration"
    parse_mode = "tree_sitter"
    requires_code_context = False

    @dataclass(frozen=True)
    class _Candidate:
        start: int
        end: int
        line: int
        node_type: str
        indent: str
        type_prefix: str
        name: str

    def apply(self, context: ParseContext) -> PolicyResult:
        tree = context.tree_sitter_tree
        if tree is None:
            warn_once(
                "compact_declarations_parser_unavailable",
                "compact_declarations: tree-sitter unavailable, skipping policy",
            )
            return PolicyResult(text=context.text, violations=[], edits=[])

        root = getattr(tree, "root_node", None)
        if root is None:
            return PolicyResult(text=context.text, violations=[], edits=[])

        min_group_size = max(2, int(self._config.get("min_group_size", 3)))
        data = context.text.encode("utf-8", errors="ignore")

        replacements: list[tuple[int, int, str, int]] = []
        stack = [root]
        while stack:
            node = stack.pop()
            replacements.extend(self._collect_parent_replacements(node, data, min_group_size))
            stack.extend(reversed(getattr(node, "children", [])))

        if not replacements:
            return PolicyResult(text=context.text, violations=[], edits=[])

        updated_data = data
        for start, end, replacement, _ in sorted(replacements, key=lambda item: item[0], reverse=True):
            updated_data = updated_data[:start] + replacement.encode("utf-8") + updated_data[end:]
        updated = updated_data.decode("utf-8")

        if updated == context.text:
            return PolicyResult(text=context.text, violations=[], edits=[])

        violations = [
            Violation(
                policy=self.name,
                message="Compacted adjacent same-type declarations",
                line=line,
                column=1,
            )
            for _, _, _, line in replacements
        ]
        edits = self._line_edits(context.text, updated)
        return PolicyResult(text=updated, violations=violations, edits=edits)

    def _collect_parent_replacements(
        self,
        node: object,
        data: bytes,
        min_group_size: int,
    ) -> list[tuple[int, int, str, int]]:
        children = list(getattr(node, "children", []))
        if len(children) < min_group_size:
            return []

        replacements: list[tuple[int, int, str, int]] = []
        idx = 0
        while idx < len(children):
            first = self._candidate(children[idx], data)
            if first is None:
                idx += 1
                continue

            group = [first]
            j = idx + 1
            while j < len(children):
                nxt = self._candidate(children[j], data)
                if nxt is None or not self._can_group(group[-1], nxt, data):
                    break
                group.append(nxt)
                j += 1

            if len(group) >= min_group_size:
                replacement = self._build_replacement(group, data)
                if replacement is not None:
                    replacements.append(replacement)
                idx = j
            else:
                idx += 1
        return replacements

    def _candidate(self, node: object, data: bytes) -> _Candidate | None:
        node_type = str(getattr(node, "type", "") or "")
        if node_type not in {"declaration", "field_declaration"}:
            return None

        start = int(getattr(node, "start_byte", 0) or 0)
        end = int(getattr(node, "end_byte", 0) or 0)
        if end <= start:
            return None

        raw = data[start:end].decode("utf-8", errors="ignore")
        if "\n" in raw or "\r" in raw:
            return None
        if not raw.strip().endswith(";"):
            return None
        if "//" in raw or "/*" in raw:
            return None

        stripped = raw.strip()
        core = stripped[:-1].rstrip()
        if not core:
            return None

        # Keep this conservative to avoid semantic changes.
        banned = (",", "=", "(", ")", "{", "}", "[", "]", ":", "->")
        if any(token in core for token in banned):
            return None
        if "*" in core or "&" in core:
            return None

        parts = core.split()
        if len(parts) < 2:
            return None
        name = parts[-1]
        if not name or not (name[0].isalpha() or name[0] == "_"):
            return None
        if not all(ch.isalnum() or ch == "_" for ch in name):
            return None

        name_idx = core.rfind(name)
        if name_idx <= 0:
            return None
        type_prefix = core[:name_idx].rstrip()
        if not type_prefix:
            return None

        indent_len = len(raw) - len(raw.lstrip(" \t"))
        indent = raw[:indent_len]
        line = int(getattr(node, "start_point", (0, 0))[0]) + 1
        return CompactDeclarationsPolicy._Candidate(
            start=start,
            end=end,
            line=line,
            node_type=node_type,
            indent=indent,
            type_prefix=type_prefix,
            name=name,
        )

    def _can_group(self, left: _Candidate, right: _Candidate, data: bytes) -> bool:
        if left.node_type != right.node_type:
            return False
        if left.indent != right.indent:
            return False
        if left.type_prefix != right.type_prefix:
            return False
        between = data[left.end:right.start].decode("utf-8", errors="ignore")
        return between.strip() == ""

    def _build_replacement(
        self,
        group: list[_Candidate],
        data: bytes,
    ) -> tuple[int, int, str, int] | None:
        names = [item.name for item in group]
        replacement = f"{group[0].indent}{group[0].type_prefix} {', '.join(names)};"
        start = group[0].start
        end = group[-1].end
        current = data[start:end].decode("utf-8", errors="ignore")
        if current == replacement:
            return None
        return start, end, replacement, group[0].line

    def _line_edits(self, before: str, after: str) -> list[Edit]:
        edits: list[Edit] = []
        before_lines = before.splitlines(keepends=True)
        after_lines = after.splitlines(keepends=True)
        shared = min(len(before_lines), len(after_lines))
        for idx in range(shared):
            if before_lines[idx] == after_lines[idx]:
                continue
            edits.append(
                Edit(
                    policy=self.name,
                    line=idx + 1,
                    before=before_lines[idx].rstrip("\r\n"),
                    after=after_lines[idx].rstrip("\r\n"),
                )
            )
        if len(before_lines) == len(after_lines):
            return edits
        tail_before = before_lines[shared:]
        tail_after = after_lines[shared:]
        tail_count = max(len(tail_before), len(tail_after))
        for offset in range(tail_count):
            before_line = tail_before[offset] if offset < len(tail_before) else ""
            after_line = tail_after[offset] if offset < len(tail_after) else ""
            edits.append(
                Edit(
                    policy=self.name,
                    line=shared + offset + 1,
                    before=before_line.rstrip("\r\n"),
                    after=after_line.rstrip("\r\n"),
                )
            )
        return edits
