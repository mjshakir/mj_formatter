from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Iterable

from .file_result import FileResult


@dataclass(frozen=True)
class BackupEntry:
    source: str
    backup: str
    size: int
    mtime_ns: int
    mtime_iso: str
    relative_path: str | None


class BackupManifest:
    def __init__(
        self,
        backup_dir: str,
        run_id: str,
        root: str,
        mode: str,
        suffix: str,
        created_at: str | None = None,
    ) -> None:
        self._backup_dir = Path(backup_dir)
        self._run_id = run_id
        self._root = Path(root).resolve()
        self._mode = mode
        self._suffix = suffix
        self._created_at = created_at or datetime.now(timezone.utc).isoformat(timespec="seconds")
        self._run_dir = self._backup_dir / self._run_id
        self._manifest_path = self._run_dir / "backup_manifest.toml"

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

        self._run_dir.mkdir(parents=True, exist_ok=True)
        content = self._render(entries)
        self._manifest_path.write_text(content, encoding="utf-8")

    def _render(self, entries: list[BackupEntry]) -> str:
        lines = [
            "[meta]",
            f'run_id = "{self._escape(self._run_id)}"',
            f'created_at = "{self._escape(self._created_at)}"',
            f'root = "{self._escape(str(self._root))}"',
            f'backup_dir = "{self._escape(str(self._backup_dir))}"',
            f'mode = "{self._escape(self._mode)}"',
            f'suffix = "{self._escape(self._suffix)}"',
            f"files = {len(entries)}",
        ]

        for entry in entries:
            lines.append("")
            lines.append("[[files]]")
            lines.append(f'source = "{self._escape(entry.source)}"')
            lines.append(f'backup = "{self._escape(entry.backup)}"')
            if entry.relative_path is not None:
                lines.append(f'relative_path = "{self._escape(entry.relative_path)}"')
            lines.append(f"size = {entry.size}")
            lines.append(f"mtime_ns = {entry.mtime_ns}")
            lines.append(f'mtime_iso = "{self._escape(entry.mtime_iso)}"')

        return "\n".join(lines) + "\n"

    def _escape(self, value: str) -> str:
        return value.replace("\\", "\\\\").replace('"', '\\"')
