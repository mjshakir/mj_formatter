from __future__ import annotations

import fnmatch
from pathlib import Path

from .app_config import AppConfig


class FileFinder:
    def __init__(self, config: AppConfig) -> None:
        self._root = Path(config.root).resolve()
        self._include = config.include_patterns
        self._exclude = config.exclude_patterns

    def collect(self) -> list[str]:
        files: set[Path] = set()
        for pattern in self._include:
            for path in self._root.glob(pattern):
                if path.is_file():
                    files.add(path)

        result: list[str] = []
        for path in sorted(files):
            rel = path.relative_to(self._root).as_posix()
            if self._is_excluded(rel):
                continue
            result.append(str(path))
        return result

    def _is_excluded(self, relative_path: str) -> bool:
        for pattern in self._exclude:
            if fnmatch.fnmatch(relative_path, pattern):
                return True
        return False
