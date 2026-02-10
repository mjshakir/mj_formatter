from __future__ import annotations

import os
from pathlib import Path
from typing import Any
from dataclasses import dataclass

from .app_config import AppConfig


class ConfigLoader:
    def load(self, args: Any) -> AppConfig:
        config_path = self._resolve_config_path(args)
        if config_path and not config_path.exists():
            raise FileNotFoundError(f"Config not found: {config_path}")
        data = self._load_toml(config_path) if config_path else {}
        return self._build_config(data, args)

    def _resolve_config_path(self, args: Any) -> Path | None:
        if getattr(args, "config", None):
            return Path(args.config)

        cwd = Path.cwd()
        local = cwd / "config" / "config.toml"
        if local.exists():
            return local

        default_config = Path(__file__).resolve().parents[2] / "config" / "config.toml"
        if default_config.exists():
            return default_config

        return None

    @dataclass(frozen=True)
    class PolicySourceArgs:
        args: Any
        data: dict[str, Any]
        base_dir: Path

    def _resolve_policy_sources(
        self,
        policy_args: "ConfigLoader.PolicySourceArgs",
    ) -> tuple[Path, list[Path], Path | None]:
        args = policy_args.args
        data = policy_args.data
        base_dir = policy_args.base_dir
        policies = data.get("policies", {})
        style_name = getattr(args, "style", None) or policies.get("style")
        policy_dir = policies.get("policy_dir")

        styles_root = Path(__file__).resolve().parents[2] / "styles"
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
        data = self._load_toml(enable_file)
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
            data = self._load_toml(path)
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

    def _load_toml(self, path: Path) -> dict[str, Any]:
        try:
            import tomllib  # Python 3.11+
        except ModuleNotFoundError:  # pragma: no cover - fallback
            import tomli as tomllib  # type: ignore

        with path.open("rb") as handle:
            return tomllib.load(handle)

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
            ConfigLoader.PolicySourceArgs(args=args, data=data, base_dir=base_dir)
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

        backup_mode = str(formatter.get("backup_mode", "suffix"))
        backup_suffix = str(formatter.get("backup_suffix", ".bak"))
        backup_dir = str(formatter.get("backup_dir", "scripts/mj_formatter/backups"))

        report_path = str(getattr(args, "report", None) or formatter.get("report_path", "format_report.jsonl"))
        cache_path = str(formatter.get("cache_path", "scripts/mj_formatter/.cache/cache.bin"))
        policy_cache_path = str(
            formatter.get(
                "policy_cache_path",
                str(style_root / "cache" / "policy_cache.bin"),
            )
        )

        log_level = str(getattr(args, "log_level", None) or formatter.get("log_level", "INFO"))
        log_file = str(getattr(args, "log_file", None) or formatter.get("log_file", "")) or None
        profile_enabled = bool(getattr(args, "profile", False) or formatter.get("profile", False))
        sort_results = bool(formatter.get("sort_results", True))

        clang_args = tuple(formatter.get("clang_args", []) or [])
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
            cache_enabled=cache_enabled,
            cache_path=cache_path,
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
        )
