from __future__ import annotations

from pathlib import Path

from mj_formatter.core.files import BackupManifest, BackupManifestConfig
from mj_formatter.core.types import FileResult


def test_backup_manifest_written(tmp_path: Path) -> None:
    backup_dir = tmp_path / "backups"
    run_id = "20250205_120000"
    root = tmp_path / "root"
    root.mkdir()

    target = root / "file.txt"
    target.write_text("new", encoding="utf-8")
    backup = backup_dir / run_id / "file.txt.bak"
    backup.parent.mkdir(parents=True, exist_ok=True)
    backup.write_text("old", encoding="utf-8")

    results = [
        FileResult(
            path=str(target),
            changed=True,
            violations=[],
            edits=[],
            error=None,
            backup_path=str(backup),
            cache_hit=False,
            profile=None,
        )
    ]

    manifest = BackupManifest(
        BackupManifestConfig(
            backup_dir=str(backup_dir),
            run_id=run_id,
            root=str(root),
            mode="suffix",
            suffix=".bak",
            created_at="2025-02-05T12:00:00+00:00",
        )
    )
    manifest.write(results)

    manifest_path = backup_dir / run_id / "backup_manifest.toml"
    assert manifest_path.exists()
    content = manifest_path.read_text(encoding="utf-8")
    assert "run_id" in content
    assert "format_version" in content
