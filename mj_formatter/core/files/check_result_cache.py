from __future__ import annotations

import hashlib
import logging
from collections import OrderedDict
from pathlib import Path
from typing import Any

from ..types import Edit, FileResult, Violation


class CheckResultCache:
    _VERSION = 1

    def __init__(self, path: str, enabled: bool = True, l1_size: int = 2048) -> None:
        self._enabled = bool(enabled)
        self._path = Path(path)
        self._l1_size = max(64, int(l1_size))
        self._logger = logging.getLogger("mj_formatter")
        self._disk: Any | None = None
        self._l1 = self._build_l1(self._l1_size)
        if self._enabled:
            self._disk = self._open_disk_cache(self._path)

    @property
    def enabled(self) -> bool:
        return self._enabled

    def close(self) -> None:
        disk = self._disk
        self._disk = None
        if disk is None:
            return
        try:
            disk.close()
        except Exception:
            pass

    def hash_file(self, path: str) -> str | None:
        file_path = Path(path)
        if not file_path.exists():
            return None
        digest = hashlib.blake2b(digest_size=16)
        try:
            with file_path.open("rb") as handle:
                while True:
                    chunk = handle.read(1024 * 1024)
                    if not chunk:
                        break
                    digest.update(chunk)
        except Exception:
            return None
        return digest.hexdigest()

    def make_key(self, *, path: str, content_hash: str, fingerprint: str) -> str:
        digest = hashlib.blake2b(digest_size=16)
        digest.update(str(Path(path).resolve()).encode("utf-8", errors="ignore"))
        digest.update(b"\0")
        digest.update(content_hash.encode("utf-8", errors="ignore"))
        digest.update(b"\0")
        digest.update(fingerprint.encode("utf-8", errors="ignore"))
        return digest.hexdigest()

    def get(self, *, path: str, content_hash: str, fingerprint: str) -> FileResult | None:
        if not self._enabled:
            return None
        key = self.make_key(path=path, content_hash=content_hash, fingerprint=fingerprint)

        payload = self._l1_get(key)
        if payload is not None:
            result = self._deserialize(payload)
            if result is not None:
                result.cache_hit = True
            return result

        disk = self._disk
        if disk is None:
            return None
        try:
            payload = disk.get(key, default=None)
        except Exception as exc:
            self._logger.warning("check-result cache read failed: %s", exc)
            return None
        if payload is None:
            return None
        self._l1_set(key, payload)
        result = self._deserialize(payload)
        if result is not None:
            result.cache_hit = True
        return result

    def put(self, *, path: str, content_hash: str, fingerprint: str, result: FileResult) -> None:
        if not self._enabled or result.error:
            return
        key = self.make_key(path=path, content_hash=content_hash, fingerprint=fingerprint)
        payload = self._serialize(result)
        self._l1_set(key, payload)
        disk = self._disk
        if disk is None:
            return
        try:
            disk.set(key, payload)
        except Exception as exc:
            self._logger.warning("check-result cache write failed: %s", exc)

    def _build_l1(self, size: int):
        try:
            from cachetools import LRUCache  # type: ignore

            return LRUCache(maxsize=size)
        except Exception:
            return OrderedDict()

    def _l1_get(self, key: str) -> Any | None:
        try:
            value = self._l1.get(key)
        except Exception:
            value = None
        if value is None:
            return None
        if isinstance(self._l1, OrderedDict):
            self._l1.move_to_end(key)
        return value

    def _l1_set(self, key: str, value: Any) -> None:
        if isinstance(self._l1, OrderedDict):
            self._l1[key] = value
            self._l1.move_to_end(key)
            while len(self._l1) > self._l1_size:
                self._l1.popitem(last=False)
            return
        try:
            self._l1[key] = value
        except Exception:
            pass

    def _open_disk_cache(self, path: Path):
        try:
            from diskcache import Cache  # type: ignore

            path.parent.mkdir(parents=True, exist_ok=True)
            return Cache(str(path))
        except Exception as exc:
            self._logger.warning("diskcache unavailable for check-result cache: %s", exc)
            return None

    def _serialize(self, result: FileResult) -> dict[str, Any]:
        return {
            "version": self._VERSION,
            "path": result.path,
            "changed": bool(result.changed),
            "error": result.error,
            "backup_path": result.backup_path,
            "profile": dict(result.profile or {}),
            "parse_modes": dict(result.parse_modes or {}),
            "warnings": list(result.warnings or []),
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
        }

    def _deserialize(self, payload: Any) -> FileResult | None:
        if not isinstance(payload, dict):
            return None
        if int(payload.get("version", 0)) != self._VERSION:
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
        return FileResult(
            path=str(payload.get("path", "")),
            changed=bool(payload.get("changed", False)),
            violations=violations,
            edits=edits,
            error=payload.get("error"),
            backup_path=payload.get("backup_path"),
            cache_hit=True,
            profile=payload.get("profile") if isinstance(payload.get("profile"), dict) else None,
            parse_modes=payload.get("parse_modes") if isinstance(payload.get("parse_modes"), dict) else None,
            warnings=payload.get("warnings") if isinstance(payload.get("warnings"), list) else None,
        )

