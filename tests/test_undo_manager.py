from __future__ import annotations

from pathlib import Path

from mj_formatter.core.files import UndoManager
from mj_formatter.core.types import FileIOConfig


def test_undo_manager_uses_manifest(tmp_path: Path) -> None:
    root = tmp_path / "root"
    root.mkdir()
    target = root / "src" / "file.cpp"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text("NEW", encoding="utf-8")

    backup_dir = tmp_path / "backups"
    run_id = "20250205_130000"
    run_dir = backup_dir / run_id
    run_dir.mkdir(parents=True, exist_ok=True)
    backup = run_dir / "src" / "file.cpp.bak"
    backup.parent.mkdir(parents=True, exist_ok=True)
    backup.write_text("OLD", encoding="utf-8")

    manifest = run_dir / "backup_manifest.toml"
    manifest.write_text(
        "\n".join(
            [
                "[meta]",
                "format_version = 1",
                'run_id = "20250205_130000"',
                'created_at = "2025-02-05T13:00:00+00:00"',
                f'root = "{root}"',
                f'backup_dir = "{backup_dir}"',
                'mode = "suffix"',
                'suffix = ".bak"',
                "files = 1",
                "",
                "[[files]]",
                f'source = "{target}"',
                f'backup = "{backup}"',
                'relative_path = "src/file.cpp"',
                "size = 3",
                "mtime_ns = 0",
                'mtime_iso = "2025-02-05T13:00:00+00:00"',
                "",
            ]
        ),
        encoding="utf-8",
    )

    config = FileIOConfig(
        root=str(root),
        backup=True,
        backup_mode="suffix",
        backup_suffix=".bak",
        backup_dir=str(backup_dir),
    )
    manager = UndoManager(config)
    ok, err = manager.restore(target, delete_backup=False)
    assert ok, err
    assert target.read_text(encoding="utf-8") == "OLD"
