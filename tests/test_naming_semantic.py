from __future__ import annotations

from pathlib import Path
import pytest

from mj_formatter.core.parsing import CodeContext, SemanticContext, SemanticReference, SemanticSymbol, build_code_context
from mj_formatter.core.types import ParseContext
from mj_formatter.core.parsing import ParserManager
from mj_formatter.policies.naming_conventions_policy import NamingConventionsPolicy, _Decl


def _nth_span(text: str, needle: str, occurrence: int) -> tuple[int, int]:
    start = -1
    pos = 0
    for _ in range(occurrence + 1):
        start = text.index(needle, pos)
        pos = start + len(needle)
    return start, start + len(needle)


def _line_col(text: str, offset: int) -> tuple[int, int]:
    line = text.count("\n", 0, offset) + 1
    line_start = text.rfind("\n", 0, offset)
    if line_start == -1:
        return line, offset + 1
    return line, offset - line_start


def test_naming_semantic_renames_declaration_and_references() -> None:
    text = (
        "int calc(int value) {\n"
        "    int count = value + 1;\n"
        "    return count;\n"
        "}\n"
    )
    path = str(Path("sample.cpp").resolve())

    manager = ParserManager()
    tu, warning = manager.parse_clang(
        ParserManager.ClangParseArgs(
            text=text,
            path=path,
            args=("-x", "c++", "-std=c++20"),
            include_function_bodies=True,
        )
    )
    if warning is not None:
        pytest.skip(f"clang unavailable in test environment: {warning}")

    context = ParseContext(
        text=text,
        path=path,
        tree_sitter_tree=None,
        tree_sitter_lang=None,
        clang_ast=tu,
        warnings=[],
        code_context=build_code_context(path=path, text=text, clang_ast=tu, tree_sitter_tree=None),
    )
    policy = NamingConventionsPolicy(
        {
            "standard": "mj",
            "prefer_clang_semantic": True,
            "use_tree_sitter": False,
            "use_semantic_rename": True,
            "min_confidence": 0.5,
            "max_risk": "high",
        }
    )

    result = policy.apply(context)
    assert "int calc(int value)" in result.text
    assert "int _count = value + 1;" in result.text
    assert "return _count;" in result.text


def test_naming_semantic_risk_gate_skips_cross_file_symbols() -> None:
    text = (
        "int calc(int inputValue) {\n"
        "    return inputValue;\n"
        "}\n"
    )
    decl_start, decl_end = _nth_span(text, "inputValue", 0)
    ref_start, ref_end = _nth_span(text, "inputValue", 1)
    decl_line, decl_col = _line_col(text, decl_start)
    ref_line, ref_col = _line_col(text, ref_start)

    symbol = SemanticSymbol(
        usr="usr-value",
        name="inputValue",
        kind="parm_decl",
        scope_kind="param",
        scope_name="calc",
        line=decl_line,
        column=decl_col,
        start=decl_start,
        end=decl_end,
        is_static=False,
        is_const=False,
        is_constexpr=False,
        is_consteval=False,
        is_pointer=False,
        smart_ptr=None,
        is_std_function=False,
        is_template_type=False,
    )
    semantic = SemanticContext(
        symbols=(symbol,),
        references=(
            SemanticReference(
                usr="usr-value",
                start=decl_start,
                end=decl_end,
                line=decl_line,
                column=decl_col,
                is_declaration=True,
            ),
            SemanticReference(
                usr="usr-value",
                start=ref_start,
                end=ref_end,
                line=ref_line,
                column=ref_col,
                is_declaration=False,
            ),
        ),
    )
    code_context = CodeContext(
        clang_functions=(),
        clang_variables=(),
        semantic_context=semantic,
        semantic_file_counts={"usr-value": 2},
        tree_root_type=None,
        tree_node_count=0,
    )
    context = ParseContext(
        text=text,
        path="sample.cpp",
        tree_sitter_tree=None,
        tree_sitter_lang=None,
        clang_ast=None,
        warnings=[],
        code_context=code_context,
    )
    policy = NamingConventionsPolicy(
        {
            "standard": "mj",
            "prefer_clang_semantic": True,
            "use_tree_sitter": False,
            "use_semantic_rename": True,
            "min_confidence": 0.1,
            "max_risk": "low",
        }
    )

    result = policy.apply(context)
    assert result.text == text
    assert any("risk high exceeds max_risk low" in item.message for item in result.violations)


