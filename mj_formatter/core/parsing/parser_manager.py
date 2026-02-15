from __future__ import annotations

from pathlib import Path
from typing import Any
import warnings
import os
from ctypes.util import find_library
from functools import lru_cache
from collections.abc import Sequence
from threading import Lock, local
from typing import Final, Literal

from ..types import ClangParseArgs


class ParserManager:
    _CPP_EXTENSIONS: Final[frozenset[str]] = frozenset({".cpp", ".cc", ".cxx", ".hpp", ".hh", ".hxx", ".h"})
    _C_EXTENSIONS: Final[frozenset[str]] = frozenset({".c"})
    ClangParseArgs = ClangParseArgs

    def __init__(self, clang_library_paths: Sequence[str] = ()) -> None:
        self._ts_available = False
        self._ts_error: str | None = None
        self._ts_languages: dict[str, Any] = {}
        self._ts_lock = Lock()
        self._ts_parser_tls = local()
        self._ts_provider = "none"
        self._clang_available = False
        self._clang_error: str | None = None
        self._clang_tls = local()
        self._clang_lock = Lock()
        self._clang_library_paths = tuple(str(item) for item in clang_library_paths)
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
        except Exception as exc:  # pragma: no cover
            self._ts_available = False
            self._ts_error = str(exc)
            return
        try:
            import tree_sitter_cpp  # noqa: F401
            import tree_sitter_c  # noqa: F401

            self._ts_provider = "native"
            self._ts_available = True
            return
        except Exception:
            pass
        try:
            from tree_sitter_languages import get_language  # noqa: F401
        except Exception as exc:  # pragma: no cover
            self._ts_available = False
            self._ts_error = str(exc)
            return
        self._ts_provider = "bundle"
        self._ts_available = True

    def _init_clang(self) -> None:
        try:
            from clang import cindex

            # libclang location can only be configured before the first load.
            # If python bindings already expose a configured path (for example
            # from the `libclang` wheel), do not override it with a system copy.
            if not getattr(cindex.Config, "loaded", False):
                configured_file = getattr(cindex.Config, "library_file", None)
                configured_path = getattr(cindex.Config, "library_path", None)
                if not configured_file and not configured_path:
                    libclang = self._find_libclang()
                    if libclang:
                        cindex.Config.set_library_file(libclang)
            self._clang_tls.index = cindex.Index.create()
            self._clang_available = True
        except Exception as exc:  # pragma: no cover
            self._clang_available = False
            self._clang_error = str(exc)

    def _find_libclang(self) -> str | None:
        candidates: list[Path] = []

        for raw in self._clang_library_paths:
            candidate = Path(raw)
            if candidate.is_dir():
                for item in candidate.glob("libclang*.so*"):
                    if "clang-cpp" in item.name:
                        continue
                    candidates.append(item)
                continue
            if candidate.exists():
                if "clang-cpp" in candidate.name:
                    continue
                candidates.append(candidate)

        # Environment override if set
        env_path = os.environ.get("LIBCLANG_PATH")
        if env_path:
            env_candidate = Path(env_path)
            if env_candidate.is_dir():
                for env_path in env_candidate.glob("libclang*.so*"):
                    if "clang-cpp" in env_path.name:
                        continue
                    candidates.insert(0, env_path)
            elif env_candidate.exists():
                if "clang-cpp" in env_candidate.name:
                    return None
                candidates.insert(0, env_candidate)

        # Prefer stable sonames first, then versioned variants.
        candidates.sort(key=lambda p: (".so." in p.name, len(p.name)))
        for candidate in candidates:
            if candidate.exists():
                return str(candidate)
        return find_library("clang")

    def parse_clang(self, args: "ParserManager.ClangParseArgs") -> tuple[Any | None, str | None]:
        if not self._clang_available:
            return None, f"clang unavailable: {self._clang_error or 'missing dependency'}"
        try:
            from clang import cindex

            index = self._get_clang_index(cindex)
            unsaved = [(args.path, args.text)]
            options = 0
            if not args.include_function_bodies:
                options |= cindex.TranslationUnit.PARSE_SKIP_FUNCTION_BODIES
            tu = index.parse(
                args.path,
                args=list(args.args),
                unsaved_files=unsaved,
                options=options,
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
        parsers = self._get_thread_local_parsers()
        if language in parsers:
            return parsers[language]
        from tree_sitter import Parser

        parser = Parser()
        ts_language = self._get_language(language)
        if ts_language is None:
            raise RuntimeError(f"unsupported tree-sitter language: {language}")
        if hasattr(parser, "set_language"):
            parser.set_language(ts_language)
        else:  # pragma: no cover - compatibility fallback
            parser.language = ts_language
        parsers[language] = parser
        return parser

    def _get_thread_local_parsers(self) -> dict[str, Any]:
        parsers = getattr(self._ts_parser_tls, "parsers", None)
        if parsers is None:
            parsers = {}
            self._ts_parser_tls.parsers = parsers
        return parsers

    def _get_language(self, language: str) -> Any | None:
        with self._ts_lock:
            cached = self._ts_languages.get(language)
            if cached is not None:
                return cached
            ts_language: Any | None = None
            if self._ts_provider == "native":
                ts_language = self._native_language(language)
            elif self._ts_provider == "bundle":
                from tree_sitter_languages import get_language

                ts_language = get_language(language)
            if ts_language is not None:
                self._ts_languages[language] = ts_language
            return ts_language

    def _get_clang_index(self, cindex_module: Any) -> Any:
        index = getattr(self._clang_tls, "index", None)
        if index is not None:
            return index
        with self._clang_lock:
            index = getattr(self._clang_tls, "index", None)
            if index is not None:
                return index
            created = cindex_module.Index.create()
            self._clang_tls.index = created
            return created

    def _native_language(self, language: str) -> Any | None:
        try:
            from tree_sitter import Language
        except Exception:
            return None

        module_name = ""
        if language == "cpp":
            module_name = "tree_sitter_cpp"
        elif language == "c":
            module_name = "tree_sitter_c"
        else:
            return None
        try:
            module = __import__(module_name)
            capsule = module.language()
            try:
                return Language(capsule)
            except TypeError:
                return capsule
        except Exception:
            return None

    def _guess_language(self, path: str) -> str | None:
        ext = Path(path).suffix.lower()
        return self._guess_language_from_extension(ext)

    @staticmethod
    @lru_cache(maxsize=128)
    def _guess_language_from_extension(ext: str) -> Literal["cpp", "c"] | None:
        if ext in ParserManager._CPP_EXTENSIONS:
            return "cpp"
        if ext in ParserManager._C_EXTENSIONS:
            return "c"
        return None

    def has_tree_sitter(self) -> bool:
        return bool(self._ts_available)

    def has_clang(self) -> bool:
        return bool(self._clang_available)

    def tree_sitter_error(self) -> str | None:
        return self._ts_error

    def clang_error(self) -> str | None:
        return self._clang_error
