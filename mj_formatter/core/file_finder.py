from __future__ import annotations

import fnmatch
import os
import re
from pathlib import Path

from .app_config import AppConfig


class FileFinder:
    def __init__(self, config: AppConfig) -> None:
        self._config = config
        self._root = Path(config.root).resolve()
        self._include = self._expand_patterns(config.include_patterns)
        self._exclude = self._expand_patterns(config.exclude_patterns)
        self._include_re = [re.compile(fnmatch.translate(p)) for p in self._include]
        self._exclude_re = [re.compile(fnmatch.translate(p)) for p in self._exclude]

    def collect(self) -> list[str]:
        if not self._include_re:
            return []

        result: list[str] = []
        stack: list[tuple[Path, str]] = [(self._root, "")]
        while stack:
            dir_path, rel_dir = stack.pop()
            try:
                with os.scandir(dir_path) as entries:
                    for entry in entries:
                        name = entry.name
                        rel_path = f"{rel_dir}/{name}" if rel_dir else name
                        rel_path = rel_path.replace(os.sep, "/")
                        if entry.is_dir(follow_symlinks=False):
                            if self._is_excluded_dir(rel_path):
                                continue
                            stack.append((Path(entry.path), rel_path))
                            continue
                        if not entry.is_file(follow_symlinks=False):
                            continue
                        if not self._is_included(rel_path):
                            continue
                        if self._is_excluded(rel_path):
                            continue
                        result.append(str(Path(entry.path)))
            except FileNotFoundError:
                continue

        if self._config.sort_results:
            return sorted(result)
        return result

    def _match_any(self, patterns: list[re.Pattern[str]], path: str) -> bool:
        return any(pattern.match(path) for pattern in patterns)

    def _is_included(self, relative_path: str) -> bool:
        return self._match_any(self._include_re, relative_path)

    def _is_excluded(self, relative_path: str) -> bool:
        return self._match_any(self._exclude_re, relative_path)

    def _is_excluded_dir(self, relative_dir: str) -> bool:
        if not self._exclude_re:
            return False
        probe = relative_dir.rstrip("/") + "/"
        return self._match_any(self._exclude_re, probe)

    def _expand_patterns(self, patterns: tuple[str, ...]) -> tuple[str, ...]:
        expanded: set[str] = set()
        for pattern in patterns:
            expanded.update(self._expand_glob(pattern))
        return tuple(sorted(expanded))

    def _expand_glob(self, pattern: str) -> set[str]:
        token = "**/"
        idx = pattern.find(token)
        if idx == -1:
            return {pattern}
        before = pattern[:idx]
        after = pattern[idx + len(token) :]
        results: set[str] = set()
        for tail in self._expand_glob(after):
            results.add(before + token + tail)
            results.add(before + tail)
        return results
