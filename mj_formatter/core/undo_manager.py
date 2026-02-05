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
        try:
            rel = target.resolve().relative_to(self._root)
        except Exception:
            rel = None

        if rel is not None:
            run_dirs = self._list_run_dirs()
            for run_dir in sorted(run_dirs, reverse=True):
                candidate = run_dir / rel
                if self._backup_mode != "mirror":
                    candidate = candidate.with_name(candidate.name + self._backup_suffix)
                found = self._latest_numbered(candidate)
                if found is not None:
                    return found

            if self._backup_mode == "mirror":
                candidate = self._backup_dir / rel
                if candidate.exists():
                    return candidate

        if self._backup_mode != "mirror":
            base = Path(f"{target}{self._backup_suffix}")
            found = self._latest_numbered(base)
            if found is not None:
                return found

        return None

    def _list_run_dirs(self) -> list[Path]:
        if not self._backup_dir.exists():
            return []
        run_dirs: list[Path] = []
        for entry in self._backup_dir.iterdir():
            if not entry.is_dir():
                continue
            run_dirs.append(entry)
        return run_dirs

    def _latest_numbered(self, base: Path) -> Path | None:
        if base.exists():
            return base

        parent = base.parent
        if not parent.exists():
            return None
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
