from __future__ import annotations

from mj_formatter.core.policy_factory import PolicyFactory
from mj_formatter.core.structs import AppConfig
from mj_formatter.policies.align_assignments_policy import AlignAssignmentsPolicy


def _make_config(policy_settings: dict[str, dict[str, object]]) -> AppConfig:
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
        policy_settings=policy_settings,
    )


def test_policy_factory_creates_declarative_policy() -> None:
    config = _make_config(
        {
            "align_assignments": {
                "type": "align_columns",
                "enabled": True,
                "operator": "=",
            }
        }
    )
    factory = PolicyFactory(config)
    policy = factory.create("align_assignments", config.policy_settings["align_assignments"])
    assert isinstance(policy, AlignAssignmentsPolicy)
    assert policy.name == "align_assignments"
