from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Any

from ..core.edit import Edit
from ..core.parse_context import ParseContext
from ..core.policy_result import PolicyResult
from ..core.violation import Violation
from .policy_base import Policy
from ..utils.warn_once import warn_once


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
    is_template_type: bool = False
    is_std_function: bool = False
    scope_name: str | None = None
    line: int = 0


@dataclass(frozen=True)
class _FuncMatch:
    name: str
    scope_name: str | None = None


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
    _param_scope_re = re.compile(
        r"^\s*(?:[\w:<>,\s*&]+)\s+([A-Za-z_]\w*(?:::[A-Za-z_]\w*)?)\s*\(([^)]*)\)\s*(const)?\s*(?:noexcept\s*)?(?:\:\s*[^\\{]*)?\{",
        re.MULTILINE,
    )
    _var_decl_re = re.compile(
        r"""
            ^\s*
            (?P<qualifiers>(?:consteval|constexpr|const|static)\s+)*
            (?P<type>[\w:<>,*&][\w:<>,\s*&]*?)\s+
            (?P<name>[A-Za-z_]\w*)
            \s*(?:=|;|,|\[|\(|$)
            """,
        re.VERBOSE,
    )
    _signature_re = re.compile(
        r"^\s*[\w:<>,\s*&]+\s+[A-Za-z_]\w*\s*\(.*\)\s*(const)?\s*[;{]\s*$"
    )
    _word_re = re.compile(r"\b[A-Za-z_]\w*\b")
    _template_decl_re = re.compile(r"template\s*<([^>]+)>", re.DOTALL)
    _template_param_re = re.compile(r"\b(?:typename|class)\s+([A-Za-z_]\w*)")
    _std_function_re = re.compile(r"\bstd::function\s*<", re.IGNORECASE)

    def __init__(self, config: dict[str, object]) -> None:
        super().__init__(config)
        self._standard = str(self._config.get("standard", "mj")).lower()
        self._standards = self._build_standards()
        self._rules = self._standards.get(self._standard, self._standards["mj"])
        self._use_tree_sitter = bool(self._config.get("use_tree_sitter", True))
        if self._use_tree_sitter:
            self.parse_mode = "tree_sitter"

    def apply(self, context: ParseContext) -> PolicyResult:
        text = context.text
        if not text:
            return PolicyResult(text=text, violations=[], edits=[])

        code_mask = self._code_mask(text)
        code_text = self._masked_text(text, code_mask)
        lines = text.splitlines(keepends=True)

        template_params = self._collect_template_params(code_text)
        tree = context.tree_sitter_tree
        use_tree = (
            self._use_tree_sitter
            and tree is not None
            and getattr(getattr(tree, "root_node", None), "has_error", False) is False
        )
        if use_tree and tree is not None:
            tree_decls = self._collect_declarations_tree_sitter(
                NamingConventionsPolicy.TreeDeclArgs(
                    text=text,
                    tree=tree,
                    template_params=template_params,
                )
            )
            code_lines = code_text.splitlines(keepends=True)
            regex_decls = self._collect_declarations(
                NamingConventionsPolicy.CollectDeclsArgs(
                    code_lines=code_lines,
                    template_params=template_params,
                )
            )
            tree_names = {decl.name for decl in tree_decls}
            decls = tree_decls + [decl for decl in regex_decls if decl.name not in tree_names]
        else:
            code_lines = code_text.splitlines(keepends=True)
            decls = self._collect_declarations(
                NamingConventionsPolicy.CollectDeclsArgs(
                    code_lines=code_lines,
                    template_params=template_params,
                )
            )
        if not decls:
            return PolicyResult(text=text, violations=[], edits=[])

        rename_map, conflicts = self._build_rename_map(decls)
        if not rename_map and not conflicts:
            return PolicyResult(text=text, violations=[], edits=[])

        if use_tree and context.tree_sitter_tree is not None:
            updated, edits = self._apply_renames_tree_sitter(
                text,
                rename_map,
                context.tree_sitter_tree,
            )
        else:
            warn_once(
                "naming_conventions_regex_fallback",
                "naming_conventions: regex fallback is less accurate; install tree-sitter-languages for better results",
            )
            updated, edits = self._apply_renames(
                NamingConventionsPolicy.ApplyRenamesArgs(
                    lines=lines,
                    code_mask=code_mask,
                    rename_map=rename_map,
                )
            )

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

    @dataclass(frozen=True)
    class CollectDeclsArgs:
        code_lines: list[str]
        template_params: set[str]

    @dataclass(frozen=True)
    class TreeDeclArgs:
        text: str
        tree: Any
        template_params: set[str]

    def _collect_declarations(self, args: "NamingConventionsPolicy.CollectDeclsArgs") -> list[_Decl]:
        decls: list[_Decl] = []
        text = "".join(args.code_lines)
        for match in self._param_scope_re.finditer(text):
            params = match.group(2)
            line_no = text.count("\n", 0, match.start()) + 1
            decls.extend(
                self._param_decls(
                    NamingConventionsPolicy.ParamDeclArgs(
                        params=params,
                        template_params=args.template_params,
                        line_no=line_no,
                    )
                )
            )
        brace_depth = 0
        pending_class: str | None = None
        pending_namespace: str | None = None
        pending_function: str | None = None
        class_stack: list[tuple[str, int]] = []
        function_stack: list[int] = []
        template_depth = 0

        for idx, line in enumerate(args.code_lines):
            line_no = idx + 1
            stripped = line.strip()
            if stripped.startswith("#define"):
                macro = stripped.split()
                if len(macro) >= 2:
                    decls.append(_Decl(name=macro[1], kind="macro", line=line_no))
                continue

            class_name = self._extract_class_name(line)
            if class_name:
                kind = "class" if "class" in line else "struct"
                name = class_name
                decls.append(_Decl(name=name, kind=kind, line=line_no))
                pending_class = name

            ns_match = re.search(r"\bnamespace\s+([A-Za-z_]\w*)", line)
            if ns_match:
                name = ns_match.group(1)
                decls.append(_Decl(name=name, kind="namespace", line=line_no))
                pending_namespace = name

            func_match = self._match_function(line)
            if func_match:
                pending_function = func_match.name
                decls.append(
                    _Decl(
                        name=func_match.name,
                        kind="function",
                        scope_name=func_match.scope_name,
                        line=line_no,
                    )
                )

            prev_template_depth = template_depth
            template_depth = self._update_template_depth(line, template_depth)
            if not self._in_template_params(line, prev_template_depth):
                var_decl = self._match_variable_decl(
                    NamingConventionsPolicy.MatchVarArgs(
                        line=line,
                        template_params=args.template_params,
                    )
                )
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
                        is_template_type=decl.is_template_type,
                        is_std_function=decl.is_std_function,
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

    def _collect_declarations_tree_sitter(self, args: "NamingConventionsPolicy.TreeDeclArgs") -> list[_Decl]:
        text = args.text
        tree = args.tree
        data = text.encode("utf-8")
        lines = text.splitlines()
        decls: list[_Decl] = []

        root = getattr(tree, "root_node", None)
        if root is None:
            return decls

        class_names: set[str] = set()

        def node_text(node: Any) -> str:
            return data[node.start_byte:node.end_byte].decode("utf-8", errors="ignore")

        def is_macro_like(name: str) -> bool:
            return name.isupper() and "_" in name

        def extract_identifier(node: Any, allow_type: bool = True) -> str | None:
            stack = [node]
            while stack:
                current = stack.pop()
                if current.type in {"identifier", "field_identifier"} or (
                    allow_type and current.type == "type_identifier"
                ):
                    return node_text(current)
                if current.type in {"parameter_list", "template_parameter_list", "template_parameter"}:
                    continue
                stack.extend(reversed(current.children))
            return None

        def decl_prefix_text(node: Any, name_node: Any) -> str:
            return data[node.start_byte:name_node.start_byte].decode("utf-8", errors="ignore")

        def has_ancestor(node: Any, types: set[str]) -> bool:
            current = getattr(node, "parent", None)
            while current is not None:
                if getattr(current, "type", None) in types:
                    return True
                current = getattr(current, "parent", None)
            return False

        def scope_kind(node: Any) -> str:
            if has_ancestor(node, {"function_definition", "lambda_expression"}):
                return "local"
            if has_ancestor(node, {"class_specifier", "struct_specifier"}):
                return "member"
            return "global"

        def is_pointer_decl(node: Any) -> bool:
            current = node
            while current is not None:
                if getattr(current, "type", None) in {"pointer_declarator", "pointer_type"}:
                    return True
                current = getattr(current, "parent", None)
            return False

        def is_template_param_node(node: Any) -> bool:
            return has_ancestor(
                node,
                {
                    "template_parameter",
                    "template_type_parameter",
                    "type_parameter_declaration",
                    "parameter_declaration",
                    "optional_parameter_declaration",
                    "template_parameter_list",
                },
            )

        def template_type_from_prefix(prefix: str) -> bool:
            tokens = re.findall(r"[A-Za-z_]\w*", prefix)
            if not tokens:
                return False
            return tokens[-1] in args.template_params

        def extract_qualified_parts(node: Any) -> list[str]:
            text = node_text(node)
            return re.findall(r"~?[A-Za-z_]\w*", text)

        stack = [root]
        while stack:
            node = stack.pop()
            node_type = node.type

            if node_type in {"class_specifier", "struct_specifier"}:
                name = None
                for child in node.children:
                    if child.type == "type_identifier":
                        name = node_text(child)
                        break
                if name is None:
                    name = extract_identifier(node, allow_type=True)
                if not name or is_macro_like(name):
                    line_no = node.start_point[0]
                    if 0 <= line_no < len(lines):
                        fallback = self._extract_class_name(lines[line_no])
                        if fallback:
                            name = fallback
                if name and not is_macro_like(name):
                    class_names.add(name)
                    decls.append(
                        _Decl(
                            name=name,
                            kind="class" if node_type == "class_specifier" else "struct",
                            line=node.start_point[0] + 1,
                        )
                    )

            elif node_type == "namespace_definition":
                name = extract_identifier(node, allow_type=False)
                if name and not is_macro_like(name):
                    decls.append(_Decl(name=name, kind="namespace", line=node.start_point[0] + 1))

            elif node_type in {"preproc_def", "preproc_function_def"}:
                name = extract_identifier(node, allow_type=False)
                if name:
                    decls.append(_Decl(name=name, kind="macro", line=node.start_point[0] + 1))

            elif node_type in {"function_definition", "declaration", "field_declaration"}:
                if node_type == "function_definition":
                    func_decl = None
                    for child in node.children:
                        if child.type in {"function_declarator", "declarator"}:
                            func_decl = child
                            break
                else:
                    func_decl = None
                    for child in node.children:
                        if child.type == "function_declarator":
                            func_decl = child
                            break
                if func_decl is not None:
                    name = extract_identifier(func_decl, allow_type=False)
                    if name and not is_macro_like(name):
                        scope_name = None
                        for child in func_decl.children:
                            if child.type in {"qualified_identifier", "scoped_identifier"}:
                                parts = extract_qualified_parts(child)
                                if len(parts) >= 2:
                                    scope_name = parts[-2]
                                break
                        decls.append(
                            _Decl(
                                name=name,
                                kind="function",
                                scope_name=scope_name,
                                line=func_decl.start_point[0] + 1,
                            )
                        )

            if node_type in {"parameter_declaration", "optional_parameter_declaration"}:
                name_node = None
                stack_name = [node]
                while stack_name:
                    current = stack_name.pop()
                    if current.type in {"identifier", "field_identifier"}:
                        name_node = current
                        break
                    if current.type in {"parameter_list", "template_parameter_list", "template_parameter"}:
                        continue
                    stack_name.extend(reversed(current.children))
                if name_node is not None and not is_template_param_node(name_node):
                    name = node_text(name_node)
                    if name and not is_macro_like(name):
                        prefix = decl_prefix_text(node, name_node)
                        is_constexpr = "constexpr" in prefix
                        is_consteval = "consteval" in prefix
                        is_static = "static" in prefix
                        is_const = "const" in prefix and not is_constexpr and not is_consteval
                        is_ptr = is_pointer_decl(name_node)
                        smart_ptr = None
                        if "shared_ptr" in prefix:
                            smart_ptr = "shared"
                        elif "unique_ptr" in prefix:
                            smart_ptr = "unique"
                        elif "weak_ptr" in prefix:
                            smart_ptr = "weak"
                        is_std_function = "std::function" in prefix
                        is_template_type = template_type_from_prefix(prefix)
                        decls.append(
                            _Decl(
                                name=name,
                                kind="local",
                                is_static=is_static,
                                is_const=is_const,
                                is_constexpr=is_constexpr,
                                is_consteval=is_consteval,
                                is_pointer=is_ptr,
                                smart_ptr=smart_ptr,
                                is_template_type=is_template_type,
                                is_std_function=is_std_function,
                                scope_name=None,
                                line=name_node.start_point[0] + 1,
                            )
                        )

            if node_type in {"init_declarator", "field_declaration", "declaration"}:
                if node_type == "init_declarator":
                    decl_node = node.parent or node
                else:
                    decl_node = node
                if any(child.type == "function_declarator" for child in node.children):
                    stack.extend(reversed(node.children))
                    continue
                if has_ancestor(decl_node, {"function_declarator"}):
                    stack.extend(reversed(node.children))
                    continue
                name_node = None
                name_node = None
                stack_name = [node]
                while stack_name:
                    current = stack_name.pop()
                    if current.type in {"identifier", "field_identifier"}:
                        name_node = current
                        break
                    if current.type in {"parameter_list", "template_parameter_list", "template_parameter"}:
                        continue
                    stack_name.extend(reversed(current.children))
                if name_node is not None and not is_template_param_node(name_node):
                    if has_ancestor(name_node, {"enumerator", "enum_specifier"}):
                        stack.extend(reversed(node.children))
                        continue
                    name = node_text(name_node)
                    if name and not is_macro_like(name):
                        prefix = decl_prefix_text(decl_node, name_node)
                        is_constexpr = "constexpr" in prefix
                        is_consteval = "consteval" in prefix
                        is_static = "static" in prefix
                        is_const = "const" in prefix and not is_constexpr and not is_consteval
                        is_ptr = is_pointer_decl(name_node)
                        smart_ptr = None
                        if "shared_ptr" in prefix:
                            smart_ptr = "shared"
                        elif "unique_ptr" in prefix:
                            smart_ptr = "unique"
                        elif "weak_ptr" in prefix:
                            smart_ptr = "weak"
                        is_std_function = "std::function" in prefix
                        is_template_type = template_type_from_prefix(prefix)
                        decls.append(
                            _Decl(
                                name=name,
                                kind=scope_kind(name_node),
                                is_static=is_static,
                                is_const=is_const,
                                is_constexpr=is_constexpr,
                                is_consteval=is_consteval,
                                is_pointer=is_ptr,
                                smart_ptr=smart_ptr,
                                is_template_type=is_template_type,
                                is_std_function=is_std_function,
                                scope_name=None,
                                line=name_node.start_point[0] + 1,
                            )
                        )

            stack.extend(reversed(node.children))

        return decls

    def _match_function(self, line: str) -> _FuncMatch | None:
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            return None
        if any(stripped.startswith(k + " ") for k in self._control_keywords):
            return None
        if "::~" in line:
            return None
        match = re.search(r"\b([A-Za-z_]\w*(?:::\w+)*)\s*\(", line)
        if not match:
            return None
        prefix = line[: match.start()]
        if not prefix.strip():
            return None
        if "=" in prefix or "." in prefix or "->" in prefix:
            return None
        if "return" in prefix:
            return None
        full = match.group(1)
        parts = full.split("::")
        if len(parts) >= 2:
            last = parts[-1]
            prev = parts[-2]
            if last == prev or last == f"~{prev}":
                return None
        name = parts[-1]
        if name in self._skip_name_keywords:
            return None
        scope_name = parts[-2] if len(parts) >= 2 else None
        return _FuncMatch(name=name, scope_name=scope_name)

    @dataclass(frozen=True)
    class MatchVarArgs:
        line: str
        template_params: set[str]

    def _match_variable_decl(self, args: "NamingConventionsPolicy.MatchVarArgs") -> _Decl | None:
        line = args.line
        stripped = line.lstrip()
        if stripped.startswith("template"):
            return None
        if re.match(r"^[A-Za-z_]\w*(?:::\w+)?\s*\(", stripped):
            return None
        if re.match(r"^~[A-Za-z_]\w*\s*\(", stripped):
            return None
        if stripped.startswith(("return ", "return(", "throw ", "throw(", "case ", "break", "continue", "goto ")):
            return None
        if stripped.startswith("static_assert"):
            return None
        if "operator" in line and "(" in line:
            return None
        for keyword in self._control_keywords:
            if stripped.startswith(keyword + " ") or stripped.startswith(keyword + "("):
                return None
        if "(" in line and ")" in line and ";" not in line and "{" in line:
            return None
        if re.search(r"\b(class|struct|namespace|enum|using)\b", line):
            return None
        if re.search(r"\btypedef\b", line):
            return None

        if "=" not in line and ";" not in line:
            return None

        type_and_name = self._var_decl_re.search(line)
        if not type_and_name:
            return None

        name_end = type_and_name.end("name")
        tail = line[name_end:]
        paren_idx = tail.find("(")
        eq_idx = tail.find("=")
        if paren_idx != -1 and (eq_idx == -1 or paren_idx < eq_idx):
            return None

        typ = type_and_name.group("type")
        name = type_and_name.group("name")

        prefix = line[: type_and_name.start("name")]
        is_static = re.search(r"\bstatic\b", prefix) is not None
        is_constexpr = re.search(r"\bconstexpr\b", prefix) is not None
        is_consteval = re.search(r"\bconsteval\b", prefix) is not None
        is_const = re.search(r"\bconst\b", prefix) is not None and not is_constexpr and not is_consteval
        is_pointer = "*" in typ

        smart_ptr = None
        if "shared_ptr" in typ:
            smart_ptr = "shared"
        elif "unique_ptr" in typ:
            smart_ptr = "unique"
        elif "weak_ptr" in typ:
            smart_ptr = "weak"
        is_std_function = bool(self._std_function_re.search(typ))
        is_template_type = False
        type_token = typ.strip().split()[-1] if typ.strip() else ""
        if type_token in args.template_params:
            is_template_type = True

        return _Decl(
            name=name,
            kind="var",
            is_static=is_static,
            is_const=is_const,
            is_constexpr=is_constexpr,
            is_consteval=is_consteval,
            is_pointer=is_pointer,
            smart_ptr=smart_ptr,
            is_template_type=is_template_type,
            is_std_function=is_std_function,
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
        protected: set[str] = set()
        class_names = {d.name for d in decls if d.kind in {"class", "struct"}}

        for decl in decls:
            target = self._target_name(decl, class_names)
            if not target or target == decl.name:
                if decl.kind in {"function", "class", "struct", "namespace"}:
                    if decl.name in rename:
                        rename.pop(decl.name, None)
                        conflicts[decl.name] = "conflicts with existing symbol"
                    protected.add(decl.name)
                continue
            if decl.name in protected:
                conflicts[decl.name] = "conflicts with existing symbol"
                rename.pop(decl.name, None)
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
            if decl.scope_name and (
                decl.name == decl.scope_name or decl.name.lstrip("~") == decl.scope_name
            ):
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
            if decl.is_template_type:
                prefixes.append("T_")
            if decl.is_std_function:
                prefixes.append("F_")
            if decl.smart_ptr == "shared":
                prefixes.append("SP_")
            elif decl.smart_ptr == "unique":
                prefixes.append("UP_")
            elif decl.smart_ptr == "weak":
                prefixes.append("WP_")
            elif decl.is_pointer:
                prefixes.append("P_")
            stripped = self._strip_prefixes(decl.name, prefixes)
            base = self._to_upper_snake(stripped or decl.name)
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
        if decl.is_template_type:
            prefixes.append("t_")
        if decl.is_std_function:
            prefixes.append("f_")
        if decl.smart_ptr == "shared":
            prefixes.append(rules["shared_ptr_prefix"])
        elif decl.smart_ptr == "unique":
            prefixes.append(rules["unique_ptr_prefix"])
        elif decl.smart_ptr == "weak":
            prefixes.append(rules["weak_ptr_prefix"])
        elif decl.is_pointer and rules.get("pointer_prefix"):
            prefixes.append(rules["pointer_prefix"])

        base_name = decl.name
        if decl.kind in {"member", "global"}:
            base_name = base_name.lstrip("_")
        stripped = self._strip_prefixes(base_name, prefixes)
        base = self._to_snake(stripped or decl.name)
        return "".join(prefixes) + base

    def _collect_template_params(self, text: str) -> set[str]:
        params: set[str] = set()
        for match in self._template_decl_re.finditer(text):
            body = match.group(1)
            for chunk in self._split_template_params(body):
                left = chunk.split("=", 1)[0].strip()
                if not left:
                    continue
                tokens = re.findall(r"[A-Za-z_]\w*", left)
                if not tokens:
                    continue
                if tokens[0] in {"typename", "class"} and len(tokens) > 1:
                    params.add(tokens[1])
                else:
                    params.add(tokens[-1])
        return params

    @dataclass(frozen=True)
    class ApplyRenamesArgs:
        lines: list[str]
        code_mask: list[bool]
        rename_map: dict[str, str]

    def _apply_renames(self, args: "NamingConventionsPolicy.ApplyRenamesArgs") -> tuple[str, list[Edit]]:
        if not args.rename_map:
            return "".join(args.lines), []

        text = "".join(args.lines)
        edits: list[Edit] = []

        def replacer(match: re.Match) -> str:
            name = match.group(0)
            return rename_map.get(name, name)

        pattern = self._word_re
        result_chars = list(text)
        offset = 0
        for match in pattern.finditer(text):
            start, end = match.span()
            if not all(args.code_mask[start:end]):
                continue
            name = match.group(0)
            new = args.rename_map.get(name)
            if not new or new == name:
                continue
            result_chars[start + offset : end + offset] = list(new)
            offset += len(new) - (end - start)

        updated = "".join(result_chars)

        if updated != text:
            for idx, (before, after) in enumerate(zip(args.lines, updated.splitlines(keepends=True))):
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
            if keep or ch in {"\n", "\r"}:
                chars.append(ch)
            else:
                chars.append(" ")
        return "".join(chars)

    def _apply_renames_tree_sitter(
        self,
        text: str,
        rename_map: dict[str, str],
        tree: Any,
    ) -> tuple[str, list[Edit]]:
        if not rename_map:
            return text, []
        root = getattr(tree, "root_node", None)
        if root is None:
            return text, []

        data = text.encode("utf-8")
        replacements: list[tuple[int, int, str]] = []
        stack = [root]
        while stack:
            node = stack.pop()
            if node.type in {"identifier", "field_identifier", "type_identifier"}:
                if self._has_ancestor_type(
                    node,
                    {
                        "template_parameter",
                        "template_type_parameter",
                        "template_template_parameter",
                    },
                ):
                    stack.extend(reversed(node.children))
                    continue
                name = data[node.start_byte : node.end_byte].decode("utf-8", errors="ignore")
                new = rename_map.get(name)
                if new and new != name:
                    if not self._should_rename_node(node, new):
                        stack.extend(reversed(node.children))
                        continue
                    replacements.append((node.start_byte, node.end_byte, new))
            stack.extend(reversed(node.children))

        if not replacements:
            return text, []

        for start, end, new in sorted(replacements, key=lambda item: item[0], reverse=True):
            data = data[:start] + new.encode("utf-8") + data[end:]
        updated = data.decode("utf-8")

        edits: list[Edit] = []
        if updated != text:
            for idx, (before, after) in enumerate(zip(text.splitlines(keepends=True), updated.splitlines(keepends=True))):
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

    def _has_ancestor_type(self, node: Any, types: set[str]) -> bool:
        current = getattr(node, "parent", None)
        while current is not None:
            if getattr(current, "type", None) in types:
                return True
            current = getattr(current, "parent", None)
        return False

    def _should_rename_node(self, node: Any, target: str) -> bool:
        if node.type == "type_identifier":
            if target.isupper():
                return True
            if target[:1].isupper():
                return True
            parent = getattr(node, "parent", None)
            parent_type = getattr(parent, "type", "") if parent is not None else ""
            if parent_type not in {"class_specifier", "struct_specifier"}:
                return False
            return True
        return True

    def _extract_class_name(self, line: str) -> str | None:
        match = re.search(r"\b(class|struct)\b([^:{;]*)", line)
        if not match:
            return None
        tail = match.group(2)
        identifiers = re.findall(r"[A-Za-z_]\w*", tail)
        if not identifiers:
            return None
        ignored = {"final", "sealed", "alignas", "constexpr", "consteval"}
        for ident in identifiers:
            if ident in ignored:
                continue
            if ident.isupper() and "_" in ident:
                continue
            return ident
        return identifiers[-1]

    def _update_template_depth(self, line: str, depth: int) -> int:
        if "template" in line or depth > 0:
            segment = line[line.find("template") :] if "template" in line else line
            depth += segment.count("<") - segment.count(">")
        return max(depth, 0)

    def _in_template_params(self, line: str, prev_depth: int) -> bool:
        if "template" in line:
            return True
        return prev_depth > 0

    def _split_template_params(self, body: str) -> list[str]:
        params: list[str] = []
        current: list[str] = []
        depth = 0
        for ch in body:
            if ch == "<":
                depth += 1
            elif ch == ">":
                depth = max(0, depth - 1)
            if ch == "," and depth == 0:
                params.append("".join(current).strip())
                current = []
                continue
            current.append(ch)
        tail = "".join(current).strip()
        if tail:
            params.append(tail)
        return params

    def _to_snake(self, name: str) -> str:
        if name.isupper() and "_" in name:
            return name.lower()
        s1 = re.sub("(.)([A-Z][a-z]+)", r"\1_\2", name)
        s2 = re.sub("([a-z0-9])([A-Z])", r"\1_\2", s1)
        return re.sub(r"__+", "_", s2).lower()

    def _to_upper_snake(self, name: str) -> str:
        return self._to_snake(name).upper()

    def _to_camel(self, name: str) -> str:
        parts = re.split(r"[_\s]+", name)
        return "".join(part[:1].upper() + part[1:] for part in parts if part)

    def _strip_prefixes(self, name: str, prefixes: list[str]) -> str:
        if not prefixes:
            return name
        cleaned = name
        changed = True
        prefixes = [p for p in prefixes if p]
        while changed and prefixes:
            changed = False
            for prefix in prefixes:
                if cleaned.startswith(prefix):
                    cleaned = cleaned[len(prefix):]
                    changed = True
                    break
        return cleaned

    @dataclass(frozen=True)
    class ParamListArgs:
        text: str
        start: int
        end: int

    def _in_param_list(self, args: "NamingConventionsPolicy.ParamListArgs") -> bool:
        line_start = args.text.rfind("\n", 0, args.start) + 1
        line_end = args.text.find("\n", args.start)
        if line_end == -1:
            line_end = len(args.text)
        line = args.text[line_start:line_end]
        if "(" not in line or ")" not in line:
            return False
        signature = self._signature_re.match(line.strip())
        if not signature:
            return False
        left = line.find("(")
        right = line.find(")")
        if left == -1 or right == -1 or right < left:
            return False
        idx = args.start - line_start
        return left < idx < right

    def _collect_param_scopes(self, text: str) -> list[tuple[int, int, set[str]]]:
        scopes: list[tuple[int, int, set[str]]] = []
        for match in self._param_scope_re.finditer(text):
            params = match.group(2)
            names = self._extract_param_names(params)
            start = match.start()
            brace = 0
            i = match.end() - 1
            while i < len(text):
                ch = text[i]
                if ch == "{":
                    brace += 1
                elif ch == "}":
                    brace -= 1
                    if brace == 0:
                        scopes.append((start, i + 1, names))
                        break
                i += 1
        return scopes

    @dataclass(frozen=True)
    class ParamDeclArgs:
        params: str
        template_params: set[str]
        line_no: int

    def _param_decls(self, args: "NamingConventionsPolicy.ParamDeclArgs") -> list[_Decl]:
        decls: list[_Decl] = []
        for part in args.params.split(","):
            chunk = part.strip()
            if not chunk:
                continue
            if "=" in chunk:
                chunk = chunk.split("=", 1)[0].strip()
            match = re.search(r"([A-Za-z_]\w*)\s*(?:\)|\]|$)", chunk)
            if not match:
                match = re.search(r"([A-Za-z_]\w*)\s*$", chunk)
            if not match:
                continue
            name = match.group(1)
            prefix = chunk[: match.start(1)]
            is_constexpr = "constexpr" in prefix
            is_consteval = "consteval" in prefix
            is_const = "const" in prefix and not is_constexpr and not is_consteval
            is_pointer = "*" in prefix
            smart_ptr = None
            if "shared_ptr" in prefix:
                smart_ptr = "shared"
            elif "unique_ptr" in prefix:
                smart_ptr = "unique"
            elif "weak_ptr" in prefix:
                smart_ptr = "weak"
            is_std_function = "std::function" in prefix
            is_template_type = False
            tokens = re.findall(r"[A-Za-z_]\w*", prefix)
            if tokens and tokens[-1] in args.template_params:
                is_template_type = True
            decls.append(
                _Decl(
                    name=name,
                    kind="local",
                    is_static=False,
                    is_const=is_const,
                    is_constexpr=is_constexpr,
                    is_consteval=is_consteval,
                    is_pointer=is_pointer,
                    smart_ptr=smart_ptr,
                    is_template_type=is_template_type,
                    is_std_function=is_std_function,
                    line=args.line_no,
                )
            )
        return decls

    def _extract_param_names(self, params: str) -> set[str]:
        names: set[str] = set()
        for part in params.split(","):
            chunk = part.strip()
            if not chunk:
                continue
            if "=" in chunk:
                chunk = chunk.split("=", 1)[0].strip()
            match = re.search(r"([A-Za-z_]\w*)\s*(?:\)|\]|$)", chunk)
            if not match:
                match = re.search(r"([A-Za-z_]\w*)\s*$", chunk)
            if match:
                names.add(match.group(1))
        return names

    @dataclass(frozen=True)
    class ParamScopeArgs:
        pos: int
        name: str
        scopes: list[tuple[int, int, set[str]]]

    def _in_param_scope(self, args: "NamingConventionsPolicy.ParamScopeArgs") -> bool:
        for start, end, names in args.scopes:
            if start <= args.pos <= end and args.name in names:
                return True
        return False
