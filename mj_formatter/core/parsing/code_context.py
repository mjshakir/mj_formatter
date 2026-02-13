from __future__ import annotations

from dataclasses import replace
from pathlib import Path
from typing import Any

from .clang_decls import ClangDeclCollector, ClangFunctionDecl, ClangVarDecl
from ..types.context import (
    CodeBlock,
    CodeContext,
    SemanticContext,
    SemanticReference,
    SemanticSymbol,
    TreeContextData,
    TreeDeclaration,
)

def build_code_context(
    path: str,
    text: str,
    clang_ast: Any | None,
    tree_sitter_tree: Any | None,
    project_index_cache: Any | None = None,
) -> CodeContext:
    tree_context = _collect_tree_context(tree_sitter_tree, text)
    clang_functions: tuple[ClangFunctionDecl, ...] = ()
    clang_variables: tuple[ClangVarDecl, ...] = ()
    semantic_context: SemanticContext | None = None
    semantic_refs_by_usr: dict[str, tuple[SemanticReference, ...]] = {}
    semantic_non_declaration_ref_counts: dict[str, int] = {}
    semantic_function_symbols: tuple[SemanticSymbol, ...] = ()
    semantic_class_names: tuple[str, ...] = ()
    semantic_file_counts: dict[str, int] = {}
    semantic_consensus_scores: dict[str, float] = {}
    semantic_reference_consensus_scores: dict[str, float] = {}
    semantic_declaration_consensus_scores: dict[str, float] = {}
    semantic_reference_counts: dict[str, int] = {}
    semantic_scope_purity: dict[str, float] = {}
    semantic_project_reference_counts: dict[str, int] = {}
    semantic_project_consensus_scores: dict[str, float] = {}
    semantic_hybrid_confidence = 0.0
    hybrid_blocks: tuple[CodeBlock, ...] = tree_context.blocks

    if clang_ast is not None:
        collector = ClangDeclCollector(clang_ast, path)
        clang_functions = tuple(collector.functions())
        clang_variables = tuple(collector.variables())

        loaded_from_cache = False
        if project_index_cache is not None:
            semantic_context = project_index_cache.get_semantic(path, text)
            loaded_from_cache = semantic_context is not None
        if semantic_context is None:
            semantic_context = _collect_semantic_context(clang_ast, path)
        (
            semantic_context,
            semantic_reference_consensus_scores,
            semantic_declaration_consensus_scores,
        ) = _apply_parser_consensus(
            semantic=semantic_context,
            text=text,
            tree_context=tree_context,
        )
        if project_index_cache is not None and not loaded_from_cache:
            project_index_cache.put_semantic(path, text, semantic_context)

        (
            semantic_refs_by_usr,
            semantic_non_declaration_ref_counts,
            semantic_function_symbols,
            semantic_class_names,
        ) = _build_semantic_indexes(semantic_context)

        semantic_consensus_scores = _pairs_to_float_map(semantic_context.consensus_by_usr)
        semantic_reference_counts = _pairs_to_int_map(semantic_context.reference_count_by_usr)
        if semantic_consensus_scores:
            weighted_total = 0.0
            total_weight = 0
            for usr, score in semantic_consensus_scores.items():
                weight = max(1, int(semantic_reference_counts.get(usr, 0)))
                weighted_total += float(score) * float(weight)
                total_weight += weight
            if total_weight > 0:
                semantic_hybrid_confidence = weighted_total / float(total_weight)
            else:
                semantic_hybrid_confidence = (
                    sum(semantic_consensus_scores.values()) / float(len(semantic_consensus_scores))
                )
        semantic_scope_purity = _pairs_to_float_map(semantic_context.scope_purity_by_usr)
        if project_index_cache is not None:
            usrs = {symbol.usr for symbol in semantic_context.symbols if symbol.usr}
            semantic_project_reference_counts = {
                usr: int(project_index_cache.symbol_reference_count(usr))
                for usr in usrs
            }
            semantic_project_consensus_scores = {
                usr: float(project_index_cache.symbol_consensus_score(usr))
                for usr in usrs
            }
            semantic_file_counts = {
                usr: int(project_index_cache.symbol_file_count(usr))
                for usr in usrs
            }
        hybrid_blocks = _merge_blocks_with_semantic(tree_context.blocks, semantic_context)

    return CodeContext(
        clang_functions=clang_functions,
        clang_variables=clang_variables,
        semantic_context=semantic_context,
        semantic_refs_by_usr=semantic_refs_by_usr,
        semantic_non_declaration_ref_counts=semantic_non_declaration_ref_counts,
        semantic_function_symbols=semantic_function_symbols,
        semantic_class_names=semantic_class_names,
        semantic_file_counts=semantic_file_counts,
        tree_root_type=tree_context.root_type,
        tree_node_count=tree_context.node_count,
        tree_identifier_count=len(tree_context.identifier_spans),
        tree_declarations=tree_context.declarations,
        hybrid_blocks=hybrid_blocks,
        semantic_consensus_scores=semantic_consensus_scores,
        semantic_reference_consensus_scores=semantic_reference_consensus_scores,
        semantic_declaration_consensus_scores=semantic_declaration_consensus_scores,
        semantic_reference_counts=semantic_reference_counts,
        semantic_scope_purity=semantic_scope_purity,
        semantic_project_reference_counts=semantic_project_reference_counts,
        semantic_project_consensus_scores=semantic_project_consensus_scores,
        semantic_hybrid_confidence=semantic_hybrid_confidence,
    )


