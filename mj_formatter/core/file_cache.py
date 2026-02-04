from __future__ import annotations

import os
import pickle
from pathlib import Path


class FileCache:
    def __init__(self, cache_path: str) -> None:
        self._path = Path(cache_path)
        self._data: dict[str, tuple[int, int]] = {}

    def load(self) -> None:
        if not self._path.exists():
            self._data = {}
            return
        try:
            with self._path.open("rb") as handle:
                self._data = pickle.load(handle)
        except Exception:
            self._data = {}

    def save(self) -> None:
        self._path.parent.mkdir(parents=True, exist_ok=True)
        with self._path.open("wb") as handle:
            pickle.dump(self._data, handle, protocol=pickle.HIGHEST_PROTOCOL)

    def should_process(self, path: str) -> bool:
        try:
            stat = os.stat(path)
        except FileNotFoundError:
            return False
        cached = self._data.get(path)
        if cached is None:
            return True
        return cached != (stat.st_mtime_ns, stat.st_size)

    def update(self, path: str) -> None:
        try:
            stat = os.stat(path)
        except FileNotFoundError:
            return
        self._data[path] = (stat.st_mtime_ns, stat.st_size)
