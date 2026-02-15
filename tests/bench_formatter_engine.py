from __future__ import annotations

from mj_formatter.core.processing import FormatterEngine
from mj_formatter.core.types import AppConfig


def _make_config() -> AppConfig:
    return AppConfig(
        root=".",
        include_patterns=(),
        exclude_patterns=(),
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
        policy_settings={
            "align_assignments": {
                "enabled": True,
                "type": "align_columns",
                "touch_contract": "code_only",
                "operator": "=",
                "ignore_in": ["for", "if", "while", "switch"],
                "non_assignment_patterns": [
                    "\\)\\s*=\\s*(?:delete|default)\\s*;",
                    "\\)\\s*=\\s*0\\s*;",
                    "^\\s*template\\s*<",
                ],
            }
        },
    )


def _make_text(lines: int = 2000) -> str:
    rows = []
    for i in range(lines):
        rows.append(f"m_value{i} = other.m_value{i};\n")
    return "".join(rows)


def test_benchmark_align_assignments(benchmark) -> None:
    config = _make_config()
    engine = FormatterEngine(config)
    text = _make_text()
    benchmark(engine.apply, text, "bench.cpp")