def test_naming_semantic_strict_parser_consensus_skips_low_consensus() -> None:
    text = (
        "int calc(int inputValue) {\n"
        "    return inputValue;\n"
        "}\n"
    )
    decl_start, decl_end = _nth_span(text, "inputValue", 0)
    ref_start, ref_end = _nth_span(text, "inputValue", 1)
    decl_line, decl_col = _line_col(text, decl_start)
    ref_line, ref_col = _line_col(text, ref_start)
    symbol = SemanticSymbol(
        usr="usr-value",
        name="inputValue",
        kind="parm_decl",
        scope_kind="param",
        scope_name="calc",
        line=decl_line,
        column=decl_col,
        start=decl_start,
        end=decl_end,
        is_static=False,
        is_const=False,
        is_constexpr=False,
        is_consteval=False,
        is_pointer=False,
        smart_ptr=None,
        is_std_function=False,
        is_template_type=False,
        scope_usr="usr-calc",
    )
    semantic = SemanticContext(
        symbols=(symbol,),
        references=(
            SemanticReference(
                usr="usr-value",
                start=decl_start,
                end=decl_end,
                line=decl_line,
                column=decl_col,
                is_declaration=True,
                scope_usr="usr-calc",
            ),
            SemanticReference(
                usr="usr-value",
                start=ref_start,
                end=ref_end,
                line=ref_line,
                column=ref_col,
                is_declaration=False,
                scope_usr="usr-calc",
            ),
        ),
    )
    code_context = CodeContext(
        clang_functions=(),
        clang_variables=(),
        semantic_context=semantic,
        semantic_file_counts={"usr-value": 1},
        tree_root_type=None,
        tree_node_count=0,
        semantic_consensus_scores={"usr-value": 0.25},
        semantic_reference_counts={"usr-value": 1},
        semantic_scope_purity={"usr-value": 1.0},
        semantic_project_reference_counts={"usr-value": 1},
        semantic_project_consensus_scores={"usr-value": 0.25},
    )
    context = ParseContext(
        text=text,
        path="sample.cpp",
        tree_sitter_tree=None,
        tree_sitter_lang=None,
        clang_ast=None,
        warnings=[],
        code_context=code_context,
    )
    policy = NamingConventionsPolicy(
        {
            "standard": "mj",
            "prefer_clang_semantic": True,
            "use_tree_sitter": False,
            "use_semantic_rename": True,
            "parser_consensus_mode": "strict",
            "parser_consensus_min": 0.80,
            "min_confidence": 0.1,
            "max_risk": "high",
        }
    )

    result = policy.apply(context)
    assert result.text == text
    assert any("parser consensus 0.25 below strict threshold 0.80" in item.message for item in result.violations)


