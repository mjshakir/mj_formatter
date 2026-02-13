from __future__ import annotations

from pathlib import Path

import pytest

from mj_formatter.core.types import ParseContext
from mj_formatter.core.parsing import ParserManager
from mj_formatter.policies.namespace_end_comments_policy import NamespaceEndCommentsPolicy


def _parse_tree_or_skip(text: str) -> object:
    manager = ParserManager()
    tree, lang, warning = manager.parse_tree_sitter(text, str(Path("sample.cpp").resolve()))
    if warning is not None or tree is None or lang != "cpp":
        pytest.skip(f"tree-sitter unavailable in test environment: {warning}")
    return tree


def test_namespace_end_comments_adds_end_labels() -> None:
    text = (
        "namespace MyNs {\n"
        "int foo(int value) {\n"
        "    if (!value) {\n"
        "        while (value < 3) {\n"
        "            ++value;\n"
        "        }\n"
        "    }\n"
        "    return value;\n"
        "}\n"
        "}\n"
    )
    tree = _parse_tree_or_skip(text)
    policy = NamespaceEndCommentsPolicy(
        {
            "blocks": ["namespace", "if", "while", "function"],
            "max_named_lines": 100,
            "max_label_length": 200,
            "replace_existing": True,
        }
    )
    context = ParseContext(
        text=text,
        path="sample.cpp",
        tree_sitter_tree=tree,
        tree_sitter_lang="cpp",
        clang_ast=None,
        warnings=[],
    )

    result = policy.apply(context)
    assert "} // end while (value < 3)" in result.text
    assert "} // end if (!value)" in result.text
    assert "} // end int foo(int value)" in result.text
    assert "} // end namespace MyNs" in result.text


def test_namespace_end_comments_shortens_long_labels() -> None:
    text = (
        "int foo(int value) {\n"
        "    while (value < 3) {\n"
        "        ++value;\n"
        "    }\n"
        "    return value;\n"
        "}\n"
    )
    tree = _parse_tree_or_skip(text)
    policy = NamespaceEndCommentsPolicy(
        {
            "blocks": ["while", "function"],
            "max_named_lines": 100,
            "max_label_length": 12,
            "replace_existing": True,
        }
    )
    context = ParseContext(
        text=text,
        path="sample.cpp",
        tree_sitter_tree=tree,
        tree_sitter_lang="cpp",
        clang_ast=None,
        warnings=[],
    )

    result = policy.apply(context)
    assert "} // end while(...)" in result.text
    assert "} // end foo(...)" in result.text


def test_namespace_end_comments_replaces_incorrect_existing_comment() -> None:
    text = (
        "namespace MyNs {\n"
        "int value = 0;\n"
        "} // namespace MyNs\n"
    )
    tree = _parse_tree_or_skip(text)
    policy = NamespaceEndCommentsPolicy(
        {
            "blocks": ["namespace"],
            "max_named_lines": 100,
            "max_label_length": 200,
            "replace_existing": True,
        }
    )
    context = ParseContext(
        text=text,
        path="sample.cpp",
        tree_sitter_tree=tree,
        tree_sitter_lang="cpp",
        clang_ast=None,
        warnings=[],
    )

    result = policy.apply(context)
    assert "} // end namespace MyNs" in result.text
    assert "// namespace MyNs" not in result.text


def test_namespace_end_comments_class_label_uses_type_name_not_macro() -> None:
    text = (
        "class HAZARDSYSTEM_API BitmapTree {\n"
        "public:\n"
        "    BitmapTree() = default;\n"
        "};\n"
    )
    tree = _parse_tree_or_skip(text)
    policy = NamespaceEndCommentsPolicy(
        {
            "blocks": ["class"],
            "max_named_lines": 100,
            "max_label_length": 200,
            "replace_existing": True,
        }
    )
    context = ParseContext(
        text=text,
        path="sample.hpp",
        tree_sitter_tree=tree,
        tree_sitter_lang="cpp",
        clang_ast=None,
        warnings=[],
    )
    result = policy.apply(context)
    assert "// end class BitmapTree" in result.text
    assert "// end class HAZARDSYSTEM_API" not in result.text
