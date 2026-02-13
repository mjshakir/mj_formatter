from __future__ import annotations

import pytest

from mj_formatter.core.engine.context import EditGuard, TouchContract
from mj_formatter.core.parsing import ParseControl, ParserManager
from mj_formatter.core.types import Edit, ParseBackend, ParseContext


class _Policy:
    def __init__(self, parse_mode: str) -> None:
        self.parse_mode = parse_mode


def test_parse_control_disables_text_fallback_for_parser_policies() -> None:
    control = ParseControl()
    context = ParseContext(text="", path="x.cpp", tree_sitter_tree=object(), clang_ast=None)
    assert control.backend_for_policy(_Policy("clang"), context) == ParseBackend.SKIPPED

    context = ParseContext(text="", path="x.cpp", tree_sitter_tree=None, clang_ast=None)
    assert control.backend_for_policy(_Policy("tree_sitter"), context) == ParseBackend.SKIPPED
    assert control.backend_for_policy(_Policy("text"), context) == ParseBackend.SKIPPED


def test_edit_guard_blocks_code_only_on_comment_line() -> None:
    manager = ParserManager()
    text = "int value = 0;\n// comment\nvalue += 1;\n"
    tree, _, warning = manager.parse_tree_sitter(text, "sample.cpp")
    if tree is None:
        pytest.skip(f"tree-sitter unavailable: {warning}")

    guard = EditGuard()
    context = ParseContext(text=text, path="sample.cpp", tree_sitter_tree=tree)

    blocked = guard.validate(
        policy_name="naming_conventions",
        contract=TouchContract.CODE_ONLY,
        edits=[Edit(policy="naming_conventions", line=2, before="// comment", after="// changed")],
        parse_context=context,
    )
    assert blocked
    assert blocked[0].policy == "edit_guard"

    allowed = guard.validate(
        policy_name="naming_conventions",
        contract=TouchContract.CODE_ONLY,
        edits=[Edit(policy="naming_conventions", line=1, before="int value = 0;", after="int _value = 0;")],
        parse_context=context,
    )
    assert not allowed
