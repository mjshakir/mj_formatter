from __future__ import annotations

import pytest

from mj_formatter.core.engine.context import PostEditChecker
from mj_formatter.core.parsing import ParserManager
from mj_formatter.core.parsing.clang_args import ClangArgsResolver
from mj_formatter.core.types import AppConfig


def test_post_edit_checker_blocks_tree_regression() -> None:
    parser = ParserManager()
    tree, _, warning = parser.parse_tree_sitter("int value = 1;\n", "sample.cpp")
    if tree is None:
        pytest.skip(f"tree-sitter unavailable: {warning}")

    config = AppConfig(root=".")
    checker = PostEditChecker(parser_manager=parser, clang_args_resolver=ClangArgsResolver(config))

    result = checker.validate(
        path="sample.cpp",
        before_text="int value = 1;\n",
        after_text="int value = ;\n",
    )

    assert not result.accepted
    assert any("tree-sitter parse quality regressed" in item for item in result.messages)