def _collect_semantic_context(clang_ast: Any, path: str) -> SemanticContext:
    try:
        from clang import cindex
    except Exception:
        return SemanticContext(symbols=(), references=())

    target = str(Path(path).resolve())
    declaration_kinds = {
        cindex.CursorKind.NAMESPACE,
        cindex.CursorKind.CLASS_DECL,
        cindex.CursorKind.STRUCT_DECL,
        cindex.CursorKind.CLASS_TEMPLATE,
        cindex.CursorKind.FUNCTION_DECL,
        cindex.CursorKind.CXX_METHOD,
        cindex.CursorKind.CONSTRUCTOR,
        cindex.CursorKind.DESTRUCTOR,
        cindex.CursorKind.FUNCTION_TEMPLATE,
        cindex.CursorKind.VAR_DECL,
        cindex.CursorKind.FIELD_DECL,
        cindex.CursorKind.PARM_DECL,
    }
    function_kinds = {
        cindex.CursorKind.FUNCTION_DECL,
        cindex.CursorKind.CXX_METHOD,
        cindex.CursorKind.CONSTRUCTOR,
        cindex.CursorKind.DESTRUCTOR,
        cindex.CursorKind.FUNCTION_TEMPLATE,
    }
    class_kinds = {
        cindex.CursorKind.CLASS_DECL,
        cindex.CursorKind.STRUCT_DECL,
        cindex.CursorKind.CLASS_TEMPLATE,
    }

    symbols: list[SemanticSymbol] = []
    references: list[SemanticReference] = []
    seen_symbols: set[tuple[str, int, int]] = set()
    seen_refs: set[tuple[str, int, int, bool]] = set()

    def in_main_file(cursor: Any) -> bool:
        loc = getattr(cursor, "location", None)
        if loc is None or getattr(loc, "file", None) is None:
            return False
        try:
            return str(Path(str(loc.file)).resolve()) == target
        except Exception:
            return False

    def cursor_name_span(cursor: Any, fallback_to_extent: bool = True) -> tuple[int, int, int, int] | None:
        spelling = getattr(cursor, "spelling", "") or ""
        display = getattr(cursor, "displayname", "") or ""
        try:
            tokens = list(cursor.get_tokens())
        except Exception:
            tokens = []

        for token in tokens:
            t = token.spelling
            if not t:
                continue
            if spelling and (t == spelling or t == spelling.lstrip("~")):
                start = int(token.extent.start.offset)
                end = int(token.extent.end.offset)
                line = int(getattr(token.location, "line", 1) or 1)
                col = int(getattr(token.location, "column", 1) or 1)
                return start, end, line, col
            if spelling.startswith("operator") and t == "operator":
                start = int(token.extent.start.offset)
                end = int(token.extent.end.offset)
                line = int(getattr(token.location, "line", 1) or 1)
                col = int(getattr(token.location, "column", 1) or 1)
                return start, end, line, col
            if display and t == display:
                start = int(token.extent.start.offset)
                end = int(token.extent.end.offset)
                line = int(getattr(token.location, "line", 1) or 1)
                col = int(getattr(token.location, "column", 1) or 1)
                return start, end, line, col

        if not fallback_to_extent:
            return None
        extent = getattr(cursor, "extent", None)
        if extent is None:
            return None
        start = int(extent.start.offset)
        end = int(extent.end.offset)
        line = int(getattr(cursor.location, "line", 1) or 1)
        col = int(getattr(cursor.location, "column", 1) or 1)
        if end <= start:
            return None
        return start, end, line, col

    def cursor_usr(cursor: Any | None) -> str | None:
        if cursor is None:
            return None
        try:
            value = cursor.get_usr() or ""
            return value or None
        except Exception:
            return None

    def enclosing_callable_usr(cursor: Any | None) -> str | None:
        current = cursor
        while current is not None:
            kind = getattr(current, "kind", None)
            if kind in function_kinds:
                usr = cursor_usr(current)
                if usr:
                    return usr
            current = getattr(current, "semantic_parent", None)
        return None

    def scope_kind(cursor: Any) -> tuple[str, str | None, str | None]:
        kind = getattr(cursor, "kind", None)
        if kind == cindex.CursorKind.NAMESPACE:
            return "namespace", None, None
        if kind == cindex.CursorKind.CLASS_DECL or kind == cindex.CursorKind.CLASS_TEMPLATE:
            return "class", None, cursor_usr(cursor)
        if kind == cindex.CursorKind.STRUCT_DECL:
            return "struct", None, cursor_usr(cursor)
        if kind in function_kinds:
            parent = getattr(cursor, "semantic_parent", None)
            if parent is not None and getattr(parent, "kind", None) in class_kinds:
                return "function", getattr(parent, "spelling", None), cursor_usr(cursor)
            return "function", None, cursor_usr(cursor)
        if kind == cindex.CursorKind.FIELD_DECL:
            parent = getattr(cursor, "semantic_parent", None)
            return (
                "member",
                getattr(parent, "spelling", None) if parent is not None else None,
                cursor_usr(parent),
            )
        if kind == cindex.CursorKind.PARM_DECL:
            parent = getattr(cursor, "semantic_parent", None)
            parent_kind = getattr(parent, "kind", None) if parent is not None else None
            if parent_kind in function_kinds:
                return (
                    "param",
                    getattr(parent, "spelling", None) if parent is not None else None,
                    cursor_usr(parent),
                )
            return "param", None, None
        if kind == cindex.CursorKind.VAR_DECL:
            parent = getattr(cursor, "semantic_parent", None)
            parent_kind = getattr(parent, "kind", None) if parent is not None else None
            if parent_kind in function_kinds:
                return (
                    "local",
                    getattr(parent, "spelling", None) if parent is not None else None,
                    cursor_usr(parent),
                )
            if parent_kind in class_kinds:
                return (
                    "member",
                    getattr(parent, "spelling", None) if parent is not None else None,
                    cursor_usr(parent),
                )
            return "global", None, None
        return "global", None, None

    stack = [clang_ast.cursor]
    while stack:
        cursor = stack.pop()
        try:
            children = list(cursor.get_children())
        except Exception:
            children = []
        stack.extend(reversed(children))

        if not in_main_file(cursor):
            continue

        kind = getattr(cursor, "kind", None)
        usr = ""
        try:
            usr = cursor.get_usr() or ""
        except Exception:
            usr = ""
        name = getattr(cursor, "spelling", "") or ""

        if usr and name and kind in declaration_kinds:
            span = cursor_name_span(cursor)
            if span is not None:
                start, end, line, col = span
                key = (usr, start, end)
                if key not in seen_symbols:
                    seen_symbols.add(key)
                    cursor_type = getattr(cursor, "type", None)
                    type_spelling = str(getattr(cursor_type, "spelling", "") or "")
                    token_spellings: tuple[str, ...] = ()
                    try:
                        token_spellings = tuple(tok.spelling for tok in cursor.get_tokens())
                    except Exception:
                        token_spellings = ()
                    token_set = set(token_spellings)
                    pointer = "*" in type_spelling
                    smart_ptr = None
                    if "shared_ptr" in type_spelling:
                        smart_ptr = "shared"
                    elif "unique_ptr" in type_spelling:
                        smart_ptr = "unique"
                    elif "weak_ptr" in type_spelling:
                        smart_ptr = "weak"
                    is_constexpr = "constexpr" in token_set or "constexpr" in type_spelling
                    is_consteval = "consteval" in token_set or "consteval" in type_spelling
                    is_static = False
                    try:
                        is_static = cursor.storage_class == cindex.StorageClass.STATIC
                    except Exception:
                        is_static = False
                    if "static" in token_set:
                        is_static = True
                    is_const = False
                    try:
                        is_const = bool(cursor_type.is_const_qualified()) and not is_constexpr and not is_consteval
                    except Exception:
                        is_const = False
                    if "const" in token_set and not is_constexpr and not is_consteval:
                        is_const = True
                    is_atomic = _is_atomic_type(type_spelling, token_set)
                    scope, scope_name, scope_usr = scope_kind(cursor)
                    symbols.append(
                        SemanticSymbol(
                            usr=usr,
                            name=name,
                            kind=getattr(kind, "name", str(kind)).lower(),
                            scope_kind=scope,
                            scope_name=scope_name,
                            line=line,
                            column=col,
                            start=start,
                            end=end,
                            is_static=is_static,
                            is_const=is_const,
                            is_constexpr=is_constexpr,
                            is_consteval=is_consteval,
                            is_atomic=is_atomic,
                            is_pointer=pointer,
                            smart_ptr=smart_ptr,
                            is_std_function="std::function" in type_spelling,
                            is_template_type=False,
                            scope_usr=scope_usr,
                        )
                    )
                    ref_key = (usr, start, end, True)
                    if ref_key not in seen_refs:
                        seen_refs.add(ref_key)
                        references.append(
                            SemanticReference(
                                usr=usr,
                                start=start,
                                end=end,
                                line=line,
                                column=col,
                                is_declaration=True,
                                scope_usr=scope_usr,
                            )
                        )

        try:
            referenced = cursor.referenced
            ref_usr = referenced.get_usr() if referenced is not None else ""
        except Exception:
            ref_usr = ""
        if not ref_usr:
            continue
        if usr and ref_usr == usr and kind in declaration_kinds:
            continue
        span = cursor_name_span(cursor, fallback_to_extent=False)
        if span is None:
            continue
        start, end, line, col = span
        ref_key = (ref_usr, start, end, False)
        if ref_key in seen_refs:
            continue
        seen_refs.add(ref_key)
        references.append(
            SemanticReference(
                usr=ref_usr,
                start=start,
                end=end,
                line=line,
                column=col,
                is_declaration=False,
                scope_usr=enclosing_callable_usr(cursor),
            )
        )

    refs_by_usr: dict[str, list[SemanticReference]] = {}
    for ref in references:
        refs_by_usr.setdefault(ref.usr, []).append(ref)

    reference_count_by_usr: dict[str, int] = {}
    for usr, ref_items in refs_by_usr.items():
        reference_count_by_usr[usr] = sum(1 for ref in ref_items if not ref.is_declaration)

    symbols_by_usr: dict[str, SemanticSymbol] = {}
    for symbol in symbols:
        symbols_by_usr.setdefault(symbol.usr, symbol)

    scope_purity_by_usr: dict[str, float] = {}
    for usr, symbol in symbols_by_usr.items():
        if symbol.scope_kind not in {"local", "param"}:
            scope_purity_by_usr[usr] = 1.0
            continue
        refs = refs_by_usr.get(usr, [])
        if not refs or not symbol.scope_usr:
            scope_purity_by_usr[usr] = 1.0
            continue
        known_scope_refs = [ref for ref in refs if ref.scope_usr]
        if not known_scope_refs:
            scope_purity_by_usr[usr] = 1.0
            continue
        in_scope = sum(1 for ref in known_scope_refs if ref.scope_usr == symbol.scope_usr)
        scope_purity_by_usr[usr] = float(in_scope) / float(len(known_scope_refs))

    consensus_by_usr: dict[str, float] = {usr: 1.0 for usr in symbols_by_usr}
    return SemanticContext(
        symbols=tuple(symbols),
        references=tuple(references),
        consensus_by_usr=_float_pairs(consensus_by_usr),
        reference_count_by_usr=_int_pairs(reference_count_by_usr),
        scope_purity_by_usr=_float_pairs(scope_purity_by_usr),
    )


