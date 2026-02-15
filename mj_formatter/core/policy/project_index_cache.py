from __future__ import annotations

import hashlib
import pickle
from pathlib import Path
from typing import Any

from ..types import SemanticContext
from ..utilities import AtomicWriter


class ProjectIndexCache:
    _version = 2

    def __init__(self, path: str, enabled: bool = True, max_entries: int = 50000) -> None:
        self._path = Path(path)
        self._enabled = enabled
        self._max_entries = max(1, int(max_entries))
        self._entries: dict[str, SemanticContext] = {}
        self._file_latest_key: dict[str, str] = {}
        self._file_usrs: dict[str, set[str]] = {}
        self._usr_files: dict[str, set[str]] = {}
        self._file_usr_metrics: dict[str, dict[str, tuple[int, float]]] = {}
        self._usr_reference_counts: dict[str, int] = {}
        self._usr_consensus_totals: dict[str, float] = {}
        self._usr_consensus_counts: dict[str, int] = {}
        self._dirty = False

    def load(self) -> None:
        if not self._enabled or not self._path.exists():
            return
        data = self._read_payload(self._path)
        if not isinstance(data, dict):
            return
        if int(data.get("version", 0)) != self._version:
            return
        self._entries = data.get("entries", {}) if isinstance(data.get("entries"), dict) else {}
        self._file_latest_key = (
            data.get("file_latest_key", {}) if isinstance(data.get("file_latest_key"), dict) else {}
        )
        self._file_usrs = data.get("file_usrs", {}) if isinstance(data.get("file_usrs"), dict) else {}
        self._usr_files = data.get("usr_files", {}) if isinstance(data.get("usr_files"), dict) else {}
        self._file_usr_metrics = (
            data.get("file_usr_metrics", {}) if isinstance(data.get("file_usr_metrics"), dict) else {}
        )
        self._usr_reference_counts = (
            data.get("usr_reference_counts", {}) if isinstance(data.get("usr_reference_counts"), dict) else {}
        )
        self._usr_consensus_totals = (
            data.get("usr_consensus_totals", {}) if isinstance(data.get("usr_consensus_totals"), dict) else {}
        )
        self._usr_consensus_counts = (
            data.get("usr_consensus_counts", {}) if isinstance(data.get("usr_consensus_counts"), dict) else {}
        )

    def save(self) -> None:
        if not self._enabled or not self._dirty:
            return
        payload = {
            "version": self._version,
            "entries": self._entries,
            "file_latest_key": self._file_latest_key,
            "file_usrs": self._file_usrs,
            "usr_files": self._usr_files,
            "file_usr_metrics": self._file_usr_metrics,
            "usr_reference_counts": self._usr_reference_counts,
            "usr_consensus_totals": self._usr_consensus_totals,
            "usr_consensus_counts": self._usr_consensus_counts,
        }
        data = pickle.dumps(payload, protocol=pickle.HIGHEST_PROTOCOL)
        AtomicWriter.write_bytes(self._path, data)
        self._dirty = False

    def get_semantic(self, path: str, text: str) -> SemanticContext | None:
        if not self._enabled:
            return None
        key = self._make_key(path, text)
        value = self._entries.get(key)
        if isinstance(value, SemanticContext):
            return value
        return None

    def put_semantic(self, path: str, text: str, semantic: SemanticContext) -> None:
        if not self._enabled:
            return
        path_norm = str(Path(path).resolve())
        key = self._make_key(path, text)

        old_usrs = self._file_usrs.get(path_norm, set())
        for usr in old_usrs:
            files = self._usr_files.get(usr)
            if not files:
                continue
            files.discard(path_norm)
            if not files:
                self._usr_files.pop(usr, None)

        old_metrics = self._file_usr_metrics.get(path_norm, {})
        for usr, (ref_count, consensus) in old_metrics.items():
            self._usr_reference_counts[usr] = max(0, int(self._usr_reference_counts.get(usr, 0)) - int(ref_count))
            if self._usr_reference_counts.get(usr, 0) <= 0:
                self._usr_reference_counts.pop(usr, None)
            self._usr_consensus_totals[usr] = float(self._usr_consensus_totals.get(usr, 0.0)) - float(consensus)
            self._usr_consensus_counts[usr] = max(0, int(self._usr_consensus_counts.get(usr, 0)) - 1)
            if self._usr_consensus_counts.get(usr, 0) <= 0:
                self._usr_consensus_counts.pop(usr, None)
                self._usr_consensus_totals.pop(usr, None)

        usrs = {symbol.usr for symbol in semantic.symbols if symbol.usr}
        self._file_usrs[path_norm] = usrs
        for usr in usrs:
            self._usr_files.setdefault(usr, set()).add(path_norm)

        ref_counts = {usr: int(count) for usr, count in semantic.reference_count_by_usr}
        consensus = {usr: float(score) for usr, score in semantic.consensus_by_usr}
        current_metrics: dict[str, tuple[int, float]] = {}
        for usr in usrs:
            ref_count = int(ref_counts.get(usr, 0))
            score = float(consensus.get(usr, 1.0))
            current_metrics[usr] = (ref_count, score)
            self._usr_reference_counts[usr] = int(self._usr_reference_counts.get(usr, 0)) + ref_count
            self._usr_consensus_totals[usr] = float(self._usr_consensus_totals.get(usr, 0.0)) + score
            self._usr_consensus_counts[usr] = int(self._usr_consensus_counts.get(usr, 0)) + 1
        self._file_usr_metrics[path_norm] = current_metrics

        self._entries[key] = semantic
        self._file_latest_key[path_norm] = key

        if len(self._entries) > self._max_entries:
            overflow = len(self._entries) - self._max_entries
            for old_key in list(self._entries.keys())[:overflow]:
                self._entries.pop(old_key, None)

        self._dirty = True

    def symbol_file_count(self, usr: str) -> int:
        if not usr:
            return 0
        files = self._usr_files.get(usr)
        if not files:
            return 0
        return len(files)

    def symbol_reference_count(self, usr: str) -> int:
        if not usr:
            return 0
        return int(self._usr_reference_counts.get(usr, 0))

    def symbol_consensus_score(self, usr: str) -> float:
        if not usr:
            return 0.0
        count = int(self._usr_consensus_counts.get(usr, 0))
        if count <= 0:
            return 0.0
        total = float(self._usr_consensus_totals.get(usr, 0.0))
        return total / float(count)

    def merge_file(self, path: Path) -> None:
        if not self._enabled or not path.exists():
            return
        data = self._read_payload(path)
        if not isinstance(data, dict):
            return
        if int(data.get("version", 0)) != self._version:
            return
        entries = data.get("entries", {})
        latest = data.get("file_latest_key", {})
        if not isinstance(entries, dict) or not isinstance(latest, dict):
            return
        changed = False
        for key, semantic in entries.items():
            if isinstance(semantic, SemanticContext):
                self._entries[str(key)] = semantic
                changed = True
        for file_path, cache_key in latest.items():
            self._file_latest_key[str(file_path)] = str(cache_key)
            changed = True
        if changed:
            self._rebuild_indexes()
            self._dirty = True

    def prune_to_files(self, valid_paths: set[str]) -> None:
        if not self._enabled:
            return
        normalized = {str(Path(item).resolve()) for item in valid_paths}
        stale = [path for path in self._file_latest_key if path not in normalized]
        if not stale:
            return
        for path in stale:
            self._file_latest_key.pop(path, None)
        self._rebuild_indexes()
        self._dirty = True

    def _make_key(self, path: str, text: str) -> str:
        path_norm = str(Path(path).resolve())
        digest = hashlib.blake2b(digest_size=16)
        digest.update(path_norm.encode("utf-8", errors="ignore"))
        digest.update(b"\0")
        digest.update(text.encode("utf-8", errors="ignore"))
        return digest.hexdigest()

    def _read_payload(self, path: Path) -> dict[str, Any] | None:
        try:
            with path.open("rb") as handle:
                data = pickle.load(handle)
        except Exception:
            return None
        if isinstance(data, dict):
            return data
        return None

    def _rebuild_indexes(self) -> None:
        file_usrs: dict[str, set[str]] = {}
        usr_files: dict[str, set[str]] = {}
        file_usr_metrics: dict[str, dict[str, tuple[int, float]]] = {}
        usr_reference_counts: dict[str, int] = {}
        usr_consensus_totals: dict[str, float] = {}
        usr_consensus_counts: dict[str, int] = {}

        for file_path, cache_key in self._file_latest_key.items():
            semantic = self._entries.get(cache_key)
            if not isinstance(semantic, SemanticContext):
                continue
            usrs = {symbol.usr for symbol in semantic.symbols if symbol.usr}
            file_usrs[file_path] = usrs
            for usr in usrs:
                usr_files.setdefault(usr, set()).add(file_path)

            ref_counts = {usr: int(count) for usr, count in semantic.reference_count_by_usr}
            consensus = {usr: float(score) for usr, score in semantic.consensus_by_usr}
            metrics: dict[str, tuple[int, float]] = {}
            for usr in usrs:
                ref_count = int(ref_counts.get(usr, 0))
                score = float(consensus.get(usr, 1.0))
                metrics[usr] = (ref_count, score)
                usr_reference_counts[usr] = int(usr_reference_counts.get(usr, 0)) + ref_count
                usr_consensus_totals[usr] = float(usr_consensus_totals.get(usr, 0.0)) + score
                usr_consensus_counts[usr] = int(usr_consensus_counts.get(usr, 0)) + 1
            file_usr_metrics[file_path] = metrics

        self._file_usrs = file_usrs
        self._usr_files = usr_files
        self._file_usr_metrics = file_usr_metrics
        self._usr_reference_counts = usr_reference_counts
        self._usr_consensus_totals = usr_consensus_totals
        self._usr_consensus_counts = usr_consensus_counts
