from __future__ import annotations

from dataclasses import dataclass
from typing import Any
from enum import Enum
from collections.abc import Mapping

from ..core.types import Edit
from ..core.types import ParseContext
from ..core.types import PolicyResult
from ..core.types import Violation
from .policy_base import Policy
from ..core.utilities import warn_once
from ..core.types import SemanticContext, SemanticSymbol, SemanticReference


@dataclass(frozen=True)
class _Decl:
    name: str
    kind: str  # param | local | member | global | function | class | struct | namespace | macro
    is_static: bool = False
    is_const: bool = False
    is_constexpr: bool = False
    is_consteval: bool = False
    is_atomic: bool = False
    is_pointer: bool = False
    smart_ptr: str | None = None  # shared | unique | weak
    is_template_type: bool = False
    is_std_function: bool = False
    scope_name: str | None = None
    line: int = 0


@dataclass(frozen=True)
class _SemanticRenameDecision:
    usr: str
    old_name: str
    new_name: str
    line: int
    risk: str
    confidence: float
    reference_count: int
    parser_consensus: float
    scope_purity: float


class ParserConsensusMode(str, Enum):
    OFF = "off"
    ADVISORY = "advisory"
    STRICT = "strict"

    @classmethod
    def from_config(cls, value: object) -> "ParserConsensusMode":
        if isinstance(value, ParserConsensusMode):
            return value
        raw = str(value or cls.ADVISORY.value).strip().lower()
        for item in cls:
            if item.value == raw:
                return item
        return cls.ADVISORY


