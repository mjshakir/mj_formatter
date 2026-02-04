from __future__ import annotations

from pathlib import Path
from typing import Any


class ParserManager:
    def __init__(self) -> None:
        self._ts_available = False
        self._ts_error: str | None = None
        self._ts_parsers: dict[str, Any] = {}
        self._init_tree_sitter()

    def _init_tree_sitter(self) -> None:
        try:
            from tree_sitter import Parser  # noqa: F401
            from tree_sitter_languages import get_language  # noqa: F401
        except Exception as exc:  # pragma: no cover
            self._ts_available = False
            self._ts_error = str(exc)
            return
        self._ts_available = True

    def parse_tree_sitter(self, text: str, path: str) -> tuple[Any | None, str | None, str | None]:
        if not self._ts_available:
            return None, None, f"tree-sitter unavailable: {self._ts_error or 'missing dependency'}"

        language = self._guess_language(path)
        if not language:
            return None, None, "tree-sitter language not determined"

        try:
            parser = self._get_parser(language)
            tree = parser.parse(bytes(text, "utf-8"))
            return tree, language, None
        except Exception as exc:
            return None, language, f"tree-sitter parse failed: {exc}"

    def _get_parser(self, language: str):
        if language in self._ts_parsers:
            return self._ts_parsers[language]
        from tree_sitter import Parser
        from tree_sitter_languages import get_language

        parser = Parser()
        parser.set_language(get_language(language))
        self._ts_parsers[language] = parser
        return parser

    def _guess_language(self, path: str) -> str | None:
        ext = Path(path).suffix.lower()
        if ext in {".cpp", ".cc", ".cxx", ".hpp", ".hh", ".hxx", ".h"}:
            return "cpp"
        if ext in {".c"}:
            return "c"
        return None
