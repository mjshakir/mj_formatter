from __future__ import annotations

import os
import shutil
from pathlib import Path
from dataclasses import dataclass
from typing import Any

from .structs import FileIOConfig


class UndoManager:
    def __init__(self, config: FileIOConfig) -> None:
        self._root = Path(config.root).resolve()
        self._backup_mode = config.backup_mode
        self._backup_suffix = config.backup_suffix
        self._backup_dir = Path(config.backup_dir)
        self._manifest_cache: dict[Path, dict[str, str]] = {}
        self._manifest_meta: dict[Path, str] = {}

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
                self._prune_empty_parents(backup)
            except Exception as exc:
                return False, f"restore ok, but failed to delete backup: {exc}"

        return True, None

    def _prune_empty_parents(self, path: Path) -> None:
        try:
            current = path.parent
            backup_root = self._backup_dir.resolve()
            while current != backup_root and backup_root in current.parents:
                try:
                    current.rmdir()
                except OSError:
                    break
                current = current.parent
        except Exception:
            return

    def _find_latest_backup(self, target: Path) -> Path | None:
        try:
            rel = target.resolve().relative_to(self._root)
        except Exception:
            rel = None

        if rel is not None:
            run_dirs = self._list_run_dirs()
            for run_dir in run_dirs:
                manifest = self._load_manifest(run_dir)
                if manifest is not None:
                    rel_key = str(rel)
                    backup_path = manifest.get(rel_key)
                    if backup_path:
                        candidate = Path(backup_path)
                        if candidate.exists():
                            return candidate
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
        return self._sort_run_dirs(run_dirs)

    def _sort_run_dirs(self, run_dirs: list[Path]) -> list[Path]:
        def key(path: Path) -> str:
            return self._manifest_meta.get(path, path.name)

        for run_dir in run_dirs:
            self._load_manifest(run_dir)
        return sorted(run_dirs, key=key, reverse=True)

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

    def _load_manifest(self, run_dir: Path) -> dict[str, str] | None:
        if run_dir in self._manifest_cache:
            return self._manifest_cache[run_dir]

        manifest_path = run_dir / "backup_manifest.toml"
        if not manifest_path.exists():
            return None

        try:
            data = self._load_toml(manifest_path)
        except Exception:
            return None

        meta = data.get("meta", {})
        created_at = ""
        if isinstance(meta, dict):
            created_at = str(meta.get("created_at", ""))
        if created_at:
            self._manifest_meta[run_dir] = created_at

        mapping: dict[str, str] = {}
        files = data.get("files", [])
        if isinstance(files, list):
            for entry in files:
                if not isinstance(entry, dict):
                    continue
                rel = entry.get("relative_path")
                backup = entry.get("backup")
                if isinstance(rel, str) and isinstance(backup, str):
                    mapping[rel] = backup
        self._manifest_cache[run_dir] = mapping
        return mapping

    def _load_toml(self, path: Path) -> dict[str, Any]:
        try:
            import tomllib  # Python 3.11+
        except ModuleNotFoundError:  # pragma: no cover - fallback
            import tomli as tomllib  # type: ignore

        with path.open("rb") as handle:
            return tomllib.load(handle)

    @dataclass(frozen=True)
    class CollectTargetsArgs:
        root: Path
        include: tuple[str, ...]
        exclude: tuple[str, ...]

    def collect_targets(self, args: "UndoManager.CollectTargetsArgs") -> list[Path]:
        from .file_finder import FileFinder
        from .structs import AppConfig

        config = AppConfig(
            root=str(args.root),
            include_patterns=args.include,
            exclude_patterns=args.exclude,
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
            profile_enabled=False,
            policy_cache_path="policy_cache.bin",
            sort_results=True,
            clang_args=(),
            clang_compdb_path=None,
            clang_args_mode="merge",
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
