from __future__ import annotations

import hashlib
from collections import OrderedDict
from collections.abc import Iterable
from typing import Any

from ..types import Edit, PolicyResult, Violation


class RetrySnapshotCache:
    """In-memory LRU cache for retry pipeline outputs."""

    _KEY_VERSION = 1

    def __init__(self, max_entries: int = 128) -> None:
        size = int(max_entries)
        self._enabled = size > 0
        self._max_entries = max(1, size) if self._enabled else 0
        self._store = self._build_store(self._max_entries)
        self._hits = 0
        self._misses = 0

    @property
    def enabled(self) -> bool:
        return self._enabled

    @property
    def hits(self) -> int:
        return self._hits

    @property
    def misses(self) -> int:
        return self._misses

    def get(
        self,
        *,
        path: str,
        text: str,
        confidence_threshold: float,
        confidence_policies: Iterable[str],
        blocked_policies: Iterable[str],
    ) -> PolicyResult | None:
        if not self._enabled:
            return None
        key = self._make_key(
            path=path,
            text=text,
            confidence_threshold=confidence_threshold,
            confidence_policies=confidence_policies,
            blocked_policies=blocked_policies,
        )
        payload = self._store_get(key)
        if payload is None:
            self._misses += 1
            return None
        result = self._deserialize(payload)
        if result is None:
            self._misses += 1
            return None
        self._hits += 1
        return result

    def put(
        self,
        *,
        path: str,
        text: str,
        confidence_threshold: float,
        confidence_policies: Iterable[str],
        blocked_policies: Iterable[str],
        result: PolicyResult,
    ) -> None:
        if not self._enabled:
            return
        key = self._make_key(
            path=path,
            text=text,
            confidence_threshold=confidence_threshold,
            confidence_policies=confidence_policies,
            blocked_policies=blocked_policies,
        )
        self._store_set(key, self._serialize(result))

    def _make_key(
        self,
        *,
        path: str,
        text: str,
        confidence_threshold: float,
        confidence_policies: Iterable[str],
        blocked_policies: Iterable[str],
    ) -> str:
        digest = hashlib.blake2b(digest_size=16)
        digest.update(str(self._KEY_VERSION).encode("ascii"))
        digest.update(b"\0")
        digest.update(path.encode("utf-8", errors="ignore"))
        digest.update(b"\0")
        digest.update(hashlib.blake2b(text.encode("utf-8", errors="ignore"), digest_size=16).digest())
        digest.update(b"\0")
        digest.update(f"{confidence_threshold:.6f}".encode("ascii"))
        digest.update(b"\0")
        digest.update(",".join(sorted(set(str(item) for item in confidence_policies))).encode("utf-8"))
        digest.update(b"\0")
        digest.update(",".join(sorted(set(str(item) for item in blocked_policies))).encode("utf-8"))
        return digest.hexdigest()

    def _serialize(self, result: PolicyResult) -> dict[str, Any]:
        return {
            "text": result.text,
            "violations": [
                {
                    "policy": item.policy,
                    "message": item.message,
                    "line": int(item.line),
                    "column": item.column,
                }
                for item in result.violations
            ],
            "edits": [
                {
                    "policy": item.policy,
                    "line": int(item.line),
                    "before": item.before,
                    "after": item.after,
                }
                for item in result.edits
            ],
            "profile": dict(result.profile or {}),
            "parse_modes": dict(result.parse_modes or {}),
            "warnings": list(result.warnings or []),
        }

    def _deserialize(self, payload: Any) -> PolicyResult | None:
        if not isinstance(payload, dict):
            return None
        violations = [
            Violation(
                policy=str(item.get("policy", "")),
                message=str(item.get("message", "")),
                line=int(item.get("line", 0)),
                column=item.get("column"),
            )
            for item in payload.get("violations", [])
            if isinstance(item, dict)
        ]
        edits = [
            Edit(
                policy=str(item.get("policy", "")),
                line=int(item.get("line", 0)),
                before=str(item.get("before", "")),
                after=str(item.get("after", "")),
            )
            for item in payload.get("edits", [])
            if isinstance(item, dict)
        ]
        return PolicyResult(
            text=str(payload.get("text", "")),
            violations=violations,
            edits=edits,
            profile=dict(payload.get("profile", {})),
            parse_modes=dict(payload.get("parse_modes", {})),
            warnings=list(payload.get("warnings", [])),
        )

    def _build_store(self, size: int):
        if size <= 0:
            return OrderedDict()
        try:
            from cachetools import LRUCache  # type: ignore

            return LRUCache(maxsize=size)
        except Exception:
            return OrderedDict()

    def _store_get(self, key: str) -> Any | None:
        try:
            value = self._store.get(key)
        except Exception:
            return None
        if value is None:
            return None
        if isinstance(self._store, OrderedDict):
            self._store.move_to_end(key)
        return value

    def _store_set(self, key: str, value: Any) -> None:
        if isinstance(self._store, OrderedDict):
            self._store[key] = value
            self._store.move_to_end(key)
            while len(self._store) > self._max_entries:
                self._store.popitem(last=False)
            return
        try:
            self._store[key] = value
        except Exception:
            pass
