from __future__ import annotations

from typing import Any

from ..types import ParseBackend


class ParseControl:
    def backend_for_policy(self, policy: Any, context: Any) -> ParseBackend:
        match policy.parse_mode:
            case "tree_sitter":
                return ParseBackend.TREE if context.tree_sitter_tree is not None else ParseBackend.SKIPPED
            case "clang":
                if context.clang_ast is not None:
                    return ParseBackend.CLANG
                return ParseBackend.SKIPPED
            case _:
                return ParseBackend.SKIPPED
