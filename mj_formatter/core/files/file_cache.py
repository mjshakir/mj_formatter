from __future__ import annotations

import os
import pickle
from pathlib import Path

from ..utilities import AtomicWriter


class FileCache:
    _version = 2

    def __init__(self, cache_path: str, fingerprint: str = "") -> None:
        self._path = Path(cache_path)
        self._fingerprint = str(fingerprint)
        self._data: dict[str, tuple[int, int, str]] = {}

    def load(self) -> None:
        if not self._path.exists():
            self._data = {}
            return
        try:
            with self._path.open("rb") as handle:
                payload = pickle.load(handle)
            if isinstance(payload, dict) and int(payload.get("version", 0)) == self._version:
                raw_data = payload.get("data", {})
            else:
                raw_data = payload
            if isinstance(raw_data, dict):
                normalized: dict[str, tuple[int, int, str]] = {}
                for key, value in raw_data.items():
                    if (
                        isinstance(value, tuple)
                        and len(value) == 3
                        and isinstance(value[0], int)
                        and isinstance(value[1], int)
                    ):
                        normalized[str(key)] = (int(value[0]), int(value[1]), str(value[2]))
                    elif (
                        isinstance(value, tuple)
                        and len(value) == 2
                        and isinstance(value[0], int)
                        and isinstance(value[1], int)
                    ):
                        # Backward compatibility: old cache had no config/style fingerprint.
                        normalized[str(key)] = (int(value[0]), int(value[1]), "")
                self._data = normalized
            else:
                self._data = {}
        except Exception:
            self._data = {}

    def save(self) -> None:
        payload = {"version": self._version, "data": self._data}
        data = pickle.dumps(payload, protocol=pickle.HIGHEST_PROTOCOL)
        AtomicWriter.write_bytes(self._path, data)

    def should_process(self, path: str) -> bool:
        try:
            stat = os.stat(path)
        except FileNotFoundError:
            return False
        cached = self._data.get(path)
        if cached is None:
            return True
        return cached != (stat.st_mtime_ns, stat.st_size, self._fingerprint)

    def update(self, path: str) -> None:
        try:
            stat = os.stat(path)
        except FileNotFoundError:
            return
        self._data[path] = (stat.st_mtime_ns, stat.st_size, self._fingerprint)
