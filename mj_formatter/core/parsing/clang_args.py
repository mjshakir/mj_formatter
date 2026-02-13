from __future__ import annotations

import json
import shlex
from pathlib import Path
from typing import Iterable

from ..types import AppConfig, ClangArgsMode


class ClangArgsResolver:
    def __init__(self, config: AppConfig) -> None:
        self.config = config
        self._loaded = False
        self._map: dict[str, list[str]] | None = None

    def get_args(self, path: str) -> list[str]:
        compdb_args = self._get_compdb_args(path)
        mode = ClangArgsMode(self.config.clang_args_mode)
        match mode:
            case ClangArgsMode.ARGS_ONLY:
                return list(self.config.clang_args)
            case ClangArgsMode.COMPDB_ONLY:
                return compdb_args or []
            case ClangArgsMode.COMPDB_THEN_ARGS:
                return compdb_args if compdb_args else list(self.config.clang_args)
            case _:
                args = list(self.config.clang_args)
                if compdb_args:
                    args.extend(compdb_args)
                return args

    def _get_compdb_args(self, path: str) -> list[str]:
        if not self._loaded:
            self._load_compdb()
        if not self._map:
            return []
        key = str(Path(path).resolve())
        return self._map.get(key, [])

    def _load_compdb(self) -> None:
        self._loaded = True
        compdb_path = self._find_compdb()
        if not compdb_path:
            self._map = {}
            return
        try:
            data = json.loads(compdb_path.read_text(encoding="utf-8"))
        except Exception:
            self._map = {}
            return
        mapping: dict[str, list[str]] = {}
        for entry in data:
            file_path = entry.get("file")
            if not file_path:
                continue
            args = self._extract_args(entry)
            if not args:
                continue
            mapping[str(Path(file_path).resolve())] = args
        self._map = mapping

    def _find_compdb(self) -> Path | None:
        if self.config.clang_compdb_path:
            candidate = Path(self.config.clang_compdb_path)
            if candidate.is_dir():
                candidate = candidate / "compile_commands.json"
            if candidate.exists():
                return candidate
        root = Path(self.config.root).resolve()
        candidate = root / "compile_commands.json"
        if candidate.exists():
            return candidate
        return None

    def _extract_args(self, entry: dict) -> list[str]:
        if "arguments" in entry and isinstance(entry["arguments"], list):
            args = [str(a) for a in entry["arguments"]]
        else:
            cmd = entry.get("command")
            if not cmd:
                return []
            args = shlex.split(cmd)
        return self._sanitize_args(args, str(entry.get("file", "")))

    def _sanitize_args(self, args: Iterable[str], file_path: str) -> list[str]:
        result: list[str] = []
        skip_next = False
        file_path = str(Path(file_path).resolve())
        file_name = Path(file_path).name
        for arg in args:
            if skip_next:
                skip_next = False
                continue
            if arg in {"-c", "-o"}:
                skip_next = True
                continue
            if arg == file_path or arg == file_name:
                continue
            result.append(arg)
        return result