def _apply_parser_consensus(
    semantic: SemanticContext,
    text: str,
    tree_context: TreeContextData,
) -> tuple[SemanticContext, dict[str, float], dict[str, float]]:
    if not semantic.symbols:
        return semantic, {}, {}

    consensus_by_usr = _pairs_to_float_map(semantic.consensus_by_usr)
    reference_count_by_usr = _pairs_to_int_map(semantic.reference_count_by_usr)
    scope_purity_by_usr = _pairs_to_float_map(semantic.scope_purity_by_usr)
    reference_consensus_by_usr = dict(consensus_by_usr)
    declaration_consensus_by_usr = {usr: 1.0 for usr in consensus_by_usr}

    if tree_context.root_type is not None:
        spans = tree_context.identifier_spans
        declarations = tuple(tree_context.declarations)
        refs_by_usr: dict[str, list[SemanticReference]] = {}
        symbols_by_usr = {symbol.usr: symbol for symbol in semantic.symbols if symbol.usr}
        for ref in semantic.references:
            refs_by_usr.setdefault(ref.usr, []).append(ref)

        data = text.encode("utf-8", errors="ignore")
        symbol_usrs = set(symbols_by_usr)
        for usr in symbol_usrs:
            refs = refs_by_usr.get(usr, [])
            if refs:
                matches = 0
                total = 0
                for ref in refs:
                    total += 1
                    key = (ref.start, ref.end)
                    identifier = spans.get(key)
                    if identifier is None:
                        continue
                    text_slice = data[ref.start : ref.end].decode("utf-8", errors="ignore")
                    if text_slice == identifier:
                        matches += 1
                if total > 0:
                    reference_consensus_by_usr[usr] = float(matches) / float(total)

            symbol = symbols_by_usr.get(usr)
            if symbol is None:
                continue
            best = 0.0
            for candidate in declarations:
                if not _declaration_candidate_for_symbol(symbol, candidate):
                    continue
                best = max(best, _declaration_match_score(symbol, candidate))
                if best >= 0.98:
                    break
            declaration_consensus_by_usr[usr] = best

            ref_score = reference_consensus_by_usr.get(usr, 0.0)
            decl_score = declaration_consensus_by_usr.get(usr, 0.0)
            consensus_by_usr[usr] = _hybrid_consensus_score(ref_score, decl_score)

    symbols = tuple(
        replace(
            symbol,
            parser_consensus=float(consensus_by_usr.get(symbol.usr, symbol.parser_consensus)),
        )
        for symbol in semantic.symbols
    )
    return SemanticContext(
        symbols=symbols,
        references=semantic.references,
        consensus_by_usr=_float_pairs(consensus_by_usr),
        reference_count_by_usr=_int_pairs(reference_count_by_usr),
        scope_purity_by_usr=_float_pairs(scope_purity_by_usr),
    ), reference_consensus_by_usr, declaration_consensus_by_usr


