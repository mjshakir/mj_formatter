from __future__ import annotations

import os
from pathlib import Path
from typing import Any

from ..types import AppConfig
from ..types import PolicyEnforcement
from ..types import ParserStrategy
from ..types import PolicySourceArgs
from .toml_store import TomlStore


class ConfigLoader:
    def __init__(self, toml_loader: TomlStore | None = None) -> None:
        self._toml_loader = toml_loader or TomlStore()

    def load(self, args: Any) -> AppConfig:
        config_path = self._resolve_config_path(args)
        if config_path and not config_path.exists():
            raise FileNotFoundError(f"Config not found: {config_path}")
        data = self._toml_loader.load(config_path) if config_path else {}
        return self._build_config(data, args)

    def _resolve_config_path(self, args: Any) -> Path | None:
        if getattr(args, "config", None):
            return Path(args.config)

        cwd = Path.cwd()
        local = cwd / "config" / "config.toml"
        if local.exists():
            return local

        default_config = Path(__file__).resolve().parents[3] / "config" / "config.toml"
        if default_config.exists():
            return default_config

        return None

    def _resolve_policy_sources(
        self,
        policy_args: PolicySourceArgs,
    ) -> tuple[Path, list[Path], Path | None]:
        args = policy_args.args
        data = policy_args.data
        base_dir = policy_args.base_dir
        policies = data.get("policies", {})
        style_name = getattr(args, "style", None) or policies.get("style")
        policy_dir = policies.get("policy_dir")

        styles_root = Path(__file__).resolve().parents[3] / "styles"
        if style_name:
            style_root = styles_root / str(style_name)
        elif policy_dir:
            style_root = (base_dir / str(policy_dir)).resolve()
        else:
            style_root = styles_root / "default"

        config_dir = style_root / "format"
        if not config_dir.exists():
            raise FileNotFoundError(f"Style config folder not found: {config_dir}")

        policy_files = sorted(config_dir.glob("*.toml"))

        for raw in policies.get("files", []) or []:
            candidate = (base_dir / str(raw)).resolve()
            if not candidate.exists():
                raise FileNotFoundError(f"Policy file not found: {candidate}")
            policy_files.append(candidate)

        enable_file = style_root / "enable" / "enable.toml"
        if not enable_file.exists():
            enable_file = None

        return style_root, policy_files, enable_file

    def _load_enable_file(self, enable_file: Path | None) -> tuple[set[str], set[str]]:
        if enable_file is None:
            return set(), set()
        data = self._toml_loader.load(enable_file)
        enable_block = data.get("enable", {})
        if not isinstance(enable_block, dict):
            return set(), set()
        enabled = set(enable_block.get("enabled", []) or [])
        disabled = set(enable_block.get("disabled", []) or [])
        return enabled, disabled

    def _warn_disabled_default(self, policy_name: str, enable_file: Path | None) -> None:
        import sys

        target = enable_file or Path("styles/<style>/enable/enable.toml")
        msg = (
            f"WARNING: policy '{policy_name}' is disabled by default. "
            f"To enable it, add it to {target} with:\n"
            "[enable]\n"
            f"enabled = [\"{policy_name}\"]\n"
        )
        print(msg, file=sys.stderr)

    def _load_policy_files(self, files: list[Path]) -> dict[str, dict[str, object]]:
        merged: dict[str, dict[str, object]] = {}
        for path in files:
            data = self._toml_loader.load(path)
            policy_block = data.get("policy", {})
            if not isinstance(policy_block, dict):
                continue
            name = policy_block.get("name")
            if not name:
                continue
            name = str(name)
            settings = {k: v for k, v in policy_block.items() if k != "name"}
            if name in merged:
                merged[name].update(settings)
            else:
                merged[name] = settings
        return merged

    def _parse_csv_list(self, items: list[str] | None) -> set[str]:
        if not items:
            return set()
        result: set[str] = set()
        for raw in items:
            for item in raw.split(","):
                cleaned = item.strip()
                if cleaned:
                    result.add(cleaned)
        return result

    def _build_config(self, data: dict[str, Any], args: Any) -> AppConfig:
        formatter = data.get("formatter", {})
        policies = data.get("policies", {})

        base_dir = Path(getattr(args, "config", None) or self._resolve_config_path(args) or Path.cwd()).resolve().parent
        style_root, policy_files, enable_file = self._resolve_policy_sources(
            PolicySourceArgs(args=args, data=data, base_dir=base_dir)
        )
        policy_settings = self._load_policy_files(policy_files)

        inline_policy_settings = data.get("policy", {})
        if isinstance(inline_policy_settings, dict):
            for name, settings in inline_policy_settings.items():
                if not isinstance(settings, dict):
                    continue
                policy_settings.setdefault(name, {}).update(settings)

        enabled, disabled = self._load_enable_file(enable_file)
        for name in list(policy_settings.keys()):
            if name in enabled:
                policy_settings[name]["enabled"] = True
            elif name in disabled:
                policy_settings[name]["enabled"] = False
            else:
                policy_settings[name]["enabled"] = False
                self._warn_disabled_default(name, enable_file)

        root = str(getattr(args, "root", None) or formatter.get("root", "."))

        include_patterns = tuple(
            (getattr(args, "include", None) or [])
            + list(formatter.get("include", []))
        )
        exclude_patterns = tuple(
            (getattr(args, "exclude", None) or [])
            + list(formatter.get("exclude", []))
        )

        arg_jobs = getattr(args, "jobs", None)
        jobs = int(formatter.get("jobs", 0)) if arg_jobs is None else int(arg_jobs)

        check = bool(formatter.get("check", False)) or bool(getattr(args, "check", False))

        backup_override = getattr(args, "backup", None)
        backup = bool(formatter.get("backup", True)) if backup_override is None else bool(backup_override)

        cache_override = getattr(args, "cache", None)
        cache_enabled = bool(formatter.get("cache_enabled", True)) if cache_override is None else bool(cache_override)
        check_result_cache_enabled = bool(formatter.get("check_result_cache_enabled", True))
        check_result_cache_path = str(
            formatter.get("check_result_cache_path", "scripts/mj_formatter/.cache/check_results")
        )
        check_result_cache_l1_size = int(formatter.get("check_result_cache_l1_size", 2048))
        if check_result_cache_l1_size < 64:
            check_result_cache_l1_size = 64

        backup_mode = str(formatter.get("backup_mode", "suffix"))
        backup_suffix = str(formatter.get("backup_suffix", ".bak"))
        backup_dir = str(formatter.get("backup_dir", "scripts/mj_formatter/backups"))

        report_path = str(getattr(args, "report", None) or formatter.get("report_path", "format_report.jsonl"))
        run_journal_dir = str(formatter.get("run_journal_dir", "scripts/mj_formatter/runs"))
        cache_path = str(formatter.get("cache_path", "scripts/mj_formatter/.cache/cache.bin"))
        policy_cache_path = str(
            formatter.get(
                "policy_cache_path",
                str(style_root / "cache" / "policy_cache.bin"),
            )
        )

        log_level = str(getattr(args, "log_level", None) or formatter.get("log_level", "INFO"))
        log_file = str(getattr(args, "log_file", None) or formatter.get("log_file", "")) or None
        async_logging = bool(formatter.get("async_logging", True))
        log_queue_size = int(formatter.get("log_queue_size", 10000))
        if log_queue_size < 100:
            log_queue_size = 100
        shard_merge_workers = int(formatter.get("shard_merge_workers", 2))
        if shard_merge_workers < 1:
            shard_merge_workers = 1
        profile_enabled = bool(getattr(args, "profile", False) or formatter.get("profile", False))
        sort_results = bool(formatter.get("sort_results", True))
        conflict_detection_enabled = bool(formatter.get("conflict_detection_enabled", True))
        conflict_touch_threshold = int(formatter.get("conflict_touch_threshold", 3))
        conflict_fail_on_detected = bool(formatter.get("conflict_fail_on_detected", False))
        # Parser strategy is normalized to hybrid-only runtime.
        parser_strategy = ParserStrategy.HYBRID
        parse_pool_workers = int(
            formatter.get("parse_pool_workers", 2)
            if getattr(args, "parse_pool_workers", None) is None
            else getattr(args, "parse_pool_workers")
        )
        if parse_pool_workers < 1:
            parse_pool_workers = 1
        worker_batch_size = int(formatter.get("worker_batch_size", 2))
        if worker_batch_size < 1:
            worker_batch_size = 1
        worker_batch_prefetch = bool(formatter.get("worker_batch_prefetch", True))
        worker_batch_smart = bool(formatter.get("worker_batch_smart", True))
        autotune_override = getattr(args, "batch_autotune", None)
        worker_batch_autotune_enabled = (
            bool(formatter.get("worker_batch_autotune_enabled", False))
            if autotune_override is None
            else bool(autotune_override)
        )
        worker_batch_autotune_path = str(
            formatter.get("worker_batch_autotune_path", "scripts/mj_formatter/.cache/worker_batch_autotune.json")
        )
        raw_candidates = formatter.get("worker_batch_autotune_candidates", [1, 2, 4, 8]) or [1, 2, 4, 8]
        if not isinstance(raw_candidates, (list, tuple)):
            raw_candidates = [1, 2, 4, 8]
        candidates = []
        for item in raw_candidates:
            try:
                value = int(item)
            except Exception:
                continue
            if value >= 1:
                candidates.append(value)
        if not candidates:
            candidates = [1, 2, 4, 8]
        worker_batch_autotune_candidates = tuple(sorted(set(candidates)))
        worker_batch_autotune_probe_interval = int(formatter.get("worker_batch_autotune_probe_interval", 12))
        if worker_batch_autotune_probe_interval < 1:
            worker_batch_autotune_probe_interval = 1
        worker_batch_autotune_min_files = int(formatter.get("worker_batch_autotune_min_files", 16))
        if worker_batch_autotune_min_files < 1:
            worker_batch_autotune_min_files = 1
        post_edit_check_override = getattr(args, "post_edit_check", None)
        post_edit_check_enabled = (
            bool(formatter.get("post_edit_check_enabled", True))
            if post_edit_check_override is None
            else bool(post_edit_check_override)
        )
        post_edit_retry_enabled = bool(formatter.get("post_edit_retry_enabled", True))
        post_edit_retry_max_attempts = int(formatter.get("post_edit_retry_max_attempts", 6))
        if post_edit_retry_max_attempts < 0:
            post_edit_retry_max_attempts = 0
        post_edit_retry_confidence_step = float(formatter.get("post_edit_retry_confidence_step", 0.05))
        if post_edit_retry_confidence_step < 0.0:
            post_edit_retry_confidence_step = 0.0
        post_edit_retry_confidence_max = float(formatter.get("post_edit_retry_confidence_max", 1.00))
        post_edit_retry_confidence_max = max(0.0, min(1.0, post_edit_retry_confidence_max))
        retry_snapshot_cache_size = int(formatter.get("retry_snapshot_cache_size", 128))
        if retry_snapshot_cache_size < 8:
            retry_snapshot_cache_size = 8
        confidence_blocking_enabled = bool(formatter.get("confidence_blocking_enabled", True))
        confidence_blocking_min = float(formatter.get("confidence_blocking_min", 0.70))
        confidence_blocking_min = max(0.0, min(1.0, confidence_blocking_min))
        confidence_blocking_policies = set(
            str(item).strip()
            for item in (formatter.get("confidence_blocking_policies", ["naming_conventions", "snake_case"]) or [])
            if str(item).strip()
        )
        confidence_default_enforcement = PolicyEnforcement.from_value(
            formatter.get("confidence_default_enforcement", PolicyEnforcement.HARD.value)
        )
        confidence_strict_delta = float(formatter.get("confidence_strict_delta", 0.05))
        confidence_strict_delta = max(0.0, min(1.0, confidence_strict_delta))
        confidence_relaxed_delta = float(formatter.get("confidence_relaxed_delta", 0.10))
        confidence_relaxed_delta = max(0.0, min(1.0, confidence_relaxed_delta))
        confidence_context_bonus_cap = float(formatter.get("confidence_context_bonus_cap", 0.08))
        confidence_context_bonus_cap = max(0.0, min(0.25, confidence_context_bonus_cap))

        clang_args = tuple(formatter.get("clang_args", []) or [])
        clang_library_paths = tuple(str(item) for item in (formatter.get("clang_library_paths", []) or []))
        clang_compdb_path = formatter.get("clang_compdb", None)
        if clang_compdb_path:
            clang_compdb_path = str(clang_compdb_path)
        clang_args_mode = str(formatter.get("clang_args_mode", "merge")).lower()
        policies_default = str(policies.get("default", "all")).lower()
        enabled = set(policies.get("enabled", []))
        disabled = set(policies.get("disabled", []))

        env_enabled = self._parse_csv_list([os.environ.get("MJ_FORMATTER_ENABLE", "")])
        env_disabled = self._parse_csv_list([os.environ.get("MJ_FORMATTER_DISABLE", "")])
        enabled |= env_enabled
        disabled |= env_disabled

        cli_enabled = self._parse_csv_list(getattr(args, "enable", None))
        cli_disabled = self._parse_csv_list(getattr(args, "disable", None))
        enabled |= cli_enabled
        disabled |= cli_disabled

        policies_order = tuple(policies.get("order", []))

        return AppConfig(
            root=root,
            include_patterns=include_patterns,
            exclude_patterns=exclude_patterns,
            jobs=jobs,
            check=check,
            backup=backup,
            backup_mode=backup_mode,
            backup_suffix=backup_suffix,
            backup_dir=backup_dir,
            report_path=report_path,
            run_journal_dir=run_journal_dir,
            cache_enabled=cache_enabled,
            cache_path=cache_path,
            check_result_cache_enabled=check_result_cache_enabled,
            check_result_cache_path=check_result_cache_path,
            check_result_cache_l1_size=check_result_cache_l1_size,
            log_level=log_level,
            log_file=log_file,
            profile_enabled=profile_enabled,
            policy_cache_path=policy_cache_path,
            sort_results=sort_results,
            clang_args=clang_args,
            clang_compdb_path=clang_compdb_path,
            clang_args_mode=clang_args_mode,
            policies_default=policies_default,
            policies_enabled=frozenset(enabled),
            policies_disabled=frozenset(disabled),
            policies_order=policies_order,
            policy_settings=policy_settings,
            async_logging=async_logging,
            log_queue_size=log_queue_size,
            shard_merge_workers=shard_merge_workers,
            conflict_detection_enabled=conflict_detection_enabled,
            conflict_touch_threshold=conflict_touch_threshold,
            conflict_fail_on_detected=conflict_fail_on_detected,
            parser_strategy=parser_strategy,
            clang_library_paths=clang_library_paths,
            parse_pool_workers=parse_pool_workers,
            worker_batch_size=worker_batch_size,
            worker_batch_prefetch=worker_batch_prefetch,
            worker_batch_smart=worker_batch_smart,
            worker_batch_autotune_enabled=worker_batch_autotune_enabled,
            worker_batch_autotune_path=worker_batch_autotune_path,
            worker_batch_autotune_candidates=worker_batch_autotune_candidates,
            worker_batch_autotune_probe_interval=worker_batch_autotune_probe_interval,
            worker_batch_autotune_min_files=worker_batch_autotune_min_files,
            post_edit_check_enabled=post_edit_check_enabled,
            post_edit_retry_enabled=post_edit_retry_enabled,
            post_edit_retry_max_attempts=post_edit_retry_max_attempts,
            post_edit_retry_confidence_step=post_edit_retry_confidence_step,
            post_edit_retry_confidence_max=post_edit_retry_confidence_max,
            retry_snapshot_cache_size=retry_snapshot_cache_size,
            confidence_blocking_enabled=confidence_blocking_enabled,
            confidence_blocking_min=confidence_blocking_min,
            confidence_blocking_policies=frozenset(confidence_blocking_policies),
            confidence_default_enforcement=confidence_default_enforcement,
            confidence_strict_delta=confidence_strict_delta,
            confidence_relaxed_delta=confidence_relaxed_delta,
            confidence_context_bonus_cap=confidence_context_bonus_cap,
        )
