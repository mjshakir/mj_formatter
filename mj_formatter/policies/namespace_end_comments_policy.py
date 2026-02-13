from __future__ import annotations

import re
from typing import Any

from ..core.types import CodeBlock
from ..core.types import Edit
from ..core.types import ParseContext
from ..core.types import PolicyResult
from ..core.types import Violation
from .policy_base import Policy


class NamespaceEndCommentsPolicy(Policy):
    name = "namespace_end_comments"
    description = "Add canonical // end ... comments on closing braces"
    parse_mode = "tree_sitter"
    requires_code_context = True

    def apply(self, context: ParseContext) -> PolicyResult:
        blocks = self._resolve_blocks(context)
        if not blocks:
            return PolicyResult(text=context.text, violations=[], edits=[])

        configured_blocks = {
            str(item).strip().lower()
            for item in (self._config.get("blocks", []) or [])
            if str(item).strip()
        }
        max_named_lines = int(self._config.get("max_named_lines", 40))
        max_label_length = int(self._config.get("max_label_length", 96))
        replace_existing = bool(self._config.get("replace_existing", True))

        text = context.text
        lines = text.splitlines(keepends=True)
        line_ranges = self._line_byte_ranges(text)

        replacements: list[tuple[int, int, str]] = []
        violations: list[Violation] = []

        for block in blocks:
            kind = str(block.kind).strip().lower()
            if configured_blocks and kind not in configured_blocks:
                continue
            line_idx = int(block.close_line) - 1
            if line_idx < 0 or line_idx >= len(lines):
                continue
            line_text = lines[line_idx]
            if "}" not in line_text:
                continue

            span_lines = max(0, int(block.close_line) - int(block.open_line))
            label = self._select_label(block, span_lines, max_named_lines, max_label_length)
            expected = f"// end {label}"

            line_start, line_end = line_ranges[line_idx]
            comment_idx = line_text.find("//")
            if comment_idx >= 0:
                existing = line_text[comment_idx:].strip()
                if existing == expected:
                    if comment_idx > 0 and line_text[comment_idx - 1] != " " and replace_existing:
                        replacements.append((line_start + comment_idx, line_end, f" {expected}"))
                        violations.append(
                            Violation(
                                policy=self.name,
                                message=f"Normalize end comment spacing for {kind}",
                                line=line_idx + 1,
                                column=1,
                            )
                        )
                    continue
                if not replace_existing:
                    continue
                replace_start_idx = comment_idx
                while replace_start_idx > 0 and line_text[replace_start_idx - 1] == " ":
                    replace_start_idx -= 1
                if replace_start_idx > 0 and line_text[replace_start_idx - 1] == "}":
                    replacements.append((line_start + replace_start_idx, line_end, f" {expected}"))
                else:
                    replacements.append((line_start + comment_idx, line_end, expected))
                violations.append(
                    Violation(
                        policy=self.name,
                        message=f"Fix end comment for {kind}",
                        line=line_idx + 1,
                        column=1,
                    )
                )
            else:
                replacements.append((line_end, line_end, f" {expected}"))
                violations.append(
                    Violation(
                        policy=self.name,
                        message=f"Missing end comment for {kind}",
                        line=line_idx + 1,
                        column=1,
                    )
                )

        if not replacements:
            return PolicyResult(text=text, violations=[], edits=[])

        data = text.encode("utf-8")
        for start, end, repl in sorted(replacements, key=lambda item: item[0], reverse=True):
            data = data[:start] + repl.encode("utf-8") + data[end:]
        updated = data.decode("utf-8")

        edits: list[Edit] = []
        if updated != text:
            updated_lines = updated.splitlines(keepends=True)
            for idx, (before, after) in enumerate(zip(lines, updated_lines)):
                if before != after:
                    edits.append(
                        Edit(
                            policy=self.name,
                            line=idx + 1,
                            before=before.rstrip("\r\n"),
                            after=after.rstrip("\r\n"),
                        )
                    )
        return PolicyResult(text=updated, violations=violations, edits=edits)

    def _resolve_blocks(self, context: ParseContext) -> tuple[CodeBlock, ...]:
        code_context = getattr(context, "code_context", None)
        if code_context is not None:
            blocks = tuple(getattr(code_context, "hybrid_blocks", ()) or ())
            if blocks:
                return blocks
        return self._fallback_tree_blocks(context)

    def _fallback_tree_blocks(self, context: ParseContext) -> tuple[CodeBlock, ...]:
        tree = getattr(context, "tree_sitter_tree", None)
        root = getattr(tree, "root_node", None)
        if root is None:
            return ()
        data = context.text.encode("utf-8", errors="ignore")
        seen: set[tuple[int, int]] = set()
        blocks: list[CodeBlock] = []
        stack = [root]
        while stack:
            node = stack.pop()
            body = self._body_node(node)
            if body is not None:
                kind = self._canonical_kind(getattr(node, "type", ""))
                if kind:
                    start = int(getattr(node, "start_byte", 0))
                    end = int(getattr(body, "end_byte", 0))
                    key = (start, end)
                    if key not in seen:
                        seen.add(key)
                        header = self._normalize_space(data[start:int(body.start_byte)].decode("utf-8", errors="ignore"))
                        if not header:
                            header = kind
                        kind = self._kind_from_header(kind, header)
                        short = f"{kind}(...)"
                        if kind in {"namespace", "class", "struct"}:
                            name = self._extract_name(node, data)
                            header_name = self._name_from_header(kind, header)
                            if header_name:
                                name = header_name
                            if name:
                                short = f"{kind} {name}"
                        elif kind == "function":
                            name = self._extract_name(node, data)
                            header_name = self._name_from_header(kind, header)
                            if header_name:
                                name = header_name
                            if name:
                                short = f"{name}(...)"
                        blocks.append(
                            CodeBlock(
                                kind=kind,
                                label=header,
                                short_label=short,
                                start=start,
                                end=end,
                                open_line=int(node.start_point[0]) + 1,
                                close_line=int(body.end_point[0]) + 1,
                                source="tree",
                                confidence=0.70,
                            )
                        )
            stack.extend(reversed(getattr(node, "children", [])))
        return tuple(blocks)

    def _body_node(self, node: Any) -> Any | None:
        for child in getattr(node, "children", []):
            if getattr(child, "type", "") in {"compound_statement", "field_declaration_list", "declaration_list"}:
                return child
        return None

    def _kind_from_header(self, kind: str, header: str) -> str:
        normalized = str(header or "").lstrip()
        lowered = normalized.lower()
        if lowered.startswith("class "):
            return "class"
        if lowered.startswith("struct "):
            return "struct"
        if lowered.startswith("namespace "):
            return "namespace"
        return kind

    def _name_from_header(self, kind: str, header: str) -> str | None:
        text = str(header or "").strip()
        if not text:
            return None
        if kind in {"class", "struct"}:
            match = re.search(r"\b(?:class|struct)\b\s+(.+)$", text)
            if not match:
                return None
            tokens = re.findall(r"[A-Za-z_]\w*", match.group(1))
            if not tokens:
                return None
            return tokens[-1]
        if kind == "namespace":
            match = re.search(r"\bnamespace\b\s+(.+)$", text)
            if not match:
                return None
            tokens = re.findall(r"[A-Za-z_]\w*", match.group(1))
            if not tokens:
                return None
            return tokens[-1]
        if kind == "function":
            match = re.search(r"([~A-Za-z_]\w*(?:::[A-Za-z_]\w*)*)\s*\(", text)
            if not match:
                return None
            name = str(match.group(1) or "")
            if "::" in name:
                name = name.split("::")[-1]
            return name or None
        return None

    def _canonical_kind(self, node_type: str) -> str:
        raw = str(node_type or "").strip().lower()
        if not raw:
            return ""
        tokens = [item for item in raw.split("_") if item]
        if not tokens:
            return ""
        return tokens[0]

    def _extract_name(self, node: Any, data: bytes) -> str | None:
        node_type = str(getattr(node, "type", "") or "")
        stop_types = {"compound_statement", "field_declaration_list", "declaration_list"}
        if node_type == "namespace_definition":
            name_node = self._first_descendant(
                node,
                {"namespace_identifier", "identifier"},
                stop_types,
            )
            if name_node is not None:
                name = data[name_node.start_byte:name_node.end_byte].decode("utf-8", errors="ignore")
                if name:
                    return name
        if node_type in {"class_specifier", "struct_specifier"}:
            name_node = self._first_descendant(node, {"type_identifier"}, stop_types)
            if name_node is not None:
                name = data[name_node.start_byte:name_node.end_byte].decode("utf-8", errors="ignore")
                if name:
                    return name
        if node_type in {"function_definition", "function_declaration"}:
            declarator = self._first_descendant(node, {"function_declarator"}, stop_types)
            if declarator is not None:
                name_node = self._first_descendant(
                    declarator,
                    {"identifier", "field_identifier", "type_identifier"},
                    {"parameter_list", "template_parameter_list"},
                )
                if name_node is not None:
                    name = data[name_node.start_byte:name_node.end_byte].decode("utf-8", errors="ignore")
                    if name:
                        return name

        stack = [node]
        while stack:
            current = stack.pop()
            current_type = getattr(current, "type", "")
            if current is not node and current_type in {"identifier", "type_identifier", "namespace_identifier", "field_identifier"}:
                name = data[current.start_byte:current.end_byte].decode("utf-8", errors="ignore")
                return name or None
            if current is not node and current_type in {"compound_statement", "field_declaration_list", "declaration_list"}:
                continue
            stack.extend(reversed(getattr(current, "children", [])))
        return None

    def _first_descendant(self, node: Any, target_types: set[str], stop_types: set[str]) -> Any | None:
        stack = [node]
        while stack:
            current = stack.pop()
            current_type = getattr(current, "type", "")
            if current is not node and current_type in target_types:
                return current
            if current is not node and current_type in stop_types:
                continue
            stack.extend(reversed(getattr(current, "children", [])))
        return None

    def _select_label(
        self,
        block: CodeBlock,
        span_lines: int,
        max_named_lines: int,
        max_label_length: int,
    ) -> str:
        label = self._normalize_space(block.label)
        short = self._normalize_space(block.short_label) or f"{block.kind}(...)"
        if block.kind in {"class", "struct", "namespace"}:
            return short
        if block.kind == "function":
            short = self._function_short_label(label, short)
        label = self._normalize_control_label(block.kind, label)
        if not label:
            return short
        if span_lines > max_named_lines:
            return short
        if len(label) > max_label_length:
            return short
        return label

    def _function_short_label(self, label: str, fallback: str) -> str:
        match = re.search(r"([~A-Za-z_]\w*(?:::[A-Za-z_]\w*)*)\s*\(", label)
        if not match:
            return fallback
        name = str(match.group(1) or "")
        if "::" in name:
            name = name.split("::")[-1]
        if not name:
            return fallback
        return f"{name}(...)"

    def _normalize_control_label(self, kind: str, label: str) -> str:
        control = {"if", "while", "for", "switch", "catch"}
        if kind not in control:
            return label
        return re.sub(rf"^{kind}\(", f"{kind} (", label)

    def _line_byte_ranges(self, text: str) -> list[tuple[int, int]]:
        ranges: list[tuple[int, int]] = []
        offset = 0
        for line in text.encode("utf-8").splitlines(keepends=True):
            end = len(line.rstrip(b"\r\n"))
            ranges.append((offset, offset + end))
            offset += len(line)
        if not ranges:
            ranges.append((0, 0))
        return ranges

    def _normalize_space(self, text: str) -> str:
        return " ".join(str(text or "").strip().split())