def _build_semantic_indexes(
    semantic: SemanticContext,
) -> tuple[
    dict[str, tuple[SemanticReference, ...]],
    dict[str, int],
    tuple[SemanticSymbol, ...],
    tuple[str, ...],
]:
    refs_by_usr: dict[str, list[SemanticReference]] = {}
    non_decl_counts: dict[str, int] = {}
    function_symbols: list[SemanticSymbol] = []
    class_names: set[str] = set()
    function_kinds = {"function_decl", "cxx_method", "constructor", "destructor", "function_template"}

    for ref in semantic.references:
        refs_by_usr.setdefault(ref.usr, []).append(ref)
        if not ref.is_declaration:
            non_decl_counts[ref.usr] = non_decl_counts.get(ref.usr, 0) + 1

    for symbol in semantic.symbols:
        if symbol.kind in function_kinds and symbol.scope_kind == "function":
            function_symbols.append(symbol)
        if symbol.scope_kind in {"class", "struct"} and symbol.name:
            class_names.add(symbol.name)

    frozen_refs = {
        usr: tuple(refs)
        for usr, refs in refs_by_usr.items()
    }
    function_symbols.sort(key=lambda item: int(item.start))
    return (
        frozen_refs,
        non_decl_counts,
        tuple(function_symbols),
        tuple(sorted(class_names)),
    )


