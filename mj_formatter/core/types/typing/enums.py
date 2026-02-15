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
    HYBRID = "hybrid"

    @classmethod
    def from_value(cls, value: object) -> "ParserStrategy":
        # Hybrid-only runtime architecture.
        _ = value
        return cls.HYBRID


class PolicyEnforcement(str, Enum):
    MUST = "must"
    HARD = "hard"
    SOFT = "soft"
    ADVISORY = "advisory"

    @classmethod
    def from_value(cls, value: object) -> "PolicyEnforcement":
        if isinstance(value, cls):
            return value
        raw = str(value or cls.HARD.value).strip().lower()
        aliases = {
            "required": cls.MUST,
            "force": cls.MUST,
            "must": cls.MUST,
            "strict": cls.HARD,
            "hard": cls.HARD,
            "normal": cls.SOFT,
            "default": cls.SOFT,
            "standard": cls.SOFT,
            "relaxed": cls.ADVISORY,
        }
        if raw in aliases:
            return aliases[raw]
        for item in cls:
            if item.value == raw:
                return item
        return cls.HARD


class PolicyDecisionOutcome(str, Enum):
    APPLY = "apply"
    APPLY_PARTIAL = "apply_partial"
    ADVISORY_ONLY = "advisory_only"
    BLOCK = "block"


class TouchContract(str, Enum):
    ANY = "any"
    CODE_ONLY = "code_only"
    PREPROCESSOR_ONLY = "preprocessor_only"
    WHITESPACE_ONLY = "whitespace_only"

    @classmethod
    def from_value(cls, raw: object) -> "TouchContract":
        value = str(raw or cls.ANY.value).strip().lower()
        for item in cls:
            if item.value == value:
                return item
        return cls.ANY


class ParserConsensusMode(str, Enum):
    OFF = "off"
    ADVISORY = "advisory"
    STRICT = "strict"

    @classmethod
    def from_config(cls, value: object) -> "ParserConsensusMode":
        if isinstance(value, ParserConsensusMode):
            return value
        raw = str(value or cls.ADVISORY.value).strip().lower()
        for item in cls:
            if item.value == raw:
                return item
        return cls.ADVISORY


__all__ = [
    "ClangArgsMode",
    "ParseBackend",
    "ParserStrategy",
    "PolicyDecisionOutcome",
    "PolicyEnforcement",
    "TouchContract",
    "ParserConsensusMode",
]
