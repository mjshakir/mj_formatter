from __future__ import annotations

from enum import Enum
from typing import Any

from ..policies.lua_policy import LuaPolicy


class ParseBackend(str, Enum):
    TEXT = "text"
    REGEX = "regex"
    LUA = "lua"
    TREE = "tree"
    CLANG = "clang"


class ParseControl:
    def backend_for_policy(self, policy: Any, context: Any) -> ParseBackend:
        match policy.parse_mode:
            case _ if isinstance(policy, LuaPolicy):
                return ParseBackend.LUA
            case "tree_sitter":
                return ParseBackend.TREE if context.tree_sitter_tree is not None else ParseBackend.REGEX
            case "clang":
                return ParseBackend.CLANG if context.clang_ast is not None else ParseBackend.REGEX
            case _:
                return ParseBackend.TEXT
