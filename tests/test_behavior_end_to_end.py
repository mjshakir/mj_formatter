from __future__ import annotations

import os
import shutil
import time
from pathlib import Path
import pytest

from mj_formatter.core.files import BackupManifest, BackupManifestConfig
from mj_formatter.core.types import FileResult
from mj_formatter.core.parsing import ParserManager
from mj_formatter.core.processing import FileProcessor
from mj_formatter.core.types import AppConfig, FileIOConfig
from mj_formatter.core.files import UndoManager


def _confidence_profile() -> dict[str, object]:
    return {
        "low_score_downgrade_threshold": 0.45,
        "high_score_upgrade_threshold": 0.90,
        "high_consensus_upgrade_threshold": 0.90,
        "low_consensus_downgrade_threshold": 0.60,
        "project_consensus_upgrade_threshold": 0.92,
        "project_score_upgrade_threshold": 0.82,
        "reason_low_consensus_threshold": 0.60,
        "risk_limit_hard": 0.15,
        "risk_limit_soft": 0.30,
        "risk_limit_advisory": 0.95,
        "risk_limit_floor": 0.05,
        "delta_risk_penalty_cap": 0.15,
        "retry_risk_penalty": 0.05,
        "line_confidence_default": 0.50,
        "line_index_weight": 0.65,
        "line_block_weight": 0.35,
        "line_neighbor_deltas": [1, 2],
        "line_neighbor_decay": 0.15,
        "block_confidence_default": 0.70,
        "context_bonus_clang": 0.02,
        "context_refs_threshold_low": 32,
        "context_bonus_refs_low": 0.02,
        "context_refs_threshold_high": 128,
        "context_bonus_refs_high": 0.02,
        "context_scope_threshold_low": 0.85,
        "context_bonus_scope_low": 0.02,
        "context_scope_threshold_high": 0.95,
        "context_bonus_scope_high": 0.02,
        "context_hybrid_threshold": 0.85,
        "context_bonus_hybrid": 0.01,
        "context_coverage_threshold": 0.80,
        "context_bonus_coverage": 0.01,
    }


