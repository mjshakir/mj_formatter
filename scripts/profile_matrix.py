#!/usr/bin/env python3
from __future__ import annotations

import argparse
import csv
import json
import re
import subprocess
import sys
from pathlib import Path
from tempfile import TemporaryDirectory
from typing import Any

PROJECT_ROOT = Path(__file__).resolve().parents[1]
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

from mj_formatter.core.config import TomlStore
from mj_formatter.core.types import VariantResult, VariantSpec


class ProfileMatrixRunner:
    _SUMMARY_PATTERNS = {
        "files": re.compile(r"files processed:\s*(\d+)"),
        "changed": re.compile(r"files changed:\s*(\d+)"),
        "violations": re.compile(r"violations:\s*(\d+)"),
        "errors": re.compile(r"errors:\s*(\d+)"),
        "cache_hits": re.compile(r"cache hits:\s*(\d+)"),
        "warnings": re.compile(r"warnings:\s*(\d+)"),
        "conflicts": re.compile(r"conflicts:\s*(\d+)"),
        "elapsed_s": re.compile(r"elapsed:\s*([0-9]+(?:\.[0-9]+)?)s"),
        "throughput_files_s": re.compile(r"throughput:\s*([0-9]+(?:\.[0-9]+)?)\s*files/s"),
    }

    def __init__(self, args: argparse.Namespace) -> None:
        self._args = args
        self._config_path = Path(args.config).resolve()
        self._output_dir: Path | None = None
        self._csv_path: Path | None = None
        self._md_path: Path | None = None
        self._python_bin = sys.executable
        self._profile_defaults: dict[str, Any] = {}
        self._toml_loader = TomlStore()

    @classmethod
    def parse_args(cls, argv: list[str] | None = None) -> argparse.Namespace:
        parser = argparse.ArgumentParser(description="Run profiling matrix and emit CSV + Markdown tables.")
        parser.add_argument("--config", default="config/config.toml", help="Path to config TOML")
        parser.add_argument("--root", help="Optional root override for all matrix variants")
        parser.add_argument("--python", dest="python_bin", help="Python executable for invoking formatter")
        parser.add_argument("--output-dir", help="Override output directory for matrix artifacts")
        parser.add_argument("--only", action="append", help="Run only matching matrix variant name(s)")
        parser.add_argument("--stop-on-error", action="store_true", help="Stop after first non-zero/non-one exit")
        return parser.parse_args(argv)

    @staticmethod
    def _latest_match(pattern: re.Pattern[str], text: str) -> str | None:
        values = pattern.findall(text)
        if not values:
            return None
        return values[-1]

    def _extract_summary(self, log_text: str) -> dict[str, float | int]:
        summary: dict[str, float | int] = {
            "files": 0,
            "changed": 0,
            "violations": 0,
            "errors": 0,
            "conflicts": 0,
            "warnings": 0,
            "cache_hits": 0,
            "elapsed_s": 0.0,
            "throughput_files_s": 0.0,
        }
        for key, pattern in self._SUMMARY_PATTERNS.items():
            match = self._latest_match(pattern, log_text)
            if match is None:
                continue
            if key in {"elapsed_s", "throughput_files_s"}:
                summary[key] = float(match)
            else:
                summary[key] = int(match)
        return summary

    @staticmethod
    def _format_parse_modes(parse_modes: dict[str, Any]) -> str:
        if not parse_modes:
            return ""
        pairs = []
        for key in sorted(parse_modes):
            value = parse_modes[key]
            pairs.append(f"{key}:{value}")
        return ", ".join(pairs)

    @staticmethod
    def _markdown_escape(value: str) -> str:
        return value.replace("|", "\\|").replace("\n", " ")

    def _build_spec(self, entry: dict[str, Any], global_root: str | None) -> VariantSpec:
        if not isinstance(entry, dict):
            raise ValueError(f"Invalid matrix entry: {entry!r}")

        nested_formatter = entry.get("formatter", {})
        if nested_formatter is None:
            nested_formatter = {}
        if not isinstance(nested_formatter, dict):
            raise ValueError(f"'formatter' must be a table in matrix entry: {entry!r}")

        def get_override(key: str, default: Any = None) -> Any:
            if key in entry:
                return entry.get(key)
            if key in nested_formatter:
                return nested_formatter.get(key)
            return default

        name = str(entry.get("name", "")).strip()
        if not name:
            raise ValueError(f"Matrix entry is missing required 'name': {entry!r}")

        description = str(entry.get("description", "")).strip()
        extra_cli_args = tuple(str(item) for item in (entry.get("cli_args", []) or []))

        root = (
            global_root
            or str(entry.get("root", "") or self._profile_defaults.get("root", "")).strip()
            or None
        )

        parser_strategy = get_override("parser_strategy")
        parser_strategy = str(parser_strategy).strip() if parser_strategy is not None else None
        parse_pool_workers = get_override("parse_pool_workers")
        parse_pool_workers = int(parse_pool_workers) if parse_pool_workers is not None else None
        post_edit_check_enabled = get_override("post_edit_check_enabled")
        if post_edit_check_enabled is not None:
            post_edit_check_enabled = bool(post_edit_check_enabled)
        jobs = get_override("jobs")
        jobs = int(jobs) if jobs is not None else None

        default_check = bool(self._profile_defaults.get("check", True))
        default_profile = bool(self._profile_defaults.get("profile", True))
        default_cache_enabled = bool(self._profile_defaults.get("cache", False))
        check = bool(get_override("check", default_check))
        profile = bool(get_override("profile", default_profile))
        cache_enabled = bool(get_override("cache_enabled", default_cache_enabled))

        return VariantSpec(
            name=name,
            description=description,
            parser_strategy=parser_strategy,
            parse_pool_workers=parse_pool_workers,
            post_edit_check_enabled=post_edit_check_enabled,
            jobs=jobs,
            root=root,
            check=check,
            profile=profile,
            cache_enabled=cache_enabled,
            extra_cli_args=extra_cli_args,
        )

    def _build_command(self, spec: VariantSpec, report_path: Path) -> list[str]:
        cmd = [
            self._python_bin,
            "-m",
            "mj_formatter.main",
            "--config",
            str(self._config_path),
            "--report",
            str(report_path),
        ]

        if spec.root:
            cmd.extend(["--root", spec.root])
        if spec.check:
            cmd.append("--check")
        if spec.profile:
            cmd.append("--profile")

        cmd.append("--cache" if spec.cache_enabled else "--no-cache")

        if spec.jobs is not None:
            cmd.extend(["--jobs", str(spec.jobs)])
        if spec.parser_strategy:
            cmd.extend(["--parser-strategy", spec.parser_strategy])
        if spec.parse_pool_workers is not None:
            cmd.extend(["--parse-pool-workers", str(spec.parse_pool_workers)])
        if spec.post_edit_check_enabled is not None:
            cmd.append("--post-edit-check" if spec.post_edit_check_enabled else "--no-post-edit-check")

        if spec.extra_cli_args:
            cmd.extend(spec.extra_cli_args)
        return cmd

    @staticmethod
    def _read_metrics(path: Path) -> dict[str, Any]:
        if not path.exists():
            return {}
        try:
            with path.open("r", encoding="utf-8") as handle:
                data = json.load(handle)
            if isinstance(data, dict):
                return data
        except Exception:
            return {}
        return {}

    def _write_csv(self, path: Path, rows: list[VariantResult]) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("w", encoding="utf-8", newline="") as handle:
            writer = csv.writer(handle)
            writer.writerow(
                [
                    "variant",
                    "description",
                    "parser_strategy",
                    "parse_pool_workers",
                    "post_edit_check_enabled",
                    "jobs",
                    "files",
                    "changed",
                    "violations",
                    "errors",
                    "conflicts",
                    "warnings",
                    "cache_hits",
                    "elapsed_s",
                    "throughput_files_s",
                    "parse_modes",
                    "exit_code",
                    "command",
                ]
            )
            for item in rows:
                writer.writerow(
                    [
                        item.name,
                        item.description,
                        item.parser_strategy,
                        item.parse_pool_workers,
                        item.post_edit_check_enabled,
                        item.jobs if item.jobs is not None else "",
                        item.files,
                        item.changed,
                        item.violations,
                        item.errors,
                        item.conflicts,
                        item.warnings,
                        item.cache_hits,
                        f"{item.elapsed_s:.3f}",
                        f"{item.throughput_files_s:.2f}",
                        item.parse_modes,
                        item.exit_code,
                        item.command,
                    ]
                )

    def _write_markdown(self, path: Path, rows: list[VariantResult]) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        headers = [
            "Variant",
            "Strategy",
            "ParsePool",
            "PostCheck",
            "Files",
            "Violations",
            "Conflicts",
            "Elapsed(s)",
            "Throughput",
            "Parse Modes",
            "Exit",
        ]
        lines = [
            "| " + " | ".join(headers) + " |",
            "| " + " | ".join(["---"] * len(headers)) + " |",
        ]
        for row in rows:
            lines.append(
                "| "
                + " | ".join(
                    [
                        self._markdown_escape(row.name),
                        self._markdown_escape(row.parser_strategy),
                        str(row.parse_pool_workers),
                        "on" if row.post_edit_check_enabled else "off",
                        str(row.files),
                        str(row.violations),
                        str(row.conflicts),
                        f"{row.elapsed_s:.3f}",
                        f"{row.throughput_files_s:.2f}",
                        self._markdown_escape(row.parse_modes),
                        str(row.exit_code),
                    ]
                )
                + " |"
            )
        with path.open("w", encoding="utf-8") as handle:
            handle.write("\n".join(lines))
            handle.write("\n")

    def _load_runtime(self) -> tuple[list[VariantSpec], int]:
        if not self._config_path.exists():
            print(f"config not found: {self._config_path}", file=sys.stderr)
            return [], 2

        config_data = self._toml_loader.load(self._config_path)
        profiling = config_data.get("profiling", {})
        if not isinstance(profiling, dict):
            print(f"[profiling] section must be a table in {self._config_path}", file=sys.stderr)
            return [], 2

        matrix_entries = profiling.get("matrix", [])
        if not isinstance(matrix_entries, list) or not matrix_entries:
            print(f"no profiling matrix found in {self._config_path} ([profiling.matrix])", file=sys.stderr)
            return [], 2

        only = {item.strip() for item in (self._args.only or []) if item.strip()}
        self._profile_defaults = dict(profiling)
        self._python_bin = str(self._args.python_bin or profiling.get("python_bin") or sys.executable)

        self._output_dir = Path(
            self._args.output_dir or profiling.get("output_dir") or "scripts/mj_formatter/profile_matrix"
        ).resolve()
        self._csv_path = self._output_dir / "profile_matrix.csv"
        self._md_path = self._output_dir / "profile_matrix.md"
        self._output_dir.mkdir(parents=True, exist_ok=True)

        specs: list[VariantSpec] = []
        for raw in matrix_entries:
            if not isinstance(raw, dict):
                continue
            spec = self._build_spec(raw, self._args.root)
            if only and spec.name not in only:
                continue
            specs.append(spec)
        if not specs:
            print("no matrix variants selected", file=sys.stderr)
            return [], 2
        return specs, 0

    def _run_variant(self, spec: VariantSpec, temp_reports: Path) -> VariantResult:
        report_path = temp_reports / f"{spec.name}.jsonl"
        metrics_path = Path(f"{report_path}.metrics.json")
        cmd = self._build_command(spec, report_path)
        print(f"[profile-matrix] running: {spec.name}")
        run = subprocess.run(cmd, text=True, capture_output=True)
        combined = (run.stdout or "") + ("\n" if run.stdout and run.stderr else "") + (run.stderr or "")

        summary = self._extract_summary(combined)
        metrics = self._read_metrics(metrics_path)
        metrics_summary = metrics.get("summary", {}) if isinstance(metrics, dict) else {}
        metrics_parse_modes = metrics.get("parse_modes", {}) if isinstance(metrics, dict) else {}
        if not isinstance(metrics_summary, dict):
            metrics_summary = {}
        if not isinstance(metrics_parse_modes, dict):
            metrics_parse_modes = {}

        files = int(summary["files"] or metrics_summary.get("files", 0))
        changed = int(summary["changed"] or metrics_summary.get("changed", 0))
        violations = int(summary["violations"] or metrics_summary.get("violations", 0))
        errors = int(summary["errors"] or metrics_summary.get("errors", 0))
        conflicts = int(summary["conflicts"])
        warnings = int(summary["warnings"] or metrics_summary.get("warnings", 0))
        cache_hits = int(summary["cache_hits"] or metrics_summary.get("cache_hits", 0))
        elapsed_s = float(summary["elapsed_s"] or (float(metrics_summary.get("duration_ms", 0.0)) / 1000.0))
        throughput_files_s = float(summary["throughput_files_s"] or ((files / elapsed_s) if elapsed_s > 0 else 0.0))

        result = VariantResult(
            name=spec.name,
            description=spec.description,
            parser_strategy=spec.parser_strategy or "default",
            parse_pool_workers=spec.parse_pool_workers or 0,
            post_edit_check_enabled=True if spec.post_edit_check_enabled is None else spec.post_edit_check_enabled,
            jobs=spec.jobs,
            files=files,
            changed=changed,
            violations=violations,
            errors=errors,
            conflicts=conflicts,
            warnings=warnings,
            cache_hits=cache_hits,
            elapsed_s=elapsed_s,
            throughput_files_s=throughput_files_s,
            parse_modes=self._format_parse_modes(metrics_parse_modes),
            exit_code=run.returncode,
            command=" ".join(cmd),
        )
        print(
            f"[profile-matrix] {spec.name}: exit={result.exit_code} "
            f"elapsed={result.elapsed_s:.3f}s files={result.files} violations={result.violations}"
        )
        return result

    def run(self) -> int:
        specs, status = self._load_runtime()
        if status != 0:
            return status
        if self._csv_path is None or self._md_path is None:
            return 2

        results: list[VariantResult] = []
        with TemporaryDirectory(prefix="mj_formatter_profile_matrix_") as temp_dir:
            temp_reports = Path(temp_dir)
            for spec in specs:
                result = self._run_variant(spec, temp_reports)
                results.append(result)
                if self._args.stop_on_error and result.exit_code not in (0, 1):
                    print(f"[profile-matrix] stopping after failing variant: {spec.name}", file=sys.stderr)
                    break

        self._write_csv(self._csv_path, results)
        self._write_markdown(self._md_path, results)
        print(f"[profile-matrix] csv: {self._csv_path}")
        print(f"[profile-matrix] markdown: {self._md_path}")
        return 0


def main(argv: list[str] | None = None) -> int:
    return ProfileMatrixRunner(ProfileMatrixRunner.parse_args(argv)).run()


if __name__ == "__main__":
    raise SystemExit(main())
