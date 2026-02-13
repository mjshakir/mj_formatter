from __future__ import annotations

import hashlib
import json
import pickle
from pathlib import Path
from typing import Any

from ..types import PolicyCacheEntry, PolicyResult
from ..utilities import AtomicWriter


class PolicyCache:
    _version = 3

    def __init__(self, path: str, enabled: bool, save_interval: int = 50) -> None:
        self._path = Path(path)
        self._enabled = enabled
        self._data: dict[str, PolicyCacheEntry] = {}
        self._dirty = 0
        self._save_interval = max(1, save_interval)

    def load(self) -> None:
        if not self._enabled or not self._path.exists():
            self._data = {}
            return
        try:
            with self._path.open("rb") as handle:
                payload = pickle.load(handle)
            if isinstance(payload, dict) and payload.get("version") == self._version:
                self._data = payload.get("data", {})
            else:
                self._data = {}
        except Exception:
            self._data = {}

    def save(self) -> None:
        if not self._enabled or not self._dirty:
            return
        payload = {"version": self._version, "data": self._data}
        data = pickle.dumps(payload, protocol=pickle.HIGHEST_PROTOCOL)
        AtomicWriter.write_bytes(self._path, data)
        self._dirty = 0

    def make_key(
        self,
        policy: str,
        path: str,
        text: str,
        settings: dict[str, object],
        *,
        path_hash: str | None = None,
        text_hash: str | None = None,
        settings_hash: str | None = None,
    ) -> str:
        settings_hash = settings_hash or self.hash_settings(settings)
        text_hash = text_hash or self.hash_text(text)
        path_hash = path_hash or self.hash_text(path)
        return f"{policy}:{settings_hash}:{path_hash}:{text_hash}"

    def get(self, key: str) -> PolicyResult | None:
        if not self._enabled:
            return None
        entry = self._data.get(key)
        if entry is None:
            return None
        return PolicyResult(text=entry.text, violations=entry.violations, edits=entry.edits, warnings=entry.warnings)

    def put(self, key: str, result: PolicyResult) -> None:
        if not self._enabled:
            return
        self._data[key] = PolicyCacheEntry(
            text=result.text,
            violations=result.violations,
            edits=result.edits,
            warnings=result.warnings,
        )
        self._dirty += 1
        if self._dirty >= self._save_interval:
            self.save()

    def merge_file(self, path: Path) -> None:
        if not self._enabled or not path.exists():
            return
        try:
            with path.open("rb") as handle:
                payload = pickle.load(handle)
        except Exception:
            return
        if not isinstance(payload, dict) or int(payload.get("version", 0)) != self._version:
            return
        data = payload.get("data", {})
        if not isinstance(data, dict):
            return
        merged = 0
        for key, entry in data.items():
            if isinstance(entry, PolicyCacheEntry):
                self._data[str(key)] = entry
                merged += 1
        if merged:
            self._dirty += merged

    @staticmethod
    def hash_text(text: str) -> str:
        return hashlib.blake2b(text.encode("utf-8"), digest_size=16).hexdigest()

    @staticmethod
    def hash_settings(settings: dict[str, object]) -> str:
        try:
            payload = json.dumps(settings, sort_keys=True, default=str)
        except Exception:
            payload = repr(settings)
        return hashlib.blake2b(payload.encode("utf-8"), digest_size=16).hexdigest()
