from __future__ import annotations

from pathlib import Path

import pytest

from mj_formatter.core.config import EditorConfigResolver
from mj_formatter.core.types import ParseContext
from mj_formatter.core.parsing import ParserManager
from mj_formatter.policies.line_wrap_policy import LineWrapPolicy


def test_editorconfig_resolver_applies_ordered_sections(tmp_path: Path) -> None:
    root = tmp_path / "repo"
    root.mkdir()
    (root / ".editorconfig").write_text(
        "\n".join(
            [
                "root = true",
                "",
                "[*]",
                "indent_style = space",
                "indent_size = 4",
                "max_line_length = 120",
                "",
                "[*.hpp]",
                "max_line_length = 96",
                "",
                "[Makefile]",
                "indent_style = tab",
                "",
            ]
        ),
        encoding="utf-8",
    )

    include_dir = root / "include"
    include_dir.mkdir()
    header = include_dir / "X.hpp"
    header.write_text("int x;\n", encoding="utf-8")
    makefile = root / "Makefile"
    makefile.write_text("all:\n\t@true\n", encoding="utf-8")

    resolver = EditorConfigResolver.discover(root)
    assert resolver is not None
    hpp_props = resolver.resolve(header)
    mk_props = resolver.resolve(makefile)

    assert hpp_props.get("indent_style") == "space"
    assert hpp_props.get("indent_size") == "4"
    assert hpp_props.get("max_line_length") == "96"
    assert mk_props.get("indent_style") == "tab"


def test_line_wrap_parser_aware_call_wrapping() -> None:
    text = (
        "if (m_bitmask.compare_exchange_weak(mask, desired, std::memory_order_acq_rel, std::memory_order_relaxed)) {\n"
        "    return true;\n"
        "}\n"
    )
    manager = ParserManager()
    tree, _, warning = manager.parse_tree_sitter(text, "sample.cpp")
    if tree is None:
        pytest.skip(f"tree-sitter unavailable in test environment: {warning}")

    context = ParseContext(
        text=text,
        path="sample.cpp",
        tree_sitter_tree=tree,
        tree_sitter_lang="cpp",
        clang_ast=None,
        warnings=[],
        editorconfig={"max_line_length": "80"},
    )
    policy = LineWrapPolicy(
        {
            "max_length": 120,
            "wrap_style": "bin_pack",
            "allow_inline_prefix_args": True,
            "align_to_open_paren": True,
            "continuation_indent": 4,
            "use_editorconfig": True,
        }
    )

    result = policy.apply(context)
    assert result.text != text
    assert "compare_exchange_weak(" in result.text
    assert "\n" in result.text
    assert any("Wrapped long call/declaration line" in item.message for item in result.violations)