def _make_config(root: Path, backup_dir: Path) -> AppConfig:
    lua_script = Path(__file__).resolve().parents[1] / "styles" / "default" / "lua" / "macro_spacing.lua"
    policy_settings = {
        "lua_macro_spacing": {
            "type": "lua",
            "enabled": True,
            "touch_contract": "preprocessor_only",
            "script": str(lua_script),
            "function": "apply",
            "sandbox": True,
        },
        "brace_style": {"type": "python", "enabled": True, "touch_contract": "code_only", "style": "kr"},
        "function_void_params": {
            "type": "python",
            "enabled": True,
            "touch_contract": "code_only",
            "require_void": True,
            "no_space_before_paren": True,
            "prefer_clang": True,
            "use_tree_sitter": True,
        },
        "include_guards": {
            "type": "python",
            "enabled": True,
            "touch_contract": "preprocessor_only",
            "mode": "pragma_once",
            "header_extensions": [".h", ".hpp", ".hh", ".hxx"],
        },
        "line_wrap": {
            "type": "python",
            "enabled": True,
            "touch_contract": "code_only",
            "max_length": 100,
            "use_editorconfig": True,
            "wrap_style": "smart",
            "allow_inline_prefix_args": True,
            "align_to_open_paren": True,
            "continuation_indent": 4,
            "tab_width": 4,
            "wrap_calls": True,
            "wrap_function_declarations": False,
            "skip_declaration_expressions": True,
        },
        "namespace_end_comments": {
            "type": "python",
            "enabled": True,
            "touch_contract": "whitespace_only",
            "blocks": ["namespace", "class", "struct", "function"],
            "control_block_kinds": ["if", "while", "for", "switch", "catch"],
            "max_named_lines": 40,
            "max_label_length": 48,
            "replace_existing": True,
        },
        "pointer_bind_style": {
            "type": "python",
            "enabled": True,
            "touch_contract": "code_only",
            "style": "bind_to_type",
            "confidence_profile": _confidence_profile(),
        },
        "snake_case": {
            "type": "python",
            "enabled": True,
            "touch_contract": "code_only",
            "apply_to": "both",
            "exclude_class_namespace": True,
            "prefer_clang": True,
            "use_tree_sitter": True,
            "confidence_profile": _confidence_profile(),
        },
        "spacing_style": {
            "type": "python",
            "enabled": True,
            "touch_contract": "whitespace_only",
            "indent_style": "spaces_4",
            "tab_width": 4,
            "use_editorconfig": True,
        },
        "class_layout": {
            "type": "python",
            "enabled": True,
            "touch_contract": "code_only",
            "source_extensions": [".cpp", ".cc", ".cxx"],
            "header_extensions": [".hpp", ".h", ".hh", ".hxx"],
            "header_search_roots": [".", "include"],
        },
        "align_assignments": {
            "type": "align_columns",
            "enabled": True,
            "touch_contract": "code_only",
            "operator": "=",
            "ignore_in": ["for", "if", "while", "switch"],
            "non_assignment_patterns": ["\\)\\s*=\\s*(?:delete|default)\\s*;", "\\)\\s*=\\s*0\\s*;", "^\\s*template\\s*<"],
        },
        "trim_trailing_whitespace": {
            "type": "trim_trailing_whitespace",
            "enabled": True,
            "touch_contract": "whitespace_only",
        },
        "dash_comment_normalizer": {"type": "python", "enabled": True, "touch_contract": "whitespace_only"},
        "section_title_normalizer": {
            "type": "python",
            "enabled": True,
            "touch_contract": "whitespace_only",
            "mapping": {
                "Standard cpp library": "Standard cpp library",
                "User Defined libraries": "User-defined libraries",
                "Main header": "Main header",
            },
        },
        "include_order": {
            "type": "python",
            "enabled": True,
            "touch_contract": "preprocessor_only",
            "order_header": ["standard", "third_party", "project", "local"],
            "order_source": ["main", "standard", "third_party", "project", "local"],
            "standard_headers": [],
            "standard_prefixes": [],
            "group_titles": {
                "main": "Main header",
                "standard": "Standard Cpp Libraries",
                "third_party": "Third-party headers",
                "project": "Project headers",
                "local": "User Defined Headers",
            },
            "third_party_labels": {},
            "project_headers": [],
            "project_prefixes": [],
            "main_header_extensions": [".hpp", ".h", ".hh", ".hxx"],
            "standard_header_path_markers": ["/include/c++/", "/c++/v1/", "/include/bits/"],
            "clang_builtin_include_prefix": "/lib/clang/",
            "include_path_segment": "/include/",
            "separator_length": 64,
        },
        "naming_conventions": {
            "type": "python",
            "enabled": True,
            "touch_contract": "code_only",
            "standard": "mj",
            "standards": {
                "mj": {
                    "local_prefix": "_",
                    "member_prefix": "m_",
                    "global_prefix": "g_",
                    "static_prefix": "s_",
                    "const_prefix": "c_",
                    "atomic_prefix": "a_",
                    "pointer_prefix": "p_",
                    "shared_ptr_prefix": "sp_",
                    "unique_ptr_prefix": "up_",
                    "weak_ptr_prefix": "wp_",
                    "constexpr_prefix_upper": "C_",
                    "static_prefix_upper": "S_",
                    "function_case": "snake",
                    "type_case": "camel",
                    "namespace_case": "camel",
                    "macro_case": "upper_snake",
                    "constexpr_case": "upper_snake",
                }
            },
            "prefer_clang_semantic": True,
            "use_tree_sitter": True,
            "use_semantic_rename": False,
            "parser_consensus_mode": "advisory",
            "parser_consensus_min": 0.70,
            "confidence_profile": _confidence_profile(),
        },
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
    source_exts = {".h", ".hh", ".hpp", ".hxx", ".c", ".cc", ".cpp", ".cxx"}
    files = [item for item in input_dir.iterdir() if item.is_file() and item.suffix.lower() in source_exts]
    for item in files:
        shutil.copy2(item, target_root / item.name)
    originals = {
        item.name: (target_root / item.name).read_text(encoding="utf-8")
        for item in files
    }
    return target_root, expected_dir, originals


def test_behavior_end_to_end(tmp_path: Path) -> None:
    parser_manager = ParserManager()
    tree, _, tree_warning = parser_manager.parse_tree_sitter("int f() { return 0; }\n", "sample.cpp")
    if tree is None:
        pytest.skip(f"behavior test requires tree-sitter parser support: {tree_warning}")

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

        assert elapsed < 90.0
        assert any(result.changed for result in results)

        BackupManifest(
            BackupManifestConfig(
                backup_dir=str(backup_dir),
                run_id=run_id,
                root=str(root),
                mode="suffix",
                suffix=".bak",
            )
        ).write(results)

        result_by_name = {Path(result.path).name: result for result in results}
        for name in names:
            if not result_by_name[name].changed:
                continue
            rel = Path(name)
            backup_path = backup_dir / run_id / rel
            backup_path = backup_path.with_name(backup_path.name + ".bak")
            assert backup_path.exists()

        for name in names:
            output = (root / name).read_text(encoding="utf-8")
            expected = (expected_dir / name).read_text(encoding="utf-8")
            assert output == expected

        # Run additional passes to ensure iterative formatting continues without runtime errors.
        results_second: list[FileResult] = [processor(path) for path in paths]
        results_third: list[FileResult] = [processor(path) for path in paths]
        assert all(result.error is None for result in results_second)
        assert all(result.error is None for result in results_third)

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
            if not result_by_name[name].changed:
                restored = target.read_text(encoding="utf-8")
                assert restored == originals[name]
                continue
            ok, err = undo.restore(target, delete_backup=False)
            assert ok, err
            restored = target.read_text(encoding="utf-8")
            assert restored == originals[name]
    finally:
        if old_env is None:
            os.environ.pop("MJ_FORMATTER_BACKUP_RUN", None)
        else:
            os.environ["MJ_FORMATTER_BACKUP_RUN"] = old_env