def _collect_tree_context(tree_sitter_tree: Any | None, text: str) -> TreeContextData:
    root = getattr(tree_sitter_tree, "root_node", None)
    if root is None:
        return TreeContextData(
            root_type=None,
            node_count=0,
            identifier_spans={},
            declarations=(),
            blocks=(),
        )

    data = text.encode("utf-8", errors="ignore")
    identifier_spans: dict[tuple[int, int], str] = {}
    declarations: list[TreeDeclaration] = []
    blocks: list[CodeBlock] = []
    seen_decl_spans: set[tuple[int, int]] = set()
    seen_block_spans: set[tuple[int, int]] = set()
    node_count = 0

    stack = [root]
    while stack:
        node = stack.pop()
        node_count += 1
        node_type = getattr(node, "type", "")

        if node_type in {"identifier", "field_identifier", "type_identifier"}:
            start = int(node.start_byte)
            end = int(node.end_byte)
            identifier_spans[(start, end)] = data[start:end].decode("utf-8", errors="ignore")

        declaration = _tree_declaration_for_node(node, data)
        if declaration is not None:
            span_key = (declaration.start, declaration.end)
            if span_key not in seen_decl_spans:
                seen_decl_spans.add(span_key)
                declarations.append(declaration)

        block = _tree_block_for_node(node, data)
        if block is not None:
            span_key = (block.start, block.end)
            if span_key not in seen_block_spans:
                seen_block_spans.add(span_key)
                blocks.append(block)

        stack.extend(reversed(getattr(node, "children", [])))

    return TreeContextData(
        root_type=getattr(root, "type", None),
        node_count=node_count,
        identifier_spans=identifier_spans,
        declarations=tuple(declarations),
        blocks=tuple(blocks),
    )


