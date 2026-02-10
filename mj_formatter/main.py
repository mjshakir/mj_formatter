from __future__ import annotations

import argparse
import logging
import os
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from concurrent.futures import ProcessPoolExecutor
from multiprocessing import get_context
from typing import Iterable
from dataclasses import dataclass

from .core.app_config import AppConfig
from .core.config_loader import ConfigLoader
from .core.backup_manifest import BackupManifest, BackupManifestConfig
from .core.file_cache import FileCache
from .core.file_finder import FileFinder
from .core.file_result import FileResult
from .core.log_setup import LogSetup
from .core.policy_selector import PolicySelector
from .core.table_printer import TablePrinter
from .core.processor import FileProcessor
from .core.report_writer import ReportWriter
from .core.undo_manager import UndoManager
from .core.policy_factory import PolicyFactory
from .policies.registry import PolicyRegistry
from .core.structs import FileIOConfig, TableData, TableStyle
from .core.metrics import MetricsConfig, MetricsProcess, MetricsClient


def _parse_args(argv: list[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="MJ Formatter")
    parser.add_argument("--config", help="Path to config TOML")
    parser.add_argument("--style", help="Style pack name (folder under styles/)")
    parser.add_argument("--root", help="Root directory for file discovery")
    parser.add_argument("--include", action="append", help="Include glob (repeatable)")
    parser.add_argument("--exclude", action="append", help="Exclude glob (repeatable)")
    parser.add_argument("--enable", action="append", help="Enable policy (comma-separated)")
    parser.add_argument("--disable", action="append", help="Disable policy (comma-separated)")
    parser.add_argument("--jobs", type=int, help="Number of worker processes (0=auto)")
    parser.add_argument("--check", action="store_true", help="Check only, do not write")
    parser.add_argument("--report", help="Path to JSONL report")
    parser.add_argument("--log-level", help="Log level")
    parser.add_argument("--log-file", help="Log file path")
    parser.add_argument("--verbose", action="store_true", help="Print per-file violations")
    parser.add_argument("--profile", action="store_true", help="Profile policy timings")
    parser.add_argument(
        "--backup",
        action=argparse.BooleanOptionalAction,
        default=None,
        help="Enable/disable backup",
    )
    parser.add_argument(
        "--cache",
        action=argparse.BooleanOptionalAction,
        default=None,
        help="Enable/disable cache",
    )
    parser.add_argument("--list-styles", action="store_true", help="List styles and exit")
    parser.add_argument("--list-policies", action="store_true", help="List policies and exit")
    parser.add_argument("--validate-registry", action="store_true", help="Validate policy registry and exit")
    parser.add_argument("--undo", action="store_true", help="Restore from latest backup and delete it")
    parser.add_argument("--undo-no-delete", action="store_true", help="Restore from latest backup without deleting")
    return parser.parse_args(argv)


_WORKER_PROCESSOR: FileProcessor | None = None
_WORKER_METRICS: MetricsClient | None = None


@dataclass(frozen=True)
class WorkerRunConfig:
    config: AppConfig
    jobs: int
    metrics: MetricsClient | None


@dataclass(frozen=True)
class SummaryContext:
    results: Iterable[FileResult]
    check_only: bool
    verbose: bool
    elapsed: float
    jobs: int


def _cpu_count() -> int:
    if hasattr(os, "sched_getaffinity"):
        try:
            return len(os.sched_getaffinity(0))
        except Exception:
            pass
    return os.cpu_count() or 1


def _ensure_backup_run_id(config: AppConfig) -> str | None:
    if not config.backup or config.check:
        return None
    run_id = os.environ.get("MJ_FORMATTER_BACKUP_RUN")
    if run_id:
        return run_id
    run_id = datetime.now(timezone.utc).strftime("%Y%m%d_%H%M%S")
    os.environ["MJ_FORMATTER_BACKUP_RUN"] = run_id
    return run_id


def _get_mp_context():
    if sys.platform.startswith("linux"):
        try:
            return get_context("fork")
        except ValueError:
            pass
    return get_context("spawn")


def _init_worker(config: AppConfig, metrics_queue) -> None:
    global _WORKER_PROCESSOR
    global _WORKER_METRICS
    metrics_client = MetricsClient(metrics_queue) if metrics_queue is not None else None
    _WORKER_METRICS = metrics_client
    _WORKER_PROCESSOR = FileProcessor(config, metrics_client)


def _process_path(path: str) -> FileResult:
    if _WORKER_PROCESSOR is None:
        raise RuntimeError("Worker not initialized")
    return _WORKER_PROCESSOR(path)


def _run_workers(paths: list[str], run_config: WorkerRunConfig) -> list[FileResult]:
    if not paths:
        return []
    jobs = min(run_config.jobs, len(paths))
    if jobs <= 1:
        processor = FileProcessor(run_config.config, run_config.metrics)
        return [processor(path) for path in paths]

    ctx = _get_mp_context()
    chunksize = max(1, len(paths) // (jobs * 4) or 1)
    with ProcessPoolExecutor(
        max_workers=jobs,
        mp_context=ctx,
        initializer=_init_worker,
        initargs=(run_config.config, run_config.metrics.queue if run_config.metrics else None),
    ) as executor:
        return list(executor.map(_process_path, paths, chunksize=chunksize))


def _log_summary(logger: logging.Logger, context: SummaryContext) -> int:
    files = 0
    changed = 0
    errors = 0
    violations = 0
    cache_hits = 0
    policy_counts: dict[str, int] = {}
    policy_times: dict[str, float] = {}

    for result in context.results:
        files += 1
        if result.changed:
            changed += 1
        if result.error:
            errors += 1
        violations += len(result.violations)
        if result.cache_hit:
            cache_hits += 1
        for violation in result.violations:
            policy_counts[violation.policy] = policy_counts.get(violation.policy, 0) + 1
        if result.profile:
            for name, ms in result.profile.items():
                policy_times[name] = policy_times.get(name, 0.0) + float(ms)
        if context.verbose and result.violations:
            logger.info("violations in %s (%d)", result.path, len(result.violations))
            for violation in result.violations:
                line = f"  - {violation.policy}: {violation.message} (line {violation.line}"
                if violation.column is not None:
                    line += f", col {violation.column}"
                line += ")"
                logger.info("%s", line)

    logger.info("files processed: %s", files)
    logger.info("files changed: %s", changed)
    logger.info("violations: %s", violations)
    logger.info("errors: %s", errors)
    logger.info("cache hits: %s", cache_hits)
    logger.info("jobs used: %s", context.jobs)
    logger.info("elapsed: %.3fs", context.elapsed)
    if context.elapsed > 0:
        logger.info("throughput: %.2f files/s", files / context.elapsed)
    if policy_counts:
        top = sorted(policy_counts.items(), key=lambda item: item[1], reverse=True)[:5]
        logger.info("top policies: %s", ", ".join(f"{name}={count}" for name, count in top))
    if policy_times:
        top_time = sorted(policy_times.items(), key=lambda item: item[1], reverse=True)[:5]
        logger.info(
            "top policy times (ms): %s",
            ", ".join(f"{name}={ms:.2f}" for name, ms in top_time),
        )

    if errors:
        return 2
    if context.check_only and violations:
        return 1
    return 0


def _list_styles(logger: logging.Logger) -> int:
    styles_root = Path(__file__).resolve().parents[1] / "styles"
    if not styles_root.exists():
        logger.error("styles folder not found: %s", styles_root)
        return 2
    styles = []
    for path in styles_root.iterdir():
        if not path.is_dir():
            continue
        if (path / "format").exists():
            styles.append(path.name)
    if not styles:
        logger.warning("no styles found under %s", styles_root)
        return 1
    for name in sorted(styles):
        logger.info("style: %s", name)
    return 0


def _list_policies(logger: logging.Logger, config: AppConfig) -> int:
    registry = PolicyRegistry()
    selector = PolicySelector(config, registry)
    factory = PolicyFactory(config)
    enabled = set(selector.resolve())
    use_color = sys.stdout.isatty() and os.environ.get("NO_COLOR") is None
    style = TableStyle(use_color=use_color, padding=2, max_width=120)
    printer = TablePrinter(style)

    rows: list[list[str]] = []
    for name in sorted(factory.available_names()):
        status = "ENABLED" if name in enabled else "DISABLED"
        if use_color:
            if status == "ENABLED":
                status = f"\x1b[32m{status}\x1b[0m"
            else:
                status = f"\x1b[31m{status}\x1b[0m"
        settings = config.policy_settings.get(name, {})
        if not isinstance(settings, dict):
            settings = {}
        meta = factory.describe(name, settings)
        rows.append([name, status, meta.get("parse_mode", "text"), meta.get("description", "")])

    table = TableData(
        headers=["Policy", "Status", "Parse", "Description"],
        rows=rows,
    )
    print(printer.render(table))
    return 0


def _validate_registry(logger: logging.Logger) -> int:
    registry = PolicyRegistry()
    report = registry.validate()
    if not report.modules_without_policies and not report.policies_without_name and not report.duplicate_names:
        logger.info("registry OK (%d policies)", len(registry.names()))
        return 0

    rows: list[list[str]] = []
    for module in sorted(report.modules_without_policies):
        rows.append(["module", module, "no Policy subclasses found"])
    for policy in sorted(report.policies_without_name):
        rows.append(["policy", policy, "missing name attribute"])
    for name, modules in report.duplicate_names.items():
        rows.append(["duplicate", name, ", ".join(sorted(modules))])

    use_color = sys.stdout.isatty() and os.environ.get("NO_COLOR") is None
    style = TableStyle(use_color=use_color, padding=2, max_width=120)
    printer = TablePrinter(style)
    table = TableData(headers=["Type", "Target", "Details"], rows=rows)
    print(printer.render(table))
    return 2


def main(argv: list[str] | None = None) -> int:
    start_time = time.perf_counter()
    args = _parse_args(argv)
    if args.validate_registry and not args.list_policies and not args.list_styles:
        logger = LogSetup().configure("INFO", None)
        return _validate_registry(logger)
    if args.list_styles and not args.list_policies:
        logger = LogSetup().configure("INFO", None)
        return _list_styles(logger)
    try:
        config = ConfigLoader().load(args)
    except FileNotFoundError as exc:
        logger = LogSetup().configure("ERROR", None)
        logger.error("%s", exc)
        return 2

    logger = LogSetup().configure(config.log_level, config.log_file)

    if args.undo or args.undo_no_delete:
        io_config = FileIOConfig(
            root=config.root,
            backup=config.backup,
            backup_mode=config.backup_mode,
            backup_suffix=config.backup_suffix,
            backup_dir=config.backup_dir,
        )
        manager = UndoManager(io_config)
        targets = manager.collect_targets(
            UndoManager.CollectTargetsArgs(
                root=Path(config.root),
                include=config.include_patterns,
                exclude=config.exclude_patterns,
            )
        )
        restored, errors = manager.restore_all(targets, delete_backup=not args.undo_no_delete)
        logger.info("restored files: %s", restored)
        if errors:
            for err in errors:
                logger.error("%s", err)
            return 2
        return 0

    if args.list_styles:
        return _list_styles(logger)

    if args.validate_registry:
        result = _validate_registry(logger)
        if result != 0:
            return result

    if args.list_policies:
        return _list_policies(logger, config)

    backup_run_id = _ensure_backup_run_id(config)

    finder = FileFinder(config)
    all_files = finder.collect()

    cache = FileCache(config.cache_path)
    cache_hits: list[FileResult] = []
    files_to_process = all_files

    if config.cache_enabled:
        cache.load()
        files_to_process = []
        for path in all_files:
            if cache.should_process(path):
                files_to_process.append(path)
            else:
                cache_hits.append(
                    FileResult(
                        path=path,
                        changed=False,
                        violations=[],
                        edits=[],
                        error=None,
                        backup_path=None,
                        cache_hit=True,
                        profile=None,
                        parse_modes=None,
                    )
                )

    jobs = config.jobs
    if jobs <= 0:
        jobs = _cpu_count()

    metrics_process = MetricsProcess(
        MetricsConfig(
            log_level=config.log_level,
            log_file=config.log_file,
            output_path=f"{config.report_path}.metrics.json",
            client_buffer_size=512,
        )
    )
    metrics_client = metrics_process.start(_get_mp_context())
    run_config = WorkerRunConfig(config=config, jobs=jobs, metrics=metrics_client)
    try:
        processed = _run_workers(files_to_process, run_config)
    finally:
        metrics_process.stop()

    if config.cache_enabled:
        for result in processed:
            if not result.error:
                cache.update(result.path)
        cache.save()

    results = cache_hits + processed
    ReportWriter(config.report_path).write(results)
    if backup_run_id:
        BackupManifest(
            BackupManifestConfig(
                backup_dir=config.backup_dir,
                run_id=backup_run_id,
                root=config.root,
                mode=config.backup_mode,
                suffix=config.backup_suffix,
            )
        ).write(results)

    elapsed = time.perf_counter() - start_time
    return _log_summary(
        logger,
        SummaryContext(
            results=results,
            check_only=config.check,
            verbose=args.verbose,
            elapsed=elapsed,
            jobs=jobs,
        ),
    )


if __name__ == "__main__":
    raise SystemExit(main())
