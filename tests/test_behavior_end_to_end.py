from __future__ import annotations

import os
import shutil
import time
from pathlib import Path

from mj_formatter.core.backup_manifest import BackupManifest, BackupManifestConfig
from mj_formatter.core.file_result import FileResult
from mj_formatter.core.processor import FileProcessor
from mj_formatter.core.structs import AppConfig, FileIOConfig
from mj_formatter.core.undo_manager import UndoManager


def _make_config(root: Path, backup_dir: Path) -> AppConfig:
    lua_script = Path(__file__).resolve().parents[1] / "styles" / "default" / "lua" / "macro_spacing.lua"
    policy_settings = {
        "lua_macro_spacing": {
            "type": "lua",
            "enabled": True,
            "script": str(lua_script),
            "function": "apply",
            "sandbox": True,
        },
        "brace_style": {"type": "python", "enabled": True, "style": "kr"},
        "function_void_params": {
            "type": "python",
            "enabled": True,
            "require_void": True,
            "no_space_before_paren": True,
        },
        "include_guards": {"type": "python", "enabled": True, "mode": "pragma_once"},
        "line_wrap": {"type": "python", "enabled": True, "max_length": 100},
        "namespace_end_comments": {
            "type": "python",
            "enabled": True,
            "blocks": ["namespace", "class", "struct", "function"],
            "max_named_lines": 40,
        },
        "pointer_bind_style": {"type": "python", "enabled": True, "style": "bind_to_type"},
        "snake_case": {
            "type": "python",
            "enabled": True,
            "apply_to": "both",
            "exclude_class_namespace": True,
        },
        "spacing_style": {"type": "python", "enabled": True, "indent_style": "spaces_4"},
        "class_layout": {"type": "python", "enabled": True},
        "align_assignments": {
            "type": "align_columns",
            "enabled": True,
            "operator": "=",
            "ignore_in": ["for", "if", "while", "switch"],
        },
        "trim_trailing_whitespace": {
            "type": "trim_trailing_whitespace",
            "enabled": True,
        },
        "dash_comment_normalizer": {"type": "python", "enabled": True},
        "section_title_normalizer": {
            "type": "python",
            "enabled": True,
            "mapping": {
                "Standard cpp library": "Standard cpp library",
                "User Defined libraries": "User-defined libraries",
                "Main header": "Main header",
            },
        },
        "include_order": {
            "type": "python",
            "enabled": True,
            "order_header": ["standard", "third_party", "project", "local"],
            "order_source": ["main", "standard", "third_party", "project", "local"],
            "group_titles": {
                "main": "Main header",
                "standard": "Standard Cpp Libraries",
                "third_party": "Third-party headers",
                "project": "Project headers",
                "local": "User Defined Headers",
            },
            "project_headers": [],
            "project_prefixes": [],
            "main_header_extensions": [".hpp", ".h", ".hh", ".hxx"],
            "separator_length": 64,
        },
        "naming_conventions": {"type": "python", "enabled": True, "standard": "mj"},
    }

    return AppConfig(
        root=str(root),
        include_patterns=(),
        exclude_patterns=(),
        jobs=0,
        check=False,
        backup=True,
        backup_mode="suffix",
        backup_suffix=".bak",
        backup_dir=str(backup_dir),
        report_path=str(root / "report.jsonl"),
        cache_enabled=False,
        cache_path=str(root / "cache.bin"),
        log_level="ERROR",
        log_file=None,
        profile_enabled=False,
        policy_cache_path=str(root / "policy_cache.bin"),
        sort_results=True,
        clang_args=(),
        clang_compdb_path=None,
        clang_args_mode="merge",
        policies_default="none",
        policies_enabled=frozenset(),
        policies_disabled=frozenset(),
        policies_order=(
            "trim_trailing_whitespace",
            "dash_comment_normalizer",
            "section_title_normalizer",
            "include_guards",
            "include_order",
            "brace_style",
            "spacing_style",
            "pointer_bind_style",
            "function_void_params",
            "line_wrap",
            "snake_case",
            "naming_conventions",
            "align_assignments",
            "class_layout",
            "namespace_end_comments",
            "lua_macro_spacing",
        ),
        policy_settings=policy_settings,
    )


def _copy_fixtures(tmp_path: Path) -> tuple[Path, Path, dict[str, str]]:
    fixtures = Path(__file__).resolve().parents[1] / "behavior_test"
    input_dir = fixtures / "input"
    expected_dir = fixtures / "expected"
    target_root = tmp_path / "workspace"
    target_root.mkdir()
    for item in input_dir.iterdir():
        shutil.copy2(item, target_root / item.name)
    originals = {
        item.name: (target_root / item.name).read_text(encoding="utf-8")
        for item in input_dir.iterdir()
        if item.is_file()
    }
    return target_root, expected_dir, originals


def test_behavior_end_to_end(tmp_path: Path) -> None:
    root, expected_dir, originals = _copy_fixtures(tmp_path)
    backup_dir = tmp_path / "backups"
    run_id = "20250205_140000"
    old_env = os.environ.get("MJ_FORMATTER_BACKUP_RUN")
    os.environ["MJ_FORMATTER_BACKUP_RUN"] = run_id

    try:
        config = _make_config(root, backup_dir)
        processor = FileProcessor(config)

        names = sorted(originals.keys())
        paths = [str(root / name) for name in names]
        start = time.perf_counter()
        results: list[FileResult] = [processor(path) for path in paths]
        elapsed = time.perf_counter() - start

        assert elapsed < 5.0
        assert all(result.changed for result in results)

        BackupManifest(
            BackupManifestConfig(
                backup_dir=str(backup_dir),
                run_id=run_id,
                root=str(root),
                mode="suffix",
                suffix=".bak",
            )
        ).write(results)

        for name in names:
            rel = Path(name)
            backup_path = backup_dir / run_id / rel
            backup_path = backup_path.with_name(backup_path.name + ".bak")
            assert backup_path.exists()

        for name in names:
            output = (root / name).read_text(encoding="utf-8")
            expected = (expected_dir / name).read_text(encoding="utf-8")
            assert output == expected

        results_second: list[FileResult] = [processor(path) for path in paths]
        assert all(not result.changed for result in results_second)

        undo = UndoManager(
            FileIOConfig(
                root=str(root),
                backup=True,
                backup_mode="suffix",
                backup_suffix=".bak",
                backup_dir=str(backup_dir),
            )
        )
        for name in names:
            target = root / name
            ok, err = undo.restore(target, delete_backup=False)
            assert ok, err
            restored = target.read_text(encoding="utf-8")
            assert restored == originals[name]
    finally:
        if old_env is None:
            os.environ.pop("MJ_FORMATTER_BACKUP_RUN", None)
        else:
            os.environ["MJ_FORMATTER_BACKUP_RUN"] = old_env
