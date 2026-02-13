from __future__ import annotations

from mj_formatter.core.files import FileFinder
from mj_formatter.core.types import AppConfig


def _make_config(root: str) -> AppConfig:
    return AppConfig(
        root=root,
        include_patterns=("**/*.hpp",),
        exclude_patterns=("exclude/**",),
        jobs=0,
        check=False,
        backup=False,
        backup_mode="suffix",
        backup_suffix=".bak",
        backup_dir="backups",
        report_path="report.jsonl",
        cache_enabled=False,
        cache_path="cache.bin",
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


def test_file_finder_respects_exclude(tmp_path) -> None:
    include_dir = tmp_path / "include"
    include_dir.mkdir()
    (include_dir / "a.hpp").write_text("// a\n", encoding="utf-8")

    exclude_dir = tmp_path / "exclude"
    exclude_dir.mkdir()
    (exclude_dir / "b.hpp").write_text("// b\n", encoding="utf-8")

    config = _make_config(str(tmp_path))
    finder = FileFinder(config)
    files = finder.collect()

    assert any(path.endswith("include/a.hpp") for path in files)
    assert not any(path.endswith("exclude/b.hpp") for path in files)
