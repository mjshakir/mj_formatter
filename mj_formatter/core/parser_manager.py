from __future__ import annotations

from pathlib import Path
from typing import Any
import warnings
from dataclasses import dataclass
import os


class ParserManager:
    def __init__(self) -> None:
        self._ts_available = False
        self._ts_error: str | None = None
        self._ts_parsers: dict[str, Any] = {}
        self._clang_available = False
        self._clang_error: str | None = None
        self._clang_index: Any | None = None
        self._init_tree_sitter()
        self._init_clang()

    def _init_tree_sitter(self) -> None:
        try:
            warnings.filterwarnings(
                "ignore",
                message="Language\\(path, name\\) is deprecated",
                category=FutureWarning,
                module="tree_sitter",
            )
            from tree_sitter import Parser  # noqa: F401
            from tree_sitter_languages import get_language  # noqa: F401
        except Exception as exc:  # pragma: no cover
            self._ts_available = False
            self._ts_error = str(exc)
            return
        self._ts_available = True

    def _init_clang(self) -> None:
        try:
            from clang import cindex

            libclang = self._find_libclang()
            if libclang:
                cindex.Config.set_library_file(libclang)
            self._clang_index = cindex.Index.create()
            self._clang_available = True
        except Exception as exc:  # pragma: no cover
            self._clang_available = False
            self._clang_error = str(exc)

    def _find_libclang(self) -> str | None:
        candidates: list[Path] = []

        # Prefer project-local conda env if present
        project_conda = Path.cwd() / "conda"
        for env_path in project_conda.glob("*/lib/libclang*.so*"):
            candidates.append(env_path)

        # Fallback to system paths (apt-installed)
        system_paths = [
            Path("/usr/lib/llvm-18/lib"),
            Path("/usr/lib/llvm-17/lib"),
            Path("/usr/lib/llvm-16/lib"),
            Path("/usr/lib/llvm-15/lib"),
            Path("/usr/lib/x86_64-linux-gnu"),
            Path("/usr/lib"),
            Path("/usr/local/lib"),
        ]
        for base in system_paths:
            for env_path in base.glob("libclang*.so*"):
                candidates.append(env_path)

        # Environment override if set
        env_path = os.environ.get("LIBCLANG_PATH")
        if env_path:
            env_candidate = Path(env_path)
            if env_candidate.is_dir():
                for env_path in env_candidate.glob("libclang*.so*"):
                    candidates.insert(0, env_path)
            elif env_candidate.exists():
                candidates.insert(0, env_candidate)

        for candidate in candidates:
            if candidate.exists():
                return str(candidate)
        return None

    @dataclass(frozen=True)
    class ClangParseArgs:
        text: str
        path: str
        args: tuple[str, ...]

    def parse_clang(self, args: "ParserManager.ClangParseArgs") -> tuple[Any | None, str | None]:
        if not self._clang_available or self._clang_index is None:
            return None, f"clang unavailable: {self._clang_error or 'missing dependency'}"
        try:
            from clang import cindex

            unsaved = [(args.path, args.text)]
            tu = self._clang_index.parse(
                args.path,
                args=list(args.args),
                unsaved_files=unsaved,
                options=cindex.TranslationUnit.PARSE_SKIP_FUNCTION_BODIES,
            )
            return tu, None
        except Exception as exc:
            return None, f"clang parse failed: {exc}"

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
