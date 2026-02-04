from __future__ import annotations

import os
import shutil
from pathlib import Path

from .structs import FileIOConfig


class UndoManager:
    def __init__(self, config: FileIOConfig) -> None:
        self._root = Path(config.root).resolve()
        self._backup_mode = config.backup_mode
        self._backup_suffix = config.backup_suffix
        self._backup_dir = Path(config.backup_dir)

    def restore(self, target: Path, delete_backup: bool) -> tuple[bool, str | None]:
        backup = self._find_latest_backup(target)
        if backup is None:
            return False, f"no backup found for {target}"

        try:
            if not backup.exists():
                return False, f"backup missing: {backup}"

            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(backup, target)
        except Exception as exc:
            return False, f"restore failed: {exc}"

        if delete_backup:
            try:
                backup.unlink(missing_ok=True)
            except Exception as exc:
                return False, f"restore ok, but failed to delete backup: {exc}"

        return True, None

    def _find_latest_backup(self, target: Path) -> Path | None:
        if self._backup_mode == "mirror":
            try:
                rel = target.resolve().relative_to(self._root)
            except Exception:
                return None
            candidate = self._backup_dir / rel
            return candidate if candidate.exists() else None

        base = Path(f"{target}{self._backup_suffix}")
        if base.exists():
            return base

        # Look for numbered backups: .bak.1, .bak.2, ... pick highest
        parent = target.parent
        prefix = base.name + "."
        matches = []
        for item in parent.iterdir():
            if item.name.startswith(prefix):
                try:
                    suffix = item.name[len(prefix) :]
                    number = int(suffix)
                    matches.append((number, item))
                except ValueError:
                    continue
        if matches:
            matches.sort(key=lambda x: x[0])
            return matches[-1][1]
        return None

    def collect_targets(self, root: Path, include: tuple[str, ...], exclude: tuple[str, ...]) -> list[Path]:
        from .file_finder import FileFinder
        from .structs import AppConfig

        config = AppConfig(
            root=str(root),
            include_patterns=include,
            exclude_patterns=exclude,
            jobs=0,
            check=False,
            backup=False,
            backup_mode=self._backup_mode,
            backup_suffix=self._backup_suffix,
            backup_dir=str(self._backup_dir),
            report_path="",
            cache_enabled=False,
            cache_path="",
            log_level="ERROR",
            log_file=None,
            policies_default="none",
            policies_enabled=frozenset(),
            policies_disabled=frozenset(),
            policies_order=(),
            policy_settings={},
        )

        finder = FileFinder(config)
        return [Path(path) for path in finder.collect()]

    def restore_all(self, targets: list[Path], delete_backup: bool) -> tuple[int, list[str]]:
        restored = 0
        errors: list[str] = []
        for target in targets:
            ok, err = self.restore(target, delete_backup)
            if ok:
                restored += 1
            else:
                errors.append(err or f"restore failed for {target}")
        return restored, errors
