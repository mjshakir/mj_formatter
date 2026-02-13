from __future__ import annotations

from datetime import datetime, timezone
from pathlib import Path
from typing import Iterable

from ..config.toml_store import TomlStore
from ..types import FileResult
from ..types import BackupEntry, BackupManifestConfig


class BackupManifest:
    def __init__(self, config: BackupManifestConfig, toml_store: TomlStore | None = None) -> None:
        self._backup_dir = Path(config.backup_dir)
        self._run_id = config.run_id
        self._root = Path(config.root).resolve()
        self._mode = config.mode
        self._suffix = config.suffix
        self._created_at = config.created_at or datetime.now(timezone.utc).isoformat(timespec="seconds")
        self._run_dir = self._backup_dir / self._run_id
        self._manifest_path = self._run_dir / "backup_manifest.toml"
        self._toml_store = toml_store or TomlStore()

    def write(self, results: Iterable[FileResult]) -> None:
        entries: list[BackupEntry] = []
        for result in results:
            if not result.backup_path:
                continue
            backup_path = Path(result.backup_path)
            try:
                stat = backup_path.stat()
            except FileNotFoundError:
                continue
            mtime_iso = datetime.fromtimestamp(stat.st_mtime_ns / 1e9, tz=timezone.utc).isoformat(
                timespec="seconds"
            )
            rel = None
            try:
                rel = str(Path(result.path).resolve().relative_to(self._root))
            except Exception:
                rel = None
            entries.append(
                BackupEntry(
                    source=str(Path(result.path).resolve()),
                    backup=str(backup_path.resolve()),
                    size=stat.st_size,
                    mtime_ns=stat.st_mtime_ns,
                    mtime_iso=mtime_iso,
                    relative_path=rel,
                )
            )

        payload = self._build_payload(entries)
        self._toml_store.write(self._manifest_path, payload)

    def _build_payload(self, entries: list[BackupEntry]) -> dict[str, object]:
        files: list[dict[str, object]] = []
        for entry in entries:
            item: dict[str, object] = {
                "source": entry.source,
                "backup": entry.backup,
                "size": entry.size,
                "mtime_ns": entry.mtime_ns,
                "mtime_iso": entry.mtime_iso,
            }
            if entry.relative_path is not None:
                item["relative_path"] = entry.relative_path
            files.append(item)
        return {
            "meta": {
                "format_version": 1,
                "tool": "mj_formatter",
                "run_id": self._run_id,
                "created_at": self._created_at,
                "root": str(self._root),
                "backup_dir": str(self._backup_dir),
                "mode": self._mode,
                "suffix": self._suffix,
                "files": len(entries),
            },
            "files": files,
        }
