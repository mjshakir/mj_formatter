from __future__ import annotations

import re
from dataclasses import dataclass

from ..core.edit import Edit
from ..core.parse_context import ParseContext
from ..core.policy_result import PolicyResult
from ..core.violation import Violation
from .policy_base import Policy


@dataclass(frozen=True)
class _Decl:
    name: str
    kind: str  # local | member | global | function | class | struct | namespace | macro
    is_static: bool = False
    is_const: bool = False
    is_constexpr: bool = False
    is_consteval: bool = False
    is_pointer: bool = False
    smart_ptr: str | None = None  # shared | unique | weak
    scope_name: str | None = None
    line: int = 0


class NamingConventionsPolicy(Policy):
    name = "naming_conventions"
    description = "Enforce naming conventions with prefixes"
    parse_mode = "text"

    _control_keywords = {"if", "for", "while", "switch", "catch"}
    _skip_name_keywords = {
        "if",
        "for",
        "while",
        "switch",
        "catch",
        "return",
        "sizeof",
        "alignof",
        "operator",
    }
    _builtin_types = {
        "void",
        "bool",
        "char",
        "short",
        "int",
        "long",
        "float",
        "double",
        "signed",
        "unsigned",
        "size_t",
        "ssize_t",
        "auto",
    }

    def __init__(self, config: dict[str, object]) -> None:
        super().__init__(config)
        self._standard = str(self._config.get("standard", "mj")).lower()
        self._standards = self._build_standards()
        self._rules = self._standards.get(self._standard, self._standards["mj"])

    def apply(self, context: ParseContext) -> PolicyResult:
        text = context.text
        if not text:
            return PolicyResult(text=text, violations=[], edits=[])

        code_mask = self._code_mask(text)
        code_text = self._masked_text(text, code_mask)
        lines = text.splitlines(keepends=True)
        code_lines = code_text.splitlines(keepends=True)

        decls = self._collect_declarations(code_lines)
        if not decls:
            return PolicyResult(text=text, violations=[], edits=[])

        rename_map, conflicts = self._build_rename_map(decls)
        if not rename_map and not conflicts:
            return PolicyResult(text=text, violations=[], edits=[])

        updated, edits = self._apply_renames(lines, code_mask, rename_map)

        violations: list[Violation] = []
        for decl in decls:
            if decl.name in rename_map:
                violations.append(
                    Violation(
                        policy=self.name,
                        message=f"Rename {decl.kind} '{decl.name}' -> '{rename_map[decl.name]}'",
                        line=decl.line,
                        column=1,
                    )
                )
        for name, reason in conflicts.items():
            violations.append(
                Violation(
                    policy=self.name,
                    message=f"Skipped rename for '{name}': {reason}",
                    line=1,
                    column=1,
                )
            )

        if updated == text:
            return PolicyResult(text=text, violations=violations, edits=[])

        return PolicyResult(text=updated, violations=violations, edits=edits)

    def _build_standards(self) -> dict[str, dict[str, object]]:
        return {
            "mj": {
                "local_prefix": "_",
                "member_prefix": "m_",
                "global_prefix": "g_",
                "static_prefix": "s_",
                "const_prefix": "c_",
                "pointer_prefix": "p_",
                "shared_ptr_prefix": "sp_",
                "unique_ptr_prefix": "up_",
                "weak_ptr_prefix": "wp_",
                "constexpr_prefix_upper": "C_",
                "static_prefix_upper": "S_",
                "function_case": "snake",
                "type_case": "camel",
                "namespace_case": "camel",
                "macro_case": "upper_snake",
                "constexpr_case": "upper_snake",
            },
            "google": {
                "local_prefix": "",
                "member_prefix": "",
                "global_prefix": "g_",
                "static_prefix": "",
                "const_prefix": "",
                "pointer_prefix": "",
                "shared_ptr_prefix": "",
                "unique_ptr_prefix": "",
                "weak_ptr_prefix": "",
                "constexpr_prefix_upper": "k",
                "static_prefix_upper": "",
                "function_case": "camel",
                "type_case": "camel",
                "namespace_case": "lower",
                "macro_case": "upper_snake",
                "constexpr_case": "camel",
            },
            "llvm": {
                "local_prefix": "",
                "member_prefix": "m",
                "global_prefix": "",
                "static_prefix": "",
                "const_prefix": "",
                "pointer_prefix": "",
                "shared_ptr_prefix": "",
                "unique_ptr_prefix": "",
                "weak_ptr_prefix": "",
                "constexpr_prefix_upper": "k",
                "static_prefix_upper": "",
                "function_case": "camel",
                "type_case": "camel",
                "namespace_case": "lower",
                "macro_case": "upper_snake",
                "constexpr_case": "camel",
            },
            "qt": {
                "local_prefix": "",
                "member_prefix": "m_",
                "global_prefix": "",
                "static_prefix": "",
                "const_prefix": "",
                "pointer_prefix": "",
                "shared_ptr_prefix": "",
                "unique_ptr_prefix": "",
                "weak_ptr_prefix": "",
                "constexpr_prefix_upper": "k",
                "static_prefix_upper": "",
                "function_case": "camel",
                "type_case": "camel",
                "namespace_case": "camel",
                "macro_case": "upper_snake",
                "constexpr_case": "camel",
            },
        }

    def _collect_declarations(self, code_lines: list[str]) -> list[_Decl]:
        decls: list[_Decl] = []
        brace_depth = 0
        pending_class: str | None = None
        pending_namespace: str | None = None
        pending_function: str | None = None
        class_stack: list[tuple[str, int]] = []
        function_stack: list[int] = []

        for idx, line in enumerate(code_lines):
            line_no = idx + 1
            stripped = line.strip()
            if stripped.startswith("#define"):
                macro = stripped.split()
                if len(macro) >= 2:
                    decls.append(_Decl(name=macro[1], kind="macro", line=line_no))
                continue

            class_match = re.search(r"\b(class|struct)\s+([A-Za-z_]\w*)", line)
            if class_match:
                kind = class_match.group(1)
                name = class_match.group(2)
                decls.append(_Decl(name=name, kind=kind, line=line_no))
                pending_class = name

            ns_match = re.search(r"\bnamespace\s+([A-Za-z_]\w*)", line)
            if ns_match:
                name = ns_match.group(1)
                decls.append(_Decl(name=name, kind="namespace", line=line_no))
                pending_namespace = name

            func_match = self._match_function(line)
            if func_match:
                pending_function = func_match
                decls.append(_Decl(name=func_match, kind="function", line=line_no))

            var_decl = self._match_variable_decl(line)
            if var_decl:
                decl = var_decl
                decl = _Decl(
                    name=decl.name,
                    kind=self._scope_kind(class_stack, function_stack),
                    is_static=decl.is_static,
                    is_const=decl.is_const,
                    is_constexpr=decl.is_constexpr,
                    is_consteval=decl.is_consteval,
                    is_pointer=decl.is_pointer,
                    smart_ptr=decl.smart_ptr,
                    scope_name=class_stack[-1][0] if class_stack else None,
                    line=line_no,
                )
                decls.append(decl)

            opens = line.count("{")
            closes = line.count("}")

            for _ in range(opens):
                brace_depth += 1
                if pending_class:
                    class_stack.append((pending_class, brace_depth))
                    pending_class = None
                elif pending_namespace:
                    pending_namespace = None
                elif pending_function:
                    function_stack.append(brace_depth)
                    pending_function = None

            for _ in range(closes):
                if class_stack and class_stack[-1][1] == brace_depth:
                    class_stack.pop()
                if function_stack and function_stack[-1] == brace_depth:
                    function_stack.pop()
                brace_depth = max(0, brace_depth - 1)

        return decls

    def _match_function(self, line: str) -> str | None:
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            return None
        if any(stripped.startswith(k + " ") for k in self._control_keywords):
            return None
        match = re.search(r"\b([A-Za-z_]\w*(?:::\w+)*)\s*\(", line)
        if not match:
            return None
        name = match.group(1).split("::")[-1]
        if name in self._skip_name_keywords:
            return None
        return name

    def _match_variable_decl(self, line: str) -> _Decl | None:
        if "(" in line and ")" in line and ";" not in line and "{" in line:
            return None
        if re.search(r"\b(class|struct|namespace)\b", line):
            return None
        if re.search(r"\btypedef\b", line):
            return None

        if "=" not in line and ";" not in line and "," not in line:
            return None

        type_and_name = re.search(
            r"""
            ^\s*
            (?P<qualifiers>(?:consteval|constexpr|const|static)\s+)*
            (?P<type>[\w:<>,\s*&]+?)\s+
            (?P<name>[A-Za-z_]\w*)
            \s*(?:=|;|,|\[|\(|$)
            """,
            line,
            re.VERBOSE,
        )
        if not type_and_name:
            return None

        qualifiers = type_and_name.group("qualifiers") or ""
        typ = type_and_name.group("type")
        name = type_and_name.group("name")

        is_static = "static" in qualifiers
        is_const = "const" in qualifiers and "constexpr" not in qualifiers
        is_constexpr = "constexpr" in qualifiers
        is_consteval = "consteval" in qualifiers
        is_pointer = "*" in typ

        smart_ptr = None
        if "shared_ptr" in typ:
            smart_ptr = "shared"
        elif "unique_ptr" in typ:
            smart_ptr = "unique"
        elif "weak_ptr" in typ:
            smart_ptr = "weak"

        return _Decl(
            name=name,
            kind="var",
            is_static=is_static,
            is_const=is_const,
            is_constexpr=is_constexpr,
            is_consteval=is_consteval,
            is_pointer=is_pointer,
            smart_ptr=smart_ptr,
        )

    def _scope_kind(self, class_stack: list[tuple[str, int]], function_stack: list[int]) -> str:
        if function_stack:
            return "local"
        if class_stack:
            return "member"
        return "global"

    def _build_rename_map(self, decls: list[_Decl]) -> tuple[dict[str, str], dict[str, str]]:
        rename: dict[str, str] = {}
        conflicts: dict[str, str] = {}
        class_names = {d.name for d in decls if d.kind in {"class", "struct"}}

        for decl in decls:
            target = self._target_name(decl, class_names)
            if not target or target == decl.name:
                continue
            if decl.name in rename and rename[decl.name] != target:
                conflicts[decl.name] = "multiple targets detected"
                rename.pop(decl.name, None)
                continue
            if decl.name in conflicts:
                continue
            rename[decl.name] = target
        return rename, conflicts

    def _target_name(self, decl: _Decl, class_names: set[str]) -> str | None:
        if decl.kind == "macro":
            return self._to_upper_snake(decl.name)

        if decl.kind in {"class", "struct"}:
            return self._to_camel(decl.name)

        if decl.kind == "namespace":
            return self._to_camel(decl.name)

        if decl.kind == "function":
            if decl.name in class_names or decl.name.lstrip("~") in class_names:
                return decl.name
            return self._to_snake(decl.name)

        if decl.kind in {"local", "member", "global"}:
            return self._name_variable(decl)

        return None

    def _name_variable(self, decl: _Decl) -> str:
        rules = self._rules

        if decl.is_constexpr or decl.is_consteval:
            prefixes = []
            if decl.is_static:
                prefixes.append(rules["static_prefix_upper"])
            prefixes.append(rules["constexpr_prefix_upper"])
            if decl.smart_ptr == "shared":
                prefixes.append("SP_")
            elif decl.smart_ptr == "unique":
                prefixes.append("UP_")
            elif decl.smart_ptr == "weak":
                prefixes.append("WP_")
            elif decl.is_pointer:
                prefixes.append("P_")
            base = self._to_upper_snake(decl.name)
            return "".join(prefixes) + base

        prefixes = []
        if decl.kind == "global" and rules.get("global_prefix"):
            prefixes.append(rules["global_prefix"])
        if decl.kind == "member" and rules.get("member_prefix"):
            prefixes.append(rules["member_prefix"])
        if decl.kind == "local" and rules.get("local_prefix"):
            prefixes.append(rules["local_prefix"])
        if decl.is_static and rules.get("static_prefix"):
            prefixes.append(rules["static_prefix"])
        if decl.is_const and rules.get("const_prefix"):
            prefixes.append(rules["const_prefix"])
        if decl.smart_ptr == "shared":
            prefixes.append(rules["shared_ptr_prefix"])
        elif decl.smart_ptr == "unique":
            prefixes.append(rules["unique_ptr_prefix"])
        elif decl.smart_ptr == "weak":
            prefixes.append(rules["weak_ptr_prefix"])
        elif decl.is_pointer and rules.get("pointer_prefix"):
            prefixes.append(rules["pointer_prefix"])

        base = self._to_snake(decl.name)
        return "".join(prefixes) + base

    def _apply_renames(
        self,
        lines: list[str],
        code_mask: list[bool],
        rename_map: dict[str, str],
    ) -> tuple[str, list[Edit]]:
        if not rename_map:
            return "".join(lines), []

        text = "".join(lines)
        edits: list[Edit] = []

        def replacer(match: re.Match) -> str:
            name = match.group(0)
            return rename_map.get(name, name)

        pattern = re.compile(r"\b[A-Za-z_]\w*\b")
        result_chars = list(text)
        offset = 0
        for match in pattern.finditer(text):
            start, end = match.span()
            if not all(code_mask[start:end]):
                continue
            name = match.group(0)
            new = rename_map.get(name)
            if not new or new == name:
                continue
            result_chars[start + offset : end + offset] = list(new)
            offset += len(new) - (end - start)

        updated = "".join(result_chars)

        if updated != text:
            for idx, (before, after) in enumerate(zip(lines, updated.splitlines(keepends=True))):
                if before != after:
                    edits.append(
                        Edit(
                            policy=self.name,
                            line=idx + 1,
                            before=before.rstrip("\r\n"),
                            after=after.rstrip("\r\n"),
                        )
                    )
        return updated, edits

    def _code_mask(self, text: str) -> list[bool]:
        mask = [True] * len(text)
        i = 0
        in_line_comment = False
        in_block_comment = False
        in_string = False
        in_char = False
        while i < len(text):
            ch = text[i]
            nxt = text[i + 1] if i + 1 < len(text) else ""

            if in_line_comment:
                mask[i] = False
                if ch == "\n":
                    in_line_comment = False
                i += 1
                continue

            if in_block_comment:
                mask[i] = False
                if ch == "*" and nxt == "/":
                    mask[i + 1] = False
                    in_block_comment = False
                    i += 2
                else:
                    i += 1
                continue

            if in_string:
                mask[i] = False
                if ch == "\\":
                    if i + 1 < len(text):
                        mask[i + 1] = False
                        i += 2
                        continue
                if ch == "\"":
                    in_string = False
                i += 1
                continue

            if in_char:
                mask[i] = False
                if ch == "\\":
                    if i + 1 < len(text):
                        mask[i + 1] = False
                        i += 2
                        continue
                if ch == "'":
                    in_char = False
                i += 1
                continue

            if ch == "/" and nxt == "/":
                mask[i] = mask[i + 1] = False
                in_line_comment = True
                i += 2
                continue
            if ch == "/" and nxt == "*":
                mask[i] = mask[i + 1] = False
                in_block_comment = True
                i += 2
                continue
            if ch == "\"":
                mask[i] = False
                in_string = True
                i += 1
                continue
            if ch == "'":
                mask[i] = False
                in_char = True
                i += 1
                continue

            i += 1

        return mask

    def _masked_text(self, text: str, mask: list[bool]) -> str:
        chars = []
        for ch, keep in zip(text, mask):
            chars.append(ch if keep else " ")
        return "".join(chars)

    def _to_snake(self, name: str) -> str:
        if name.isupper() and "_" in name:
            return name.lower()
        s1 = re.sub("(.)([A-Z][a-z]+)", r"\\1_\\2", name)
        s2 = re.sub("([a-z0-9])([A-Z])", r"\\1_\\2", s1)
        return re.sub(r"__+", "_", s2).lower()

    def _to_upper_snake(self, name: str) -> str:
        return self._to_snake(name).upper()

    def _to_camel(self, name: str) -> str:
        parts = re.split(r"[_\\s]+", name)
        return "".join(part[:1].upper() + part[1:] for part in parts if part)