class NamingConventionsPolicy(Policy):
    name = "naming_conventions"
    description = "Enforce naming conventions with prefixes"
    parse_mode = "clang"
    requires_code_context = True
    def __init__(self, config: dict[str, object]) -> None:
        super().__init__(config)
        self._standard = str(self._config.get("standard", "mj")).lower()
        self._standards = self._build_standards()
        self._rules = self._standards.get(self._standard, self._standards["mj"])
        self._skip_name_keywords = self._config_set("skip_name_keywords") | self._config_set("control_keywords")
        self._reserved_identifiers = self._config_set("reserved_identifiers")
        self._builtin_types = self._config_set("builtin_types")
        self._use_semantic_rename = bool(self._config.get("use_semantic_rename", True))
        self._min_confidence = float(self._config.get("min_confidence", 0.75))
        self._max_risk = str(self._config.get("max_risk", "medium")).lower()
        self._risk_rank = {"low": 0, "medium": 1, "high": 2}
        self._max_risk_rank = self._risk_rank.get(self._max_risk, 1)
        self._parser_consensus_mode = ParserConsensusMode.from_config(
            self._config.get("parser_consensus_mode", ParserConsensusMode.ADVISORY.value)
        )
        self._parser_consensus_min = float(self._config.get("parser_consensus_min", 0.70))
        self._strict_local_scope = bool(self._config.get("strict_local_scope", True))
        self._prefer_clang_semantic = bool(self._config.get("prefer_clang_semantic", True))
        self._use_tree_sitter = bool(self._config.get("use_tree_sitter", True))
        if self._prefer_clang_semantic:
            self.parse_mode = "clang"
        elif self._use_tree_sitter:
            self.parse_mode = "tree_sitter"
        else:
            # Keep clang as the primary backend when parser preference is disabled.
            self.parse_mode = "clang"

    def apply(self, context: ParseContext) -> PolicyResult:
        text = context.text
        if not text:
            return PolicyResult(text=text, violations=[], edits=[])

        semantic_context = None
        semantic_file_counts: dict[str, int] = {}
        semantic_consensus_scores: dict[str, float] = {}
        semantic_reference_consensus_scores: dict[str, float] = {}
        semantic_declaration_consensus_scores: dict[str, float] = {}
        semantic_reference_counts: dict[str, int] = {}
        semantic_scope_purity: dict[str, float] = {}
        semantic_project_reference_counts: dict[str, int] = {}
        semantic_project_consensus_scores: dict[str, float] = {}
        semantic_refs_by_usr: Mapping[str, tuple[SemanticReference, ...]] = {}
        semantic_non_decl_ref_counts: Mapping[str, int] = {}
        semantic_class_names: tuple[str, ...] = ()
        if context.code_context is not None:
            semantic_context = getattr(context.code_context, "semantic_context", None)
            semantic_file_counts = getattr(context.code_context, "semantic_file_counts", {}) or {}
            semantic_consensus_scores = getattr(context.code_context, "semantic_consensus_scores", {}) or {}
            semantic_reference_consensus_scores = (
                getattr(context.code_context, "semantic_reference_consensus_scores", {}) or {}
            )
            semantic_declaration_consensus_scores = (
                getattr(context.code_context, "semantic_declaration_consensus_scores", {}) or {}
            )
            semantic_reference_counts = getattr(context.code_context, "semantic_reference_counts", {}) or {}
            semantic_scope_purity = getattr(context.code_context, "semantic_scope_purity", {}) or {}
            semantic_project_reference_counts = (
                getattr(context.code_context, "semantic_project_reference_counts", {}) or {}
            )
            semantic_project_consensus_scores = (
                getattr(context.code_context, "semantic_project_consensus_scores", {}) or {}
            )
            semantic_refs_by_usr = getattr(context.code_context, "semantic_refs_by_usr", {}) or {}
            semantic_non_decl_ref_counts = (
                getattr(context.code_context, "semantic_non_declaration_ref_counts", {}) or {}
            )
            semantic_class_names = tuple(getattr(context.code_context, "semantic_class_names", ()) or ())

        tree = context.tree_sitter_tree
        use_tree = self._use_tree_sitter and tree is not None

        decls: list[_Decl] = []
        semantic_decls: list[_Decl] = []
        tree_decls: list[_Decl] = []
        if isinstance(semantic_context, SemanticContext):
            semantic_decls = self._collect_declarations_semantic(semantic_context)
        if use_tree:
            tree_decls = self._collect_declarations_tree_sitter(
                NamingConventionsPolicy.TreeDeclArgs(
                    text=text,
                    tree=tree,
                )
            )
        if self._prefer_clang_semantic and semantic_decls and tree_decls:
            semantic_names = {decl.name for decl in semantic_decls}
            tree_decls = [
                decl
                for decl in tree_decls
                if decl.kind in {"macro", "namespace", "class", "struct"} or decl.name in semantic_names
            ]

        if self._prefer_clang_semantic:
            decls.extend(semantic_decls)
            decls.extend(tree_decls)
        else:
            decls.extend(tree_decls)
            decls.extend(semantic_decls)

        if not decls:
            if not use_tree and not isinstance(semantic_context, SemanticContext):
                warn_once(
                    "naming_conventions_parser_unavailable",
                    "naming_conventions: parser context unavailable, skipping policy (enable clang and/or tree-sitter-languages)",
                )
            return PolicyResult(text=text, violations=[], edits=[])

        decls = self._dedupe_decls(decls)
        decls = [decl for decl in decls if self._is_identifier_name(decl.name)]
        rename_map, conflicts = self._build_rename_map(decls)
        has_semantic = isinstance(semantic_context, SemanticContext)
        if not has_semantic and rename_map:
            # Name-only rename maps are unsafe for variable symbols when semantic refs
            # are unavailable (param/local/member collisions by identifier text).
            variable_names = {decl.name for decl in decls if decl.kind in {"param", "local", "member", "global"}}
            dropped = 0
            for name in list(rename_map.keys()):
                if name in variable_names:
                    rename_map.pop(name, None)
                    dropped += 1
            if dropped > 0:
                warn_once(
                    "naming_conventions_textual_variable_rename_disabled",
                    "naming_conventions: semantic context unavailable; skipped textual variable renames for safety",
                )

        if not rename_map and not conflicts and not (
            self._use_semantic_rename and isinstance(semantic_context, SemanticContext)
        ):
            return PolicyResult(text=text, violations=[], edits=[])

        semantic_decisions: list[_SemanticRenameDecision] = []
        semantic_skips: list[str] = []
        used_semantic = self._use_semantic_rename and isinstance(semantic_context, SemanticContext)
        if used_semantic:
            updated, edits, semantic_decisions, semantic_skips = self._apply_semantic_renames(
                NamingConventionsPolicy.SemanticRenameArgs(
                    text=text,
                    rename_map=rename_map,
                    semantic=semantic_context,
                    file_counts=semantic_file_counts,
                    consensus_scores=semantic_consensus_scores,
                    reference_consensus_scores=semantic_reference_consensus_scores,
                    declaration_consensus_scores=semantic_declaration_consensus_scores,
                    reference_counts=semantic_reference_counts,
                    scope_purity=semantic_scope_purity,
                    project_reference_counts=semantic_project_reference_counts,
                    project_consensus_scores=semantic_project_consensus_scores,
                    refs_by_usr=semantic_refs_by_usr,
                    non_declaration_ref_counts=semantic_non_decl_ref_counts,
                    class_names=semantic_class_names,
                )
            )
        elif use_tree and context.tree_sitter_tree is not None:
            updated, edits = self._apply_renames_tree_sitter(
                text,
                rename_map,
                context.tree_sitter_tree,
            )
        else:
            warn_once(
                "naming_conventions_no_tree_for_textual_rename",
                "naming_conventions: semantic rename disabled and tree-sitter unavailable; skipping rename pass",
            )
            updated, edits = text, []

        violations: list[Violation] = []
        if used_semantic:
            for decision in semantic_decisions:
                violations.append(
                    Violation(
                        policy=self.name,
                        message=(
                            f"Semantic rename '{decision.old_name}' -> '{decision.new_name}' "
                            f"[risk={decision.risk} confidence={decision.confidence:.2f} refs={decision.reference_count} "
                            f"consensus={decision.parser_consensus:.2f} scope_purity={decision.scope_purity:.2f}]"
                        ),
                            line=decision.line,
                            column=1,
                        )
                    )
        else:
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

        for message in semantic_skips:
            violations.append(
                Violation(
                    policy=self.name,
                    message=message,
                    line=1,
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

    def _config_set(self, key: str) -> set[str]:
        value = self._config.get(key, None)
        if value is None:
            return set()
        if isinstance(value, (list, tuple, set)):
            result = {str(item).strip() for item in value if str(item).strip()}
            return result
        return set()

    def _build_standards(self) -> dict[str, dict[str, object]]:
        config_standards = self._config.get("standards")
        standards: dict[str, dict[str, object]] = {}
        if isinstance(config_standards, Mapping):
            for raw_name, raw_rules in config_standards.items():
                if not isinstance(raw_rules, Mapping):
                    continue
                name = str(raw_name).strip().lower()
                if not name:
                    continue
                standards[name] = {str(key): value for key, value in raw_rules.items()}

        if not standards:
            standards["mj"] = self._fallback_standard()
            return standards

        if "mj" not in standards:
            first = next(iter(standards.values()))
            standards["mj"] = dict(first)
        return standards

    def _fallback_standard(self) -> dict[str, object]:
        return {
            "local_prefix": "_",
            "member_prefix": "m_",
            "global_prefix": "g_",
            "static_prefix": "s_",
            "const_prefix": "c_",
            "atomic_prefix": "a_",
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
        }

    @dataclass(frozen=True)
    class TreeDeclArgs:
        text: str
        tree: Any

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
        template_param_names = self._collect_template_params_from_tree(root, data)

        def node_text(node: Any) -> str:
            return data[node.start_byte:node.end_byte].decode("utf-8", errors="ignore")

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

        def looks_like_macro_name(name: str) -> bool:
            return bool(name) and name.isupper() and "_" in name

        def extract_rightmost_identifier(node: Any) -> Any | None:
            best = None
            stack = [node]
            while stack:
                current = stack.pop()
                if current.type in {"identifier", "field_identifier"}:
                    if best is None or current.start_byte > best.start_byte:
                        best = current
                if current.type in {"parameter_list", "template_parameter_list", "template_parameter"}:
                    continue
                stack.extend(reversed(current.children))
            return best

        def contains_descendant_type(node: Any, types: set[str]) -> bool:
            stack_local = [node]
            while stack_local:
                current = stack_local.pop()
                if current.type in types:
                    return True
                stack_local.extend(reversed(current.children))
            return False

        def unwrap_declarator_name_node(node: Any) -> Any | None:
            current = node
            while current is not None:
                if current.type in {"identifier", "field_identifier"}:
                    return current
                if current.type in {"qualified_identifier", "scoped_identifier"}:
                    return extract_rightmost_identifier(current)
                next_decl = current.child_by_field_name("declarator")
                if next_decl is not None and next_decl is not current:
                    current = next_decl
                    continue
                child_decl = None
                for child in current.children:
                    if child.type in {
                        "pointer_declarator",
                        "reference_declarator",
                        "array_declarator",
                        "parenthesized_declarator",
                        "qualified_identifier",
                        "scoped_identifier",
                        "identifier",
                        "field_identifier",
                    }:
                        child_decl = child
                        break
                current = child_decl
            return None

        def declaration_name_nodes(node: Any) -> list[tuple[Any, Any]]:
            pairs: list[tuple[Any, Any]] = []
            if node.type == "declaration":
                for child in node.children:
                    if child.type == "init_declarator":
                        if contains_descendant_type(child, {"function_declarator"}):
                            continue
                        name_node = unwrap_declarator_name_node(child)
                        if name_node is not None:
                            pairs.append((name_node, node))
                    elif child.type in {"identifier", "field_identifier", "pointer_declarator", "reference_declarator"}:
                        if contains_descendant_type(child, {"function_declarator"}):
                            continue
                        name_node = unwrap_declarator_name_node(child)
                        if name_node is not None:
                            pairs.append((name_node, node))
                return pairs
            if node.type == "field_declaration":
                if contains_descendant_type(node, {"function_declarator"}):
                    return pairs
                name_node = unwrap_declarator_name_node(node)
                if name_node is not None:
                    pairs.append((name_node, node))
            return pairs

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
                    "template_parameter_list",
                },
            )

        def template_type_from_prefix(prefix: str) -> bool:
            token = self._last_identifier_token(prefix)
            return bool(token) and token in template_param_names

        def extract_qualified_parts(node: Any) -> list[str]:
            return self._identifier_tokens(node_text(node), include_tilde=True)

        stack = [root]
        while stack:
            node = stack.pop()
            node_type = node.type

            if node_type in {"class_specifier", "struct_specifier"}:
                name = None
                has_body = False
                for child in node.children:
                    if child.type == "type_identifier":
                        name = node_text(child)
                    if child.type == "field_declaration_list":
                        has_body = True
                if name is None:
                    name = extract_identifier(node, allow_type=True)
                if not name or looks_like_macro_name(name) or not has_body:
                    line_no = node.start_point[0]
                    if 0 <= line_no < len(lines):
                        fallback = self._extract_class_name(lines[line_no])
                        if fallback and not looks_like_macro_name(fallback):
                            name = fallback
                if name and not looks_like_macro_name(name):
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
                if name:
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
                    name_node = extract_rightmost_identifier(func_decl)
                    name = node_text(name_node) if name_node is not None else None
                    if name is None:
                        name = extract_identifier(func_decl, allow_type=False)
                    if name:
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
                    if name:
                        prefix = decl_prefix_text(node, name_node)
                        is_constexpr = "constexpr" in prefix
                        is_consteval = "consteval" in prefix
                        is_static = "static" in prefix
                        is_const = "const" in prefix and not is_constexpr and not is_consteval
                        is_atomic = self._is_atomic_type(prefix)
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
                                kind="param",
                                is_static=is_static,
                                is_const=is_const,
                                is_constexpr=is_constexpr,
                                is_consteval=is_consteval,
                                is_atomic=is_atomic,
                                is_pointer=is_ptr,
                                smart_ptr=smart_ptr,
                                is_template_type=is_template_type,
                                is_std_function=is_std_function,
                                scope_name=None,
                                line=name_node.start_point[0] + 1,
                            )
                        )

            if node_type in {"field_declaration", "declaration"}:
                for name_node, decl_node in declaration_name_nodes(node):
                    if is_template_param_node(name_node):
                        continue
                    if has_ancestor(name_node, {"enumerator", "enum_specifier"}):
                        continue
                    name = node_text(name_node)
                    if not name:
                        continue
                    prefix = decl_prefix_text(decl_node, name_node)
                    is_constexpr = "constexpr" in prefix
                    is_consteval = "consteval" in prefix
                    is_static = "static" in prefix
                    is_const = "const" in prefix and not is_constexpr and not is_consteval
                    is_atomic = self._is_atomic_type(prefix)
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
                            is_atomic=is_atomic,
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

    def _build_rename_map(self, decls: list[_Decl]) -> tuple[dict[str, str], dict[str, str]]:
        rename: dict[str, str] = {}
        conflicts: dict[str, str] = {}
        protected: set[str] = set()
        class_names = {d.name for d in decls if d.kind in {"class", "struct"}}

        for decl in decls:
            if decl.name in self._reserved_identifiers:
                continue
            if decl.name in self._skip_name_keywords:
                continue
            if decl.name in self._builtin_types:
                continue
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
            base_name = decl.name.lstrip("~")
            if decl.name in class_names or base_name in class_names:
                return decl.name
            if decl.scope_name and (
                decl.name == decl.scope_name or base_name == decl.scope_name
            ):
                return decl.name
            if decl.scope_name and (
                self._normalized_identifier_key(base_name)
                == self._normalized_identifier_key(decl.scope_name)
            ):
                return f"~{decl.scope_name}" if decl.name.startswith("~") else decl.scope_name
            return self._to_snake(decl.name)

        if decl.kind in {"param", "local", "member", "global"}:
            return self._name_variable(decl)

        return None

    @dataclass(frozen=True)
    class SemanticRenameArgs:
        text: str
        rename_map: dict[str, str]
        semantic: SemanticContext
        file_counts: dict[str, int]
        consensus_scores: dict[str, float]
        reference_consensus_scores: dict[str, float]
        declaration_consensus_scores: dict[str, float]
        reference_counts: dict[str, int]
        scope_purity: dict[str, float]
        project_reference_counts: dict[str, int]
        project_consensus_scores: dict[str, float]
        refs_by_usr: Mapping[str, tuple[SemanticReference, ...]]
        non_declaration_ref_counts: Mapping[str, int]
        class_names: tuple[str, ...]

    def _apply_semantic_renames(
        self,
        args: "NamingConventionsPolicy.SemanticRenameArgs",
    ) -> tuple[str, list[Edit], list[_SemanticRenameDecision], list[str]]:
        symbols = list(args.semantic.symbols)
        code_mask = self._code_mask(args.text)
        code_text = self._masked_text(args.text, code_mask)
        code_occurrence_map = self._identifier_count_map(code_text, include_tilde=True)
        text_occurrence_map = self._identifier_count_map(args.text, include_tilde=True)
        text_bytes = args.text.encode("utf-8", errors="ignore")
        refs_by_usr: Mapping[str, tuple[SemanticReference, ...]] = args.refs_by_usr
        non_decl_ref_count_by_usr: Mapping[str, int] = args.non_declaration_ref_counts
        class_names = set(args.class_names)
        if not refs_by_usr:
            local_refs: dict[str, list[SemanticReference]] = {}
            local_non_decl_counts: dict[str, int] = {}
            for ref in args.semantic.references:
                local_refs.setdefault(ref.usr, []).append(ref)
                if not ref.is_declaration:
                    local_non_decl_counts[ref.usr] = local_non_decl_counts.get(ref.usr, 0) + 1
            refs_by_usr = {usr: tuple(items) for usr, items in local_refs.items()}
            non_decl_ref_count_by_usr = local_non_decl_counts
        if not class_names:
            class_names = {
                symbol.name
                for symbol in symbols
                if symbol.scope_kind in {"class", "struct"}
            }

        replacements: dict[tuple[int, int], str] = {}
        decisions: list[_SemanticRenameDecision] = []
        skips: list[str] = []
        processed_usrs: set[str] = set()

        for symbol in symbols:
            if symbol.usr in processed_usrs:
                continue
            processed_usrs.add(symbol.usr)
            old = symbol.name
            if not self._is_identifier_name(old):
                continue
            if old in self._reserved_identifiers or old in self._skip_name_keywords or old in self._builtin_types:
                continue

            target = None
            if symbol.scope_kind not in {"param", "local", "member", "global"}:
                target = args.rename_map.get(old)
            if target is None:
                semantic_decl = self._decl_from_semantic(symbol)
                if semantic_decl is None:
                    continue
                target = self._target_name(semantic_decl, class_names)
            if not target or target == old:
                continue

            refs = list(refs_by_usr.get(symbol.usr, ()))
            non_decl_ref_count = int(non_decl_ref_count_by_usr.get(symbol.usr, 0))
            code_occurrence_count = int(code_occurrence_map.get(old, 0))
            if code_occurrence_count > len(refs):
                skips.append(
                    f"Skipped semantic rename for '{old}': incomplete reference map "
                    f"(occurrences={code_occurrence_count}, refs={len(refs)})"
                )
                continue
            project_ref_count = int(args.project_reference_counts.get(symbol.usr, 0))
            if symbol.scope_kind in {"member", "global"} and non_decl_ref_count <= 0 and project_ref_count <= 0:
                skips.append(
                    f"Skipped semantic rename for '{old}': no non-declaration reference evidence "
                    "(member/global symbols require references)"
                )
                continue
            if non_decl_ref_count <= 0:
                occurrence_count = int(text_occurrence_map.get(old, 0))
                if occurrence_count > 1:
                    skips.append(
                        f"Skipped semantic rename for '{old}': incomplete reference map "
                        f"(occurrences={occurrence_count}, refs={len(refs)})"
                    )
                    continue
            parser_consensus = float(args.consensus_scores.get(symbol.usr, symbol.parser_consensus))
            reference_consensus = float(args.reference_consensus_scores.get(symbol.usr, parser_consensus))
            declaration_consensus = float(args.declaration_consensus_scores.get(symbol.usr, parser_consensus))
            project_consensus = float(args.project_consensus_scores.get(symbol.usr, 0.0))
            parser_consensus = self._blended_parser_consensus(
                local_consensus=parser_consensus,
                reference_consensus=reference_consensus,
                declaration_consensus=declaration_consensus,
                project_consensus=project_consensus,
                project_ref_count=project_ref_count,
            )
            if self._parser_consensus_mode == ParserConsensusMode.OFF:
                parser_consensus = 1.0
            scope_purity = float(args.scope_purity.get(symbol.usr, 1.0))
            if self._strict_local_scope and symbol.scope_kind in {"param", "local"} and scope_purity < 0.999:
                skips.append(
                    f"Skipped semantic rename for '{old}': local symbol escapes function scope "
                    f"(scope_purity={scope_purity:.2f})"
                )
                continue
            if self._parser_consensus_mode == ParserConsensusMode.STRICT and parser_consensus < self._parser_consensus_min:
                skips.append(
                    f"Skipped semantic rename for '{old}': parser consensus {parser_consensus:.2f} "
                    f"below strict threshold {self._parser_consensus_min:.2f}"
                )
                continue
            risk, confidence = self._semantic_risk_and_confidence(
                refs=refs,
                file_count=int(args.file_counts.get(symbol.usr, 1)),
                project_ref_count=project_ref_count,
                parser_consensus=parser_consensus,
                reference_consensus=reference_consensus,
                declaration_consensus=declaration_consensus,
                scope_purity=scope_purity,
                is_local=symbol.scope_kind in {"param", "local"},
                ref_count_hint=int(args.reference_counts.get(symbol.usr, 0)),
            )
            if confidence < self._min_confidence:
                skips.append(
                    f"Skipped semantic rename for '{old}': confidence {confidence:.2f} below {self._min_confidence:.2f}"
                )
                continue
            if self._risk_rank.get(risk, 2) > self._max_risk_rank:
                skips.append(
                    f"Skipped semantic rename for '{old}': risk {risk} exceeds max_risk {self._max_risk}"
                )
                continue

            if not refs:
                refs = [
                    SemanticReference(
                        usr=symbol.usr,
                        start=symbol.start,
                        end=symbol.end,
                        line=symbol.line,
                        column=symbol.column,
                        is_declaration=True,
                    )
                ]

            conflict = False
            for ref in refs:
                key = (ref.start, ref.end)
                if ref.end <= ref.start:
                    continue
                token = text_bytes[ref.start : ref.end].decode("utf-8", errors="ignore")
                if token != old and token != old.lstrip("~"):
                    conflict = True
                    skips.append(
                        f"Skipped semantic rename for '{old}': reference/token mismatch at {ref.line}:{ref.column} "
                        f"(token='{token}')"
                    )
                    break
                existing = replacements.get(key)
                if existing is None:
                    replacements[key] = target
                elif existing != target:
                    conflict = True
                    break
            if conflict:
                skips.append(f"Skipped semantic rename for '{old}': overlapping replacement conflict")
                continue

            ref_count = non_decl_ref_count
            decisions.append(
                _SemanticRenameDecision(
                    usr=symbol.usr,
                    old_name=old,
                    new_name=target,
                    line=symbol.line,
                    risk=risk,
                    confidence=confidence,
                    reference_count=ref_count,
                    parser_consensus=parser_consensus,
                    scope_purity=scope_purity,
                )
            )

        if not replacements:
            return args.text, [], decisions, skips

        data = args.text.encode("utf-8")
        for (start, end), value in sorted(replacements.items(), key=lambda item: item[0][0], reverse=True):
            if end <= start:
                continue
            data = data[:start] + value.encode("utf-8") + data[end:]
        updated = data.decode("utf-8")

        edits: list[Edit] = []
        if updated != args.text:
            before_lines = args.text.splitlines(keepends=True)
            after_lines = updated.splitlines(keepends=True)
            for idx, (before, after) in enumerate(zip(before_lines, after_lines)):
                if before != after:
                    edits.append(
                        Edit(
                            policy=self.name,
                            line=idx + 1,
                            before=before.rstrip("\r\n"),
                            after=after.rstrip("\r\n"),
                        )
                    )
        return updated, edits, decisions, skips

    def _decl_from_semantic(self, symbol: SemanticSymbol) -> _Decl | None:
        kind = symbol.scope_kind
        if kind not in {"param", "local", "member", "global", "function", "class", "struct", "namespace"}:
            return None
        return _Decl(
            name=symbol.name,
            kind=kind,
            is_static=symbol.is_static,
            is_const=symbol.is_const,
            is_constexpr=symbol.is_constexpr,
            is_consteval=symbol.is_consteval,
            is_atomic=bool(getattr(symbol, "is_atomic", False)),
            is_pointer=symbol.is_pointer,
            smart_ptr=symbol.smart_ptr,
            is_template_type=symbol.is_template_type,
            is_std_function=symbol.is_std_function,
            scope_name=symbol.scope_name,
            line=symbol.line,
        )

    def _semantic_risk_and_confidence(
        self,
        refs: list[SemanticReference],
        file_count: int,
        project_ref_count: int,
        parser_consensus: float,
        reference_consensus: float,
        declaration_consensus: float,
        scope_purity: float,
        is_local: bool,
        ref_count_hint: int,
    ) -> tuple[str, float]:
        ref_uses = len([ref for ref in refs if not ref.is_declaration])
        if ref_uses <= 0:
            ref_uses = max(0, ref_count_hint)

        risk = "low"
        confidence = 0.96 if ref_uses > 0 else 0.80

        if file_count > 1:
            risk = "high"
            confidence = min(confidence, 0.55)
        elif project_ref_count > 100:
            risk = "high"
            confidence = min(confidence, 0.62)
        elif project_ref_count > 20 or ref_uses == 0:
            risk = "medium"
            confidence = min(confidence, 0.80)

        if is_local and scope_purity < 0.999:
            risk = "high"
            confidence = min(confidence, 0.45 * max(0.2, scope_purity))

        parser_consensus_norm = self._clamp01(parser_consensus)
        reference_consensus_norm = self._clamp01(reference_consensus)
        declaration_consensus_norm = self._clamp01(declaration_consensus)
        consensus_signal = (
            (0.65 * parser_consensus_norm)
            + (0.20 * reference_consensus_norm)
            + (0.15 * declaration_consensus_norm)
        )

        if reference_consensus_norm < 0.50 and ref_uses > 0:
            if self._risk_rank.get(risk, 0) < self._risk_rank.get("medium", 1):
                risk = "medium"
            confidence *= 0.80
        if declaration_consensus_norm < 0.40:
            if self._risk_rank.get(risk, 0) < self._risk_rank.get("medium", 1):
                risk = "medium"
            confidence *= 0.88

        if consensus_signal < 0.50:
            risk = "high"
            confidence *= 0.45
        elif consensus_signal < self._parser_consensus_min:
            if self._risk_rank.get(risk, 0) < self._risk_rank.get("medium", 1):
                risk = "medium"
            confidence *= 0.75
        else:
            confidence *= 0.90 + (0.10 * consensus_signal)

        confidence = max(0.0, min(1.0, confidence))
        return risk, confidence

    @staticmethod
    def _clamp01(value: float) -> float:
        return max(0.0, min(1.0, float(value)))

    def _blended_parser_consensus(
        self,
        local_consensus: float,
        reference_consensus: float,
        declaration_consensus: float,
        project_consensus: float,
        project_ref_count: int,
    ) -> float:
        local = (
            (0.60 * self._clamp01(local_consensus))
            + (0.25 * self._clamp01(reference_consensus))
            + (0.15 * self._clamp01(declaration_consensus))
        )
        project = self._clamp01(project_consensus)
        if project <= 0.0:
            return local
        if project_ref_count <= 10:
            project_weight = 0.15
        elif project_ref_count <= 50:
            project_weight = 0.25
        else:
            project_weight = 0.35
        return self._clamp01((local * (1.0 - project_weight)) + (project * project_weight))

    def _name_variable(self, decl: _Decl) -> str:
        rules = self._rules

        if decl.is_constexpr or decl.is_consteval:
            prefixes = []
            if decl.is_static:
                prefixes.append(rules["static_prefix_upper"])
            prefixes.append(rules["constexpr_prefix_upper"])
            if decl.is_template_type:
                prefixes.append("T_")
            atomic_upper = str(rules.get("atomic_prefix", "a_")).upper()
            if decl.is_atomic and atomic_upper:
                prefixes.append(atomic_upper)
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
            stripped = self._strip_prefixes(decl.name, self._prefix_strip_candidates(prefixes))
            base = self._to_upper_snake(stripped or decl.name)
            return "".join(prefixes) + base

        prefixes = []
        is_param = decl.kind == "param"

        if decl.kind == "global" and rules.get("global_prefix"):
            prefixes.append(rules["global_prefix"])
        if decl.kind == "member" and rules.get("member_prefix"):
            prefixes.append(rules["member_prefix"])
        if decl.kind == "local" and rules.get("local_prefix"):
            prefixes.append(rules["local_prefix"])
        if not is_param and decl.is_static and rules.get("static_prefix"):
            prefixes.append(rules["static_prefix"])
        if not is_param and decl.is_const and rules.get("const_prefix"):
            prefixes.append(rules["const_prefix"])
        if not is_param and decl.is_template_type:
            prefixes.append("t_")
        if decl.is_atomic and rules.get("atomic_prefix"):
            prefixes.append(str(rules["atomic_prefix"]))
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
        strip_prefixes = self._prefix_strip_candidates(prefixes)
        if is_param:
            strip_prefixes.extend(
                self._prefix_strip_candidates(
                    [
                        str(rules.get("local_prefix") or ""),
                        str(rules.get("static_prefix") or ""),
                        str(rules.get("const_prefix") or ""),
                        str(rules.get("atomic_prefix") or ""),
                        "t_",
                    ]
                )
            )
        if rules.get("constexpr_prefix_upper"):
            strip_prefixes.append(str(rules["constexpr_prefix_upper"]))
        if rules.get("static_prefix_upper"):
            strip_prefixes.append(str(rules["static_prefix_upper"]))
        stripped = self._strip_prefixes(base_name, strip_prefixes)
        base = self._to_snake(stripped or decl.name)
        return "".join(prefixes) + base

    def _is_identifier_name(self, name: str) -> bool:
        if not name:
            return False
        value = name[1:] if name.startswith("~") else name
        if not value:
            return False
        head = value[0]
        if not (head.isalpha() or head == "_"):
            return False
        for ch in value[1:]:
            if not (ch.isalnum() or ch == "_"):
                return False
        return True

    def _collect_template_params_from_tree(self, root: Any, data: bytes) -> set[str]:
        params: set[str] = set()
        stack = [root]
        while stack:
            node = stack.pop()
            node_type = getattr(node, "type", "")
            if node_type in {"template_parameter", "template_type_parameter", "type_parameter_declaration"}:
                tokens = self._identifier_tokens(
                    data[node.start_byte : node.end_byte].decode("utf-8", errors="ignore")
                )
                if tokens:
                    params.add(tokens[-1])
            stack.extend(reversed(getattr(node, "children", [])))
        return params

    def _collect_declarations_semantic(self, semantic: SemanticContext) -> list[_Decl]:
        decls: list[_Decl] = []
        seen: set[tuple[str, str, int, str | None]] = set()
        for symbol in semantic.symbols:
            decl = self._decl_from_semantic(symbol)
            if decl is None:
                continue
            key = (decl.name, decl.kind, decl.line, decl.scope_name)
            if key in seen:
                continue
            seen.add(key)
            decls.append(decl)
        return decls

    def _dedupe_decls(self, decls: list[_Decl]) -> list[_Decl]:
        result: list[_Decl] = []
        seen_index: dict[tuple[str, str, int, str | None], int] = {}
        for decl in decls:
            key = (decl.name, decl.kind, decl.line, decl.scope_name)
            index = seen_index.get(key)
            if index is not None:
                existing = result[index]
                result[index] = _Decl(
                    name=existing.name,
                    kind=existing.kind,
                    is_static=existing.is_static or decl.is_static,
                    is_const=existing.is_const or decl.is_const,
                    is_constexpr=existing.is_constexpr or decl.is_constexpr,
                    is_consteval=existing.is_consteval or decl.is_consteval,
                    is_atomic=existing.is_atomic or decl.is_atomic,
                    is_pointer=existing.is_pointer or decl.is_pointer,
                    smart_ptr=existing.smart_ptr or decl.smart_ptr,
                    is_template_type=existing.is_template_type or decl.is_template_type,
                    is_std_function=existing.is_std_function or decl.is_std_function,
                    scope_name=existing.scope_name or decl.scope_name,
                    line=existing.line or decl.line,
                )
                continue
            seen_index[key] = len(result)
            result.append(decl)
        return result

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
        tokens = self._identifier_tokens(line)
        if not tokens:
            return None
        marker_index = -1
        for keyword in ("class", "struct"):
            try:
                marker_index = tokens.index(keyword)
                break
            except ValueError:
                continue
        if marker_index < 0:
            return None
        ignored = {"final", "sealed", "alignas", "constexpr", "consteval"}
        for ident in tokens[marker_index + 1 :]:
            if ident in ignored:
                continue
            if ident.isupper() and "_" in ident:
                continue
            return ident
        return None

    def _scan_identifiers(self, text: str, include_tilde: bool = False) -> list[tuple[int, int, str]]:
        spans: list[tuple[int, int, str]] = []
        length = len(text)
        index = 0
        while index < length:
            ch = text[index]
            start = -1
            if ch == "~" and include_tilde:
                next_index = index + 1
                if next_index < length and (text[next_index].isalpha() or text[next_index] == "_"):
                    prev = text[index - 1] if index > 0 else ""
                    if not (prev.isalnum() or prev == "_"):
                        start = index
                        index = next_index + 1
                        while index < length and (text[index].isalnum() or text[index] == "_"):
                            index += 1
            elif ch.isalpha() or ch == "_":
                prev = text[index - 1] if index > 0 else ""
                if not (prev.isalnum() or prev == "_"):
                    start = index
                    index += 1
                    while index < length and (text[index].isalnum() or text[index] == "_"):
                        index += 1
            if start >= 0:
                spans.append((start, index, text[start:index]))
                continue
            index += 1
        return spans

    def _identifier_tokens(self, text: str, include_tilde: bool = False) -> list[str]:
        return [token for _, _, token in self._scan_identifiers(text, include_tilde=include_tilde)]

    def _last_identifier_token(self, text: str) -> str | None:
        tokens = self._identifier_tokens(text)
        if not tokens:
            return None
        return tokens[-1]

    def _is_atomic_type(self, value: str) -> bool:
        if not value:
            return False
        for token in self._identifier_tokens(value):
            lowered = token.lower()
            if lowered == "_atomic":
                return True
            if lowered == "atomic" or lowered.startswith("atomic_"):
                return True
        return False

    def _to_snake(self, name: str) -> str:
        if name.isupper() and "_" in name:
            return name.lower()
        result: list[str] = []
        for idx, ch in enumerate(name):
            if ch == "_" or ch.isspace():
                if result and result[-1] != "_":
                    result.append("_")
                continue
            if ch.isupper():
                prev = name[idx - 1] if idx > 0 else ""
                nxt = name[idx + 1] if idx + 1 < len(name) else ""
                needs_sep = idx > 0 and (
                    prev.islower()
                    or prev.isdigit()
                    or (prev.isupper() and bool(nxt) and nxt.islower())
                )
                if needs_sep and result and result[-1] != "_":
                    result.append("_")
                result.append(ch.lower())
                continue
            result.append(ch.lower())
        compact: list[str] = []
        for ch in result:
            if ch == "_" and compact and compact[-1] == "_":
                continue
            compact.append(ch)
        return "".join(compact)

    def _to_upper_snake(self, name: str) -> str:
        return self._to_snake(name).upper()

    def _to_camel(self, name: str) -> str:
        parts: list[str] = []
        current: list[str] = []
        for ch in name:
            if ch == "_" or ch.isspace():
                if current:
                    parts.append("".join(current))
                    current = []
                continue
            current.append(ch)
        if current:
            parts.append("".join(current))
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

    def _normalized_identifier_key(self, name: str) -> str:
        return "".join(ch for ch in str(name or "") if ch != "_" and not ch.isspace()).lower()

    def _identifier_count_map(self, text: str, include_tilde: bool = False) -> dict[str, int]:
        counts: dict[str, int] = {}
        for _, _, token in self._scan_identifiers(text, include_tilde=include_tilde):
            counts[token] = counts.get(token, 0) + 1
        return counts

    def _prefix_strip_candidates(self, prefixes: list[str]) -> list[str]:
        candidates: list[str] = []
        seen: set[str] = set()
        for prefix in prefixes:
            if not prefix:
                continue
            for candidate in (prefix, prefix.lower(), prefix.upper()):
                if not candidate or candidate in seen:
                    continue
                seen.add(candidate)
                candidates.append(candidate)
        return candidates
