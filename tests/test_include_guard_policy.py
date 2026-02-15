from __future__ import annotations

import pytest

from mj_formatter.core.parsing import ParserManager
from mj_formatter.core.types import ParseContext
from mj_formatter.policies.include_guard_policy import IncludeGuardPolicy


def _policy_config(mode: str) -> dict[str, object]:
    return {"mode": mode, "header_extensions": [".h", ".hpp", ".hh", ".hxx"]}


def _parse_tree(text: str, path: str):
    manager = ParserManager()
    tree, _, warning = manager.parse_tree_sitter(text, path)
    if tree is None:
        pytest.skip(f"tree-sitter unavailable: {warning}")
    return tree


def test_include_guard_respects_existing_pragma_once() -> None:
    text = "#pragma once\n\nint value = 0;\n"
    tree = _parse_tree(text, "sample.hpp")
    policy = IncludeGuardPolicy(_policy_config("pragma_once"))
    result = policy.apply(ParseContext(text=text, path="sample.hpp", tree_sitter_tree=tree))
    assert result.text == text
    assert not result.edits


def test_include_guard_adds_pragma_once_when_missing() -> None:
    text = "int value = 0;\n"
    tree = _parse_tree(text, "sample.hpp")
    policy = IncludeGuardPolicy(_policy_config("pragma_once"))
    result = policy.apply(ParseContext(text=text, path="sample.hpp", tree_sitter_tree=tree))
    assert result.text.startswith("#pragma once\n")
    assert result.edits


def test_include_guard_detects_existing_ifndef_guard() -> None:
    text = (
        "#ifndef SAMPLE_HPP_\n"
        "#define SAMPLE_HPP_\n"
        "int value = 0;\n"
        "#endif\n"
    )
    tree = _parse_tree(text, "sample.hpp")
    policy = IncludeGuardPolicy(_policy_config("include_guard"))
    result = policy.apply(ParseContext(text=text, path="sample.hpp", tree_sitter_tree=tree))
    assert result.text == text
    assert not result.edits