def test_naming_semantic_local_scope_purity_gate() -> None:
    text = (
        "int func_a(int inputValue) {\n"
        "    return inputValue;\n"
        "}\n"
    )
    decl_start, decl_end = _nth_span(text, "inputValue", 0)
    ref_start, ref_end = _nth_span(text, "inputValue", 1)
    decl_line, decl_col = _line_col(text, decl_start)
    ref_line, ref_col = _line_col(text, ref_start)
    symbol = SemanticSymbol(
        usr="usr-value",
        name="inputValue",
        kind="parm_decl",
        scope_kind="param",
        scope_name="func_a",
        line=decl_line,
        column=decl_col,
        start=decl_start,
        end=decl_end,
        is_static=False,
        is_const=False,
        is_constexpr=False,
        is_consteval=False,
        is_pointer=False,
        smart_ptr=None,
        is_std_function=False,
        is_template_type=False,
        scope_usr="usr-func-a",
    )
    semantic = SemanticContext(
        symbols=(symbol,),
        references=(
            SemanticReference(
                usr="usr-value",
                start=decl_start,
                end=decl_end,
                line=decl_line,
                column=decl_col,
                is_declaration=True,
                scope_usr="usr-func-a",
            ),
            SemanticReference(
                usr="usr-value",
                start=ref_start,
                end=ref_end,
                line=ref_line,
                column=ref_col,
                is_declaration=False,
                scope_usr="usr-func-b",
            ),
        ),
    )
    code_context = CodeContext(
        clang_functions=(),
        clang_variables=(),
        semantic_context=semantic,
        semantic_file_counts={"usr-value": 1},
        tree_root_type=None,
        tree_node_count=0,
        semantic_consensus_scores={"usr-value": 1.0},
        semantic_reference_counts={"usr-value": 1},
        semantic_scope_purity={"usr-value": 0.5},
        semantic_project_reference_counts={"usr-value": 1},
        semantic_project_consensus_scores={"usr-value": 1.0},
    )
    context = ParseContext(
        text=text,
        path="sample.cpp",
        tree_sitter_tree=None,
        tree_sitter_lang=None,
        clang_ast=None,
        warnings=[],
        code_context=code_context,
    )
    policy = NamingConventionsPolicy(
        {
            "standard": "mj",
            "prefer_clang_semantic": True,
            "use_tree_sitter": False,
            "use_semantic_rename": True,
            "strict_local_scope": True,
            "min_confidence": 0.1,
            "max_risk": "high",
        }
    )

    result = policy.apply(context)
    assert result.text == text
    assert any("local symbol escapes function scope" in item.message for item in result.violations)


def test_naming_semantic_consensus_mode_off_allows_low_consensus() -> None:
    text = (
        "int calc(int inputValue) {\n"
        "    return inputValue;\n"
        "}\n"
    )
    decl_start, decl_end = _nth_span(text, "inputValue", 0)
    ref_start, ref_end = _nth_span(text, "inputValue", 1)
    decl_line, decl_col = _line_col(text, decl_start)
    ref_line, ref_col = _line_col(text, ref_start)
    symbol = SemanticSymbol(
        usr="usr-value",
        name="inputValue",
        kind="parm_decl",
        scope_kind="param",
        scope_name="calc",
        line=decl_line,
        column=decl_col,
        start=decl_start,
        end=decl_end,
        is_static=False,
        is_const=False,
        is_constexpr=False,
        is_consteval=False,
        is_pointer=False,
        smart_ptr=None,
        is_std_function=False,
        is_template_type=False,
        scope_usr="usr-calc",
    )
    semantic = SemanticContext(
        symbols=(symbol,),
        references=(
            SemanticReference(
                usr="usr-value",
                start=decl_start,
                end=decl_end,
                line=decl_line,
                column=decl_col,
                is_declaration=True,
                scope_usr="usr-calc",
            ),
            SemanticReference(
                usr="usr-value",
                start=ref_start,
                end=ref_end,
                line=ref_line,
                column=ref_col,
                is_declaration=False,
                scope_usr="usr-calc",
            ),
        ),
    )
    code_context = CodeContext(
        clang_functions=(),
        clang_variables=(),
        semantic_context=semantic,
        semantic_file_counts={"usr-value": 1},
        tree_root_type=None,
        tree_node_count=0,
        semantic_consensus_scores={"usr-value": 0.01},
        semantic_reference_counts={"usr-value": 1},
        semantic_scope_purity={"usr-value": 1.0},
        semantic_project_reference_counts={"usr-value": 1},
        semantic_project_consensus_scores={"usr-value": 0.01},
    )
    context = ParseContext(
        text=text,
        path="sample.cpp",
        tree_sitter_tree=None,
        tree_sitter_lang=None,
        clang_ast=None,
        warnings=[],
        code_context=code_context,
    )
    policy = NamingConventionsPolicy(
        {
            "standard": "mj",
            "prefer_clang_semantic": True,
            "use_tree_sitter": False,
            "use_semantic_rename": True,
            "parser_consensus_mode": "off",
            "parser_consensus_min": 0.99,
            "min_confidence": 0.1,
            "max_risk": "high",
        }
    )

    result = policy.apply(context)
    assert "int calc(int input_value)" in result.text
    assert "return input_value;" in result.text


