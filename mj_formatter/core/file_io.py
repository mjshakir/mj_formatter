from __future__ import annotations

import os
import shutil
import tempfile
from pathlib import Path

from .structs import FileIOConfig


class FileIO:
    def __init__(self, config: FileIOConfig) -> None:
        self._root = Path(config.root).resolve()
        self._backup = config.backup
        self._backup_mode = config.backup_mode
        self._backup_suffix = config.backup_suffix
        self._backup_dir = Path(config.backup_dir)
        self._backup_run = os.environ.get("MJ_FORMATTER_BACKUP_RUN")
        self._backup_root = self._backup_dir / self._backup_run if self._backup_run else self._backup_dir

    def read_text(self, path: str) -> str:
        with open(path, "r", encoding="utf-8") as handle:
            return handle.read()

    def write_text(self, path: str, text: str) -> tuple[str | None, str | None]:
        backup_path = None
        if self._backup:
            backup_path, error = self._make_backup(path)
            if error:
                return None, error

        error = self._write_atomic(path, text)
        if error:
            return backup_path, error

        return backup_path, None

    def _make_backup(self, path: str) -> tuple[str | None, str | None]:
        try:
            src = Path(path)
            rel = src.resolve().relative_to(self._root)
            dest = self._backup_root / rel
            if self._backup_mode != "mirror":
                dest = dest.with_name(dest.name + self._backup_suffix)
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest = self._unique_path(dest)
            shutil.copy2(src, dest)
            return str(dest), None
        except Exception as exc:
            return None, f"backup failed: {exc}"

    def _unique_path(self, path: Path) -> Path:
        if not path.exists():
            return path
        counter = 1
        while True:
            candidate = path.with_suffix(path.suffix + f".{counter}")
            if not candidate.exists():
                return candidate
            counter += 1

    def _write_atomic(self, path: str, text: str) -> str | None:
        target = Path(path)
        try:
            with tempfile.NamedTemporaryFile(
                "w",
                encoding="utf-8",
                delete=False,
                dir=str(target.parent),
            ) as handle:
                handle.write(text)
                temp_name = handle.name
            os.replace(temp_name, path)
        except Exception as exc:
            try:
                if "temp_name" in locals() and Path(temp_name).exists():
                    Path(temp_name).unlink(missing_ok=True)
            except Exception:
                pass
            return f"write failed: {exc}"
        return None
