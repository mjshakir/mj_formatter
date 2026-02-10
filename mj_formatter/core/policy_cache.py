from __future__ import annotations

import hashlib
import json
import pickle
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from .policy_result import PolicyResult


@dataclass
class PolicyCacheEntry:
    text: str
    violations: list[Any]
    edits: list[Any]


class PolicyCache:
    _version = 2

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
        self._path.parent.mkdir(parents=True, exist_ok=True)
        payload = {"version": self._version, "data": self._data}
        with self._path.open("wb") as handle:
            pickle.dump(payload, handle, protocol=pickle.HIGHEST_PROTOCOL)
        self._dirty = 0

    def make_key(self, policy: str, path: str, text: str, settings: dict[str, object]) -> str:
        settings_hash = self._hash_settings(settings)
        text_hash = self._hash_text(text)
        path_hash = self._hash_text(path)
        return f"{policy}:{settings_hash}:{path_hash}:{text_hash}"

    def get(self, key: str) -> PolicyResult | None:
        if not self._enabled:
            return None
        entry = self._data.get(key)
        if entry is None:
            return None
        return PolicyResult(text=entry.text, violations=entry.violations, edits=entry.edits)

    def put(self, key: str, result: PolicyResult) -> None:
        if not self._enabled:
            return
        self._data[key] = PolicyCacheEntry(
            text=result.text,
            violations=result.violations,
            edits=result.edits,
        )
        self._dirty += 1
        if self._dirty >= self._save_interval:
            self.save()

    def _hash_text(self, text: str) -> str:
        return hashlib.blake2b(text.encode("utf-8"), digest_size=16).hexdigest()

    def _hash_settings(self, settings: dict[str, object]) -> str:
        try:
            payload = json.dumps(settings, sort_keys=True, default=str)
        except Exception:
            payload = repr(settings)
        return hashlib.blake2b(payload.encode("utf-8"), digest_size=16).hexdigest()
