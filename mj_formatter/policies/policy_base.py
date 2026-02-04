from __future__ import annotations

from abc import ABC, abstractmethod

from ..core.policy_result import PolicyResult


class Policy(ABC):
    name = ""
    description = ""
    default_enabled = True
    parse_mode = "text"  # text | tree_sitter | clang

    def __init__(self, config: dict[str, object]) -> None:
        self._config = config

    @abstractmethod
    def apply(self, context: "ParseContext") -> PolicyResult:
        raise NotImplementedError

    def _detect_line_ending(self, text: str) -> str:
        if "\r\n" in text:
            return "\r\n"
        return "\n"
