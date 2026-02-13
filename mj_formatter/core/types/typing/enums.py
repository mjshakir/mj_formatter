from __future__ import annotations

from enum import Enum


class ClangArgsMode(str, Enum):
    MERGE = "merge"
    COMPDB_ONLY = "compdb_only"
    ARGS_ONLY = "args_only"
    COMPDB_THEN_ARGS = "compdb_then_args"


class ParseBackend(str, Enum):
    TREE = "tree"
    CLANG = "clang"
    SKIPPED = "skipped"


class ParserStrategy(str, Enum):
    POLICY = "policy"
    HYBRID = "hybrid"
    TREE_ONLY = "tree_only"
    CLANG_ONLY = "clang_only"

    @classmethod
    def from_value(cls, value: object) -> "ParserStrategy":
        if isinstance(value, cls):
            return value
        raw = str(value or cls.HYBRID.value).strip().lower()
        for item in cls:
            if item.value == raw:
                return item
        return cls.HYBRID


__all__ = ["ClangArgsMode", "ParseBackend", "ParserStrategy"]