def test_naming_standards_can_be_overridden_from_config() -> None:
    policy = NamingConventionsPolicy(
        {
            "standard": "custom",
            "standards": {
                "custom": {
                    "local_prefix": "loc_",
                    "member_prefix": "mem_",
                    "global_prefix": "glob_",
                }
            },
        }
    )
    local = policy._name_variable(
        _Decl(
            name="value",
            kind="local",
        )
    )
    member = policy._name_variable(
        _Decl(
            name="value",
            kind="member",
        )
    )
    global_name = policy._name_variable(
        _Decl(
            name="value",
            kind="global",
        )
    )
    assert local == "loc_value"
    assert member == "mem_value"
    assert global_name == "glob_value"


def test_parameter_naming_omits_local_prefix_but_keeps_pointer_and_function_prefixes() -> None:
    policy = NamingConventionsPolicy({"standard": "mj"})
    plain_param = policy._name_variable(
        _Decl(
            name="_value",
            kind="param",
        )
    )
    pointer_param = policy._name_variable(
        _Decl(
            name="value",
            kind="param",
            is_pointer=True,
        )
    )
    function_param = policy._name_variable(
        _Decl(
            name="handler",
            kind="param",
            is_std_function=True,
        )
    )

    assert plain_param == "value"
    assert pointer_param == "p_value"
    assert function_param == "f_handler"


def test_atomic_prefix_is_applied_for_param_member_and_local() -> None:
    policy = NamingConventionsPolicy({"standard": "mj"})
    param_name = policy._name_variable(
        _Decl(
            name="value",
            kind="param",
            is_atomic=True,
        )
    )
    member_name = policy._name_variable(
        _Decl(
            name="value",
            kind="member",
            is_atomic=True,
        )
    )
    local_name = policy._name_variable(
        _Decl(
            name="value",
            kind="local",
            is_atomic=True,
        )
    )

    assert param_name == "a_value"
    assert member_name == "m_a_value"
    assert local_name == "_a_value"


def test_constructor_name_normalization_uses_scope_name() -> None:
    policy = NamingConventionsPolicy({"standard": "mj"})
    target = policy._target_name(
        _Decl(
            name="atomic_unique_ptr",
            kind="function",
            scope_name="AtomicUniquePtr",
        ),
        {"AtomicUniquePtr"},
    )
    assert target == "AtomicUniquePtr"


def test_semantic_rename_skips_when_reference_map_is_incomplete() -> None:
    text = (
        "int GlobalValue = 1;\n"
        "int x = GlobalValue;\n"
    )
    symbol = SemanticSymbol(
        usr="usr-global",
        name="GlobalValue",
        kind="var_decl",
        scope_kind="global",
        scope_name=None,
        line=1,
        column=5,
        start=4,
        end=15,
        is_static=False,
        is_const=False,
        is_constexpr=False,
        is_consteval=False,
        is_pointer=False,
        smart_ptr=None,
        is_std_function=False,
        is_template_type=False,
    )
    semantic = SemanticContext(
        symbols=(symbol,),
        references=(
            SemanticReference(
                usr="usr-global",
                start=4,
                end=15,
                line=1,
                column=5,
                is_declaration=True,
            ),
        ),
    )
    context = ParseContext(
        text=text,
        path="sample.cpp",
        tree_sitter_tree=None,
        tree_sitter_lang=None,
        clang_ast=None,
        warnings=[],
        code_context=CodeContext(
            clang_functions=(),
            clang_variables=(),
            semantic_context=semantic,
            semantic_file_counts={"usr-global": 1},
            tree_root_type=None,
            tree_node_count=0,
            semantic_consensus_scores={"usr-global": 1.0},
            semantic_reference_counts={"usr-global": 0},
            semantic_scope_purity={"usr-global": 1.0},
            semantic_project_reference_counts={"usr-global": 0},
            semantic_project_consensus_scores={"usr-global": 1.0},
        ),
    )
    policy = NamingConventionsPolicy(
        {
            "standard": "mj",
            "prefer_clang_semantic": True,
            "use_tree_sitter": False,
            "use_semantic_rename": True,
            "min_confidence": 0.1,
            "max_risk": "high",
        }
    )

    result = policy.apply(context)
    assert result.text == text
    assert any(
        ("incomplete reference map" in item.message)
        or ("no non-declaration reference evidence" in item.message)
        for item in result.violations
    )


