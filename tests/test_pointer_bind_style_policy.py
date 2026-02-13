from __future__ import annotations

import pytest

from mj_formatter.core.parsing import ParserManager
from mj_formatter.core.types import ParseContext
from mj_formatter.policies.pointer_bind_style_policy import PointerBindStylePolicy


def test_pointer_bind_style_uses_tree_parser_for_spacing() -> None:
    text = "int * value = nullptr;\n"
    manager = ParserManager()
    tree, _, warning = manager.parse_tree_sitter(text, "sample.cpp")
    if tree is None:
        pytest.skip(f"tree-sitter unavailable: {warning}")

    policy = PointerBindStylePolicy({"style": "bind_to_type"})
    result = policy.apply(ParseContext(text=text, path="sample.cpp", tree_sitter_tree=tree))

    assert result.text == "int* value = nullptr;\n"
    assert result.edits
    assert result.violations


def test_pointer_bind_style_supports_bind_to_name() -> None:
    text = "int* value = nullptr;\n"
    manager = ParserManager()
    tree, _, warning = manager.parse_tree_sitter(text, "sample.cpp")
    if tree is None:
        pytest.skip(f"tree-sitter unavailable: {warning}")

    policy = PointerBindStylePolicy({"style": "bind_to_name"})
    result = policy.apply(ParseContext(text=text, path="sample.cpp", tree_sitter_tree=tree))

    assert result.text == "int *value = nullptr;\n"