def _tree_declaration_for_node(node: Any, data: bytes) -> TreeDeclaration | None:
    node_type = getattr(node, "type", "")
    kind = ""
    name_node = None

    if node_type == "namespace_definition":
        kind = "namespace"
        name_node = _first_descendant_by_types(node, {"namespace_identifier", "identifier"})
    elif node_type in {"class_specifier", "struct_specifier"}:
        kind = "type"
        name_node = _first_descendant_by_types(node, {"type_identifier"})
    elif node_type in {"function_definition", "function_declaration"}:
        kind = "function"
        declarator = _first_descendant_by_types(node, {"function_declarator"})
        if declarator is not None:
            name_node = _first_descendant_by_types(
                declarator,
                {"identifier", "field_identifier", "type_identifier"},
                stop_types={"parameter_list", "template_parameter_list"},
            )
    elif node_type == "field_declaration":
        kind = "member"
        name_node = _first_descendant_by_types(node, {"field_identifier", "identifier"})
    elif node_type == "parameter_declaration":
        kind = "param"
        name_node = _first_descendant_by_types(node, {"identifier"})
    elif node_type in {"declaration", "init_declarator"}:
        kind = "variable"
        declarator = _first_descendant_by_types(
            node,
            {"init_declarator", "declarator", "pointer_declarator", "reference_declarator", "array_declarator"},
        )
        if declarator is not None:
            name_node = _first_descendant_by_types(declarator, {"identifier"})
        if name_node is None:
            name_node = _first_descendant_by_types(node, {"identifier"})

    if name_node is None:
        return None

    name = _node_text(name_node, data)
    if not name:
        return None
    start = int(name_node.start_byte)
    end = int(name_node.end_byte)
    line = int(name_node.start_point[0]) + 1
    column = int(name_node.start_point[1]) + 1
    scope_kind, scope_name = _tree_scope_for_node(node, data)
    if kind == "variable":
        if scope_kind in {"class", "struct"}:
            kind = "member"
        elif scope_kind == "function":
            kind = "local"
        else:
            kind = "global"
    return TreeDeclaration(
        name=name,
        kind=kind,
        scope_kind=scope_kind,
        scope_name=scope_name,
        start=start,
        end=end,
        line=line,
        column=column,
    )


def _tree_block_for_node(node: Any, data: bytes) -> CodeBlock | None:
    body = _node_body(node)
    if body is None:
        return None
    node_type = str(getattr(node, "type", "") or "")
    kind = _canonical_kind_from_node_type(node_type)
    if not kind:
        return None

    header = _node_header_text(node, body, data)
    if not header:
        header = kind
    kind = _refine_kind_from_tree(node, kind)
    short_label = f"{kind}(...)"
    if kind in {"namespace", "class", "struct"}:
        name = _extract_name(node, data, kind=kind)
        if name:
            short_label = f"{kind} {name}"
    elif kind == "function":
        name = _extract_name(node, data, kind=kind)
        if name:
            short_label = f"{name}(...)"
    return CodeBlock(
        kind=kind,
        label=header,
        short_label=short_label,
        start=int(node.start_byte),
        end=int(body.end_byte),
        open_line=int(node.start_point[0]) + 1,
        close_line=int(body.end_point[0]) + 1,
    )


def _node_body(node: Any) -> Any | None:
    for child in getattr(node, "children", []):
        child_type = getattr(child, "type", "")
        if child_type in {"compound_statement", "field_declaration_list", "declaration_list"}:
            return child
    return None


def _canonical_kind_from_node_type(node_type: str) -> str:
    raw = str(node_type or "").strip().lower()
    if not raw:
        return ""
    tokens = [token for token in raw.split("_") if token]
    if not tokens:
        return ""
    if len(tokens) == 1:
        return tokens[0]
    if tokens[1] in {"statement", "definition", "specifier", "clause"}:
        return tokens[0]
    if tokens[-1] in {"statement", "definition", "specifier", "clause"}:
        return tokens[0]
    return tokens[0]


def _node_header_text(node: Any, body: Any, data: bytes) -> str:
    start = int(getattr(node, "start_byte", 0))
    end = int(getattr(body, "start_byte", 0))
    if end <= start:
        return ""
    raw = data[start:end].decode("utf-8", errors="ignore")
    return _normalize_space(raw)


def _extract_name(node: Any, data: bytes, kind: str | None = None) -> str | None:
    node_type = str(getattr(node, "type", "") or "")
    stop_types = {"compound_statement", "field_declaration_list", "declaration_list"}
    effective_kind = str(kind or "").strip().lower()
    if node_type == "namespace_definition" or effective_kind == "namespace":
        name_node = _rightmost_descendant_by_types(
            node,
            {"namespace_identifier", "identifier"},
            stop_types=stop_types,
        )
        if name_node is not None:
            name = _node_text(name_node, data)
            if name:
                return name
    if node_type in {"class_specifier", "struct_specifier"} or effective_kind in {"class", "struct"}:
        name_node = _first_descendant_by_types(
            node,
            {"type_identifier"},
            stop_types=stop_types,
        )
        if name_node is not None:
            name = _node_text(name_node, data)
            if name:
                return name
    if node_type in {"function_definition", "function_declaration"} or effective_kind == "function":
        declarator = _first_descendant_by_types(node, {"function_declarator"})
        if declarator is not None:
            name_node = _rightmost_descendant_by_types(
                declarator,
                {"identifier", "field_identifier", "type_identifier", "destructor_name", "operator_name"},
                stop_types={"parameter_list", "template_parameter_list"},
            )
            if name_node is not None:
                name = _node_text(name_node, data)
                if name:
                    if "::" in name:
                        name = name.split("::")[-1]
                    return name

    name_node = _first_descendant_by_types(
        node,
        {"identifier", "type_identifier", "namespace_identifier", "field_identifier"},
        stop_types=stop_types,
    )
    if name_node is None:
        return None
    name = _node_text(name_node, data)
    return name or None


