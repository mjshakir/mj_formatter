from __future__ import annotations

import pytest

from mj_formatter.core.parsing import ParserManager
from mj_formatter.core.types import ParseContext
from mj_formatter.policies.include_order_policy import IncludeOrderPolicy


def _include_order_config() -> dict[str, object]:
    return {
        "order_header": ["standard", "third_party", "project", "local"],
        "order_source": ["main", "standard", "third_party", "project", "local"],
        "standard_headers": [],
        "standard_prefixes": [],
        "project_headers": [],
        "project_prefixes": [],
        "main_header_extensions": [".hpp", ".h", ".hh", ".hxx"],
        "standard_header_path_markers": ["/include/c++/", "/c++/v1/", "/include/bits/"],
        "clang_builtin_include_prefix": "/lib/clang/",
        "include_path_segment": "/include/",
        "separator_length": 64,
        "group_titles": {
            "main": "Main header",
            "standard": "Standard Cpp Libraries",
            "third_party": "Third-party headers",
            "project": "Project headers",
            "local": "User Defined Headers",
        },
        "third_party_labels": {},
    }


def test_include_order_treats_cstdbool_as_standard_header() -> None:
    source_path = "/tmp/ThreadRegistry.hpp"
    text = (
        "#pragma once\n"
        "\n"
        "#include <cstdbool>\n"
        "#include \"MyLocal.hpp\"\n"
    )
    clang_ast = _FakeTranslationUnit(
        [
            _FakeInclusion(
                source=source_path,
                line=3,
                include_path="/usr/include/c++/13/cstdbool",
            )
        ]
    )
    manager = ParserManager()
    tree, _, warning = manager.parse_tree_sitter(text, source_path)
    if tree is None:
        pytest.skip(f"tree-sitter unavailable: {warning}")
    policy = IncludeOrderPolicy(_include_order_config())
    result = policy.apply(ParseContext(text=text, path=source_path, tree_sitter_tree=tree, clang_ast=clang_ast))
    assert "Third-party headers: cstdbool" not in result.text
    assert "// Standard Cpp Libraries" in result.text
    assert "#include <cstdbool>" in result.text


class _FakeFile:
    def __init__(self, name: str) -> None:
        self.name = name

    def __str__(self) -> str:
        return self.name


class _FakeLocation:
    def __init__(self, file_path: str, line: int) -> None:
        self.file = _FakeFile(file_path)
        self.line = line


class _FakeInclusion:
    def __init__(self, source: str, line: int, include_path: str) -> None:
        self.location = _FakeLocation(source, line)
        self.include = _FakeFile(include_path)


class _FakeTranslationUnit:
    def __init__(self, includes: list[_FakeInclusion]) -> None:
        self._includes = includes

    def get_includes(self) -> list[_FakeInclusion]:
        return self._includes


def test_include_order_uses_clang_context_for_std_headers_with_dot_h() -> None:
    source_path = "/tmp/sample.cpp"
    text = (
        "#include <stdio.h>\n"
        "#include \"MyLocal.hpp\"\n"
    )
    clang_ast = _FakeTranslationUnit(
        [
            _FakeInclusion(
                source=source_path,
                line=1,
                include_path="/usr/lib/llvm-19/lib/clang/19/include/stdio.h",
            )
        ]
    )
    manager = ParserManager()
    tree, _, warning = manager.parse_tree_sitter(text, source_path)
    if tree is None:
        pytest.skip(f"tree-sitter unavailable: {warning}")
    policy = IncludeOrderPolicy(_include_order_config())
    result = policy.apply(ParseContext(text=text, path=source_path, clang_ast=clang_ast, tree_sitter_tree=tree))

    assert "// Standard Cpp Libraries" in result.text
    assert "Third-party headers: stdio.h" not in result.text