def test_naming_tree_fallback_skips_variable_renames_without_semantic_context() -> None:
    text = (
        "int f(int plane) {\n"
        "    return plane;\n"
        "}\n"
        "\n"
        "int g() {\n"
        "    int plane = 1;\n"
        "    return plane;\n"
        "}\n"
    )
    manager = ParserManager()
    tree, lang, warning = manager.parse_tree_sitter(text, str(Path("sample.cpp").resolve()))
    if warning is not None or tree is None or lang != "cpp":
        pytest.skip(f"tree-sitter unavailable in test environment: {warning}")

    context = ParseContext(
        text=text,
        path="sample.cpp",
        tree_sitter_tree=tree,
        tree_sitter_lang="cpp",
        clang_ast=None,
        warnings=[],
        code_context=None,
    )
    policy = NamingConventionsPolicy(
        {
            "standard": "mj",
            "prefer_clang_semantic": False,
            "use_tree_sitter": True,
            "use_semantic_rename": True,
            "min_confidence": 0.1,
            "max_risk": "high",
        }
    )

    result = policy.apply(context)
    assert result.text == text


def test_semantic_member_rename_requires_reference_evidence() -> None:
    text = "class Widget { int value; };"
    symbol = SemanticSymbol(
        usr="usr-member",
        name="value",
        kind="field_decl",
        scope_kind="member",
        scope_name="Widget",
        line=1,
        column=20,
        start=19,
        end=24,
        is_static=False,
        is_const=False,
        is_constexpr=False,
        is_consteval=False,
        is_pointer=False,
        smart_ptr=None,
        is_std_function=False,
        is_template_type=False,
    )
    semantic = SemanticContext(
        symbols=(symbol,),
        references=(
            SemanticReference(
                usr="usr-member",
                start=19,
                end=24,
                line=1,
                column=20,
                is_declaration=True,
            ),
        ),
    )
    context = ParseContext(
        text=text,
        path="sample.hpp",
        tree_sitter_tree=None,
        tree_sitter_lang=None,
        clang_ast=None,
        warnings=[],
        code_context=CodeContext(
            clang_functions=(),
            clang_variables=(),
            semantic_context=semantic,
            semantic_file_counts={"usr-member": 1},
            tree_root_type=None,
            tree_node_count=0,
            semantic_consensus_scores={"usr-member": 1.0},
            semantic_reference_counts={"usr-member": 0},
            semantic_scope_purity={"usr-member": 1.0},
            semantic_project_reference_counts={"usr-member": 0},
            semantic_project_consensus_scores={"usr-member": 1.0},
        ),
    )
    policy = NamingConventionsPolicy(
        {
            "standard": "mj",
            "prefer_clang_semantic": True,
            "use_tree_sitter": False,
            "use_semantic_rename": True,
            "min_confidence": 0.1,
            "max_risk": "high",
        }
    )

    result = policy.apply(context)
    assert result.text == text
    assert any("no non-declaration reference evidence" in item.message for item in result.violations)