def _refine_kind_from_tree(node: Any, base_kind: str) -> str:
    kind = str(base_kind or "").strip().lower()
    if kind in {"class", "struct", "namespace", "function"}:
        return kind

    stop_types = {"compound_statement", "field_declaration_list", "declaration_list"}
    kind_checks: tuple[tuple[set[str], str], ...] = (
        ({"class_specifier"}, "class"),
        ({"struct_specifier"}, "struct"),
        ({"namespace_definition"}, "namespace"),
        ({"function_definition", "function_declaration", "function_declarator"}, "function"),
    )
    for types, resolved in kind_checks:
        if _first_descendant_by_types(node, types, stop_types=stop_types) is not None:
            return resolved
    return kind


def _tree_scope_for_node(node: Any, data: bytes) -> tuple[str, str | None]:
    current = getattr(node, "parent", None)
    while current is not None:
        current_type = getattr(current, "type", "")
        if current_type == "function_definition":
            declarator = _first_descendant_by_types(current, {"function_declarator"})
            if declarator is not None:
                name_node = _first_descendant_by_types(
                    declarator,
                    {"identifier", "field_identifier", "type_identifier"},
                    stop_types={"parameter_list", "template_parameter_list"},
                )
                if name_node is not None:
                    return "function", _node_text(name_node, data)
            return "function", None
        if current_type == "namespace_definition":
            name_node = _first_descendant_by_types(current, {"namespace_identifier", "identifier"})
            return "namespace", _node_text(name_node, data) if name_node is not None else None
        if current_type == "class_specifier":
            name_node = _first_descendant_by_types(current, {"type_identifier"})
            return "class", _node_text(name_node, data) if name_node is not None else None
        if current_type == "struct_specifier":
            name_node = _first_descendant_by_types(current, {"type_identifier"})
            return "struct", _node_text(name_node, data) if name_node is not None else None
        current = getattr(current, "parent", None)
    return "global", None


def _first_descendant_by_types(
    node: Any,
    target_types: set[str],
    stop_types: set[str] | None = None,
) -> Any | None:
    stop_types = stop_types or set()
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


def _rightmost_descendant_by_types(
    node: Any,
    target_types: set[str],
    stop_types: set[str] | None = None,
) -> Any | None:
    stop_types = stop_types or set()
    best: Any | None = None
    best_start = -1
    stack = [node]
    while stack:
        current = stack.pop()
        current_type = getattr(current, "type", "")
        if current is not node and current_type in stop_types:
            continue
        if current is not node and current_type in target_types:
            start = int(getattr(current, "start_byte", -1))
            if start >= best_start:
                best = current
                best_start = start
        stack.extend(getattr(current, "children", []))
    return best


def _node_text(node: Any, data: bytes) -> str:
    start = int(getattr(node, "start_byte", 0))
    end = int(getattr(node, "end_byte", 0))
    if end <= start:
        return ""
    return data[start:end].decode("utf-8", errors="ignore")


def _declaration_match_score(symbol: SemanticSymbol, declaration: TreeDeclaration) -> float:
    score = 0.0
    if _ranges_overlap(symbol.start, symbol.end, declaration.start, declaration.end):
        score += 0.60
    elif abs(int(symbol.start) - int(declaration.start)) <= 8:
        score += 0.35

    if _scope_kind_compatible(symbol.scope_kind, declaration.scope_kind):
        score += 0.25
    if _kind_compatible(symbol.kind, declaration.kind):
        score += 0.15
    if symbol.name == declaration.name:
        score += 0.10
    return min(1.0, score)


def _declaration_candidate_for_symbol(symbol: SemanticSymbol, declaration: TreeDeclaration) -> bool:
    if not _kind_compatible(symbol.kind, declaration.kind):
        return False
    if not _scope_kind_compatible(symbol.scope_kind, declaration.scope_kind):
        return False
    if _ranges_overlap(symbol.start, symbol.end, declaration.start, declaration.end):
        return True
    byte_distance = abs(int(symbol.start) - int(declaration.start))
    if byte_distance <= 768:
        return True
    line_distance = abs(int(symbol.line) - int(declaration.line))
    return line_distance <= 3


