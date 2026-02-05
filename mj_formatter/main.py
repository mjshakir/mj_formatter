from __future__ import annotations

import argparse
import logging
import os
import sys
from datetime import datetime, timezone
from pathlib import Path
from concurrent.futures import ProcessPoolExecutor
from multiprocessing import get_context
from typing import Iterable

from .core.app_config import AppConfig
from .core.config_loader import ConfigLoader
from .core.backup_manifest import BackupManifest
from .core.file_cache import FileCache
from .core.file_finder import FileFinder
from .core.file_result import FileResult
from .core.log_setup import LogSetup
from .core.policy_selector import PolicySelector
from .core.table_printer import TablePrinter
from .core.processor import FileProcessor
from .core.report_writer import ReportWriter
from .core.undo_manager import UndoManager
from .policies.registry import PolicyRegistry
from .core.structs import FileIOConfig, TableData, TableStyle


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


def _init_worker(config: AppConfig) -> None:
    global _WORKER_PROCESSOR
    _WORKER_PROCESSOR = FileProcessor(config)


def _process_path(path: str) -> FileResult:
    if _WORKER_PROCESSOR is None:
        raise RuntimeError("Worker not initialized")
    return _WORKER_PROCESSOR(path)


def _run_workers(paths: list[str], config: AppConfig, jobs: int) -> list[FileResult]:
    if not paths:
        return []
    jobs = min(jobs, len(paths))
    if jobs <= 1:
        processor = FileProcessor(config)
        return [processor(path) for path in paths]

    ctx = _get_mp_context()
    chunksize = max(1, len(paths) // (jobs * 2) or 1)
    with ProcessPoolExecutor(
        max_workers=jobs,
        mp_context=ctx,
        initializer=_init_worker,
        initargs=(config,),
    ) as executor:
        return list(executor.map(_process_path, paths, chunksize=chunksize))


def _log_summary(
    logger: logging.Logger,
    results: Iterable[FileResult],
    check_only: bool,
    verbose: bool,
) -> int:
    files = 0
    changed = 0
    errors = 0
    violations = 0

    for result in results:
        files += 1
        if result.changed:
            changed += 1
        if result.error:
            errors += 1
        violations += len(result.violations)
        if verbose and result.violations:
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

    if errors:
        return 2
    if check_only and violations:
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
    enabled = set(selector.resolve())
    use_color = sys.stdout.isatty() and os.environ.get("NO_COLOR") is None
    style = TableStyle(use_color=use_color, padding=2, max_width=120)
    printer = TablePrinter(style)

    rows: list[list[str]] = []
    for name, cls in sorted(registry.items(), key=lambda item: item[0]):
        status = "ENABLED" if name in enabled else "DISABLED"
        if use_color:
            if status == "ENABLED":
                status = f"\x1b[32m{status}\x1b[0m"
            else:
                status = f"\x1b[31m{status}\x1b[0m"
        parse_mode = str(getattr(cls, "parse_mode", "text"))
        description = str(getattr(cls, "description", ""))
        rows.append([name, status, parse_mode, description])

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
            Path(config.root),
            config.include_patterns,
            config.exclude_patterns,
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
                    )
                )

    jobs = config.jobs
    if jobs <= 0:
        jobs = _cpu_count()

    processed = _run_workers(files_to_process, config, jobs)

    if config.cache_enabled:
        for result in processed:
            if not result.error:
                cache.update(result.path)
        cache.save()

    results = cache_hits + processed
    ReportWriter(config.report_path).write(results)
    if backup_run_id:
        BackupManifest(
            backup_dir=config.backup_dir,
            run_id=backup_run_id,
            root=config.root,
            mode=config.backup_mode,
            suffix=config.backup_suffix,
        ).write(results)

    return _log_summary(logger, results, config.check, args.verbose)


if __name__ == "__main__":
    raise SystemExit(main())