def _hybrid_consensus_score(reference_score: float, declaration_score: float) -> float:
    if reference_score <= 0.0 and declaration_score <= 0.0:
        return 0.0
    if reference_score <= 0.0:
        return float(max(0.0, min(1.0, declaration_score)))
    if declaration_score <= 0.0:
        return float(max(0.0, min(1.0, reference_score)))
    blended = (0.65 * float(reference_score)) + (0.35 * float(declaration_score))
    return float(max(0.0, min(1.0, blended)))


def _scope_kind_compatible(symbol_scope: str, tree_scope: str) -> bool:
    symbol_scope_norm = str(symbol_scope or "").strip().lower()
    tree_scope_norm = str(tree_scope or "").strip().lower()
    if symbol_scope_norm == tree_scope_norm:
        return True
    if symbol_scope_norm == "member" and tree_scope_norm in {"class", "struct", "member"}:
        return True
    if symbol_scope_norm in {"class", "struct"} and tree_scope_norm == "member":
        return True
    if symbol_scope_norm == "global" and tree_scope_norm == "namespace":
        return True
    if symbol_scope_norm == "param" and tree_scope_norm == "function":
        return True
    return False


def _kind_compatible(symbol_kind: str, tree_kind: str) -> bool:
    symbol_kind_norm = str(symbol_kind or "").strip().lower()
    tree_kind_norm = str(tree_kind or "").strip().lower()
    if symbol_kind_norm == tree_kind_norm:
        return True
    if symbol_kind_norm in {"function_decl", "cxx_method", "constructor", "destructor", "function_template"}:
        return tree_kind_norm == "function"
    if symbol_kind_norm in {"field_decl", "var_decl", "parm_decl"}:
        return tree_kind_norm in {"member", "param", "local", "global", "variable"}
    if symbol_kind_norm in {"class_decl", "struct_decl", "class_template"}:
        return tree_kind_norm == "type"
    if symbol_kind_norm == "namespace":
        return tree_kind_norm == "namespace"
    return False


def _ranges_overlap(start_a: int, end_a: int, start_b: int, end_b: int) -> bool:
    return int(start_a) < int(end_b) and int(start_b) < int(end_a)


def _merge_blocks_with_semantic(
    blocks: tuple[CodeBlock, ...],
    semantic: SemanticContext | None,
) -> tuple[CodeBlock, ...]:
    if not blocks:
        return blocks
    if semantic is None or not semantic.symbols:
        return blocks

    candidate_kinds = {
        "function": {"function_decl", "cxx_method", "constructor", "destructor", "function_template"},
        "class": {"class_decl", "class_template"},
        "struct": {"struct_decl"},
        "namespace": {"namespace"},
    }
    symbols = tuple(semantic.symbols)
    merged: list[CodeBlock] = []
    for block in blocks:
        kinds = candidate_kinds.get(block.kind)
        if not kinds:
            merged.append(block)
            continue
        overlap_found = False
        for symbol in symbols:
            if symbol.kind not in kinds:
                continue
            if _ranges_overlap(block.start, block.end, symbol.start, symbol.end):
                overlap_found = True
                break
            if abs(int(block.open_line) - int(symbol.line)) <= 1:
                overlap_found = True
                break
        if overlap_found:
            merged.append(
                replace(
                    block,
                    source="hybrid",
                    confidence=max(float(block.confidence), 0.95),
                )
            )
        else:
            merged.append(block)
    return tuple(merged)


def _is_atomic_type(type_spelling: str, token_set: set[str]) -> bool:
    if "atomic" in token_set or "_Atomic" in token_set:
        return True
    spelling = str(type_spelling or "")
    # Tokenize C/C++ type spelling without regex.
    for separator in ("::", "<", ">", ",", "*", "&", "(", ")", "[", "]"):
        spelling = spelling.replace(separator, " ")
    for token in spelling.split():
        if token == "_Atomic" or token == "atomic" or token.startswith("atomic_"):
            return True
    return False


def _normalize_space(text: str) -> str:
    return " ".join(str(text or "").strip().split())


def _pairs_to_float_map(pairs: tuple[tuple[str, float], ...]) -> dict[str, float]:
    values: dict[str, float] = {}
    for key, value in pairs:
        values[str(key)] = float(value)
    return values


def _pairs_to_int_map(pairs: tuple[tuple[str, int], ...]) -> dict[str, int]:
    values: dict[str, int] = {}
    for key, value in pairs:
        values[str(key)] = int(value)
    return values


def _float_pairs(values: dict[str, float]) -> tuple[tuple[str, float], ...]:
    return tuple(sorted((str(key), float(value)) for key, value in values.items()))


def _int_pairs(values: dict[str, int]) -> tuple[tuple[str, int], ...]:
    return tuple(sorted((str(key), int(value)) for key, value in values.items()))
