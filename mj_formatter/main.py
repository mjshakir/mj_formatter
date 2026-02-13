from __future__ import annotations

import argparse
import logging
import os
import sys
import time
from pathlib import Path
from concurrent.futures import Future, ThreadPoolExecutor

from .core.config import ConfigLoader
from .core.files import (
    BackupManifest,
    BackupManifestConfig,
    CheckResultCache,
    FileCache,
    FileFinder,
    ReportWriter,
    UndoManager,
)
from .core.logging import AsyncLogManager, LogSetup
from .core.policy import PolicyFactory, PolicySelector
from .core.reporting import MetricsProcess, TablePrinter
from .policies.registry import PolicyRegistry
from .core.types import (
    AppConfig,
    FileIOConfig,
    FileResult,
    MetricsConfig,
    SummaryContext,
    TableData,
    TableStyle,
    WorkerRunConfig,
)
from .core.runtime import (
    BatchAutoTuner,
    CacheRunManager,
    CacheShardMerger,
    RunJournal,
    SummaryLogger,
    WorkerRunner,
)


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
        "--parser-strategy",
        choices=["policy", "hybrid", "tree_only", "clang_only"],
        help="Parser strategy override",
    )
    parser.add_argument(
        "--parse-pool-workers",
        type=int,
        help="Per-process thread count for tree/clang parsing",
    )
    parser.add_argument(
        "--post-edit-check",
        action=argparse.BooleanOptionalAction,
        default=None,
        help="Enable/disable post-edit parser validation",
    )
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
    parser.add_argument(
        "--batch-autotune",
        action=argparse.BooleanOptionalAction,
        default=None,
        help="Enable/disable worker batch auto-tuning",
    )
    parser.add_argument("--list-styles", action="store_true", help="List styles and exit")
    parser.add_argument("--list-policies", action="store_true", help="List policies and exit")
    parser.add_argument("--validate-registry", action="store_true", help="Validate policy registry and exit")
    parser.add_argument("--undo", action="store_true", help="Restore from latest backup and delete it")
    parser.add_argument("--undo-no-delete", action="store_true", help="Restore from latest backup without deleting")
    return parser.parse_args(argv)

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
    log_manager: AsyncLogManager | None = None
    run_journal: RunJournal | None = None
    check_result_cache: CheckResultCache | None = None
    summary_logger = SummaryLogger()
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

    ctx = WorkerRunner.get_mp_context()
    if config.async_logging:
        log_manager = AsyncLogManager(
            level=config.log_level,
            log_file=config.log_file,
            ctx=ctx,
            queue_size=config.log_queue_size,
        )
        log_queue = log_manager.start()
        logger = logging.getLogger("mj_formatter")
    else:
        log_queue = None
        logger = LogSetup().configure(config.log_level, config.log_file)

    try:
        if args.undo or args.undo_no_delete:
            io_config = FileIOConfig(
                root=config.root,
                backup=config.backup,
                backup_mode=config.backup_mode,
                backup_suffix=config.backup_suffix,
                backup_dir=config.backup_dir,
            )
            manager = UndoManager(io_config)
            targets = manager.latest_manifest_targets()
            if not targets:
                targets = manager.collect_targets(
                    UndoManager.CollectTargetsArgs(
                        root=Path(config.root),
                        include=config.include_patterns,
                        exclude=config.exclude_patterns,
                    )
                )
            restored, errors = manager.restore_all(
                targets,
                delete_backup=not args.undo_no_delete,
                ignore_missing_backups=True,
            )
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

        run_journal = RunJournal(config.run_journal_dir, logger)
        run_journal.start(config)

        cache_run_manager = CacheRunManager(config)
        backup_run_id = cache_run_manager.ensure_backup_run_id()

        finder = FileFinder(config)
        all_files = finder.collect()

        cache_fingerprint = cache_run_manager.cache_fingerprint()
        cache = FileCache(config.cache_path, fingerprint=cache_fingerprint)
        check_result_cache = CheckResultCache(
            config.check_result_cache_path,
            enabled=bool(config.cache_enabled and config.check and config.check_result_cache_enabled),
            l1_size=config.check_result_cache_l1_size,
        )
        cache_hits: list[FileResult] = []
        files_to_process = all_files
        check_hashes: dict[str, str] = {}

        if config.cache_enabled:
            cache.load()
            files_to_process = []
            for path in all_files:
                if cache.should_process(path):
                    if check_result_cache.enabled:
                        content_hash = check_result_cache.hash_file(path)
                        if content_hash:
                            check_hashes[path] = content_hash
                            cached_result = check_result_cache.get(
                                path=path,
                                content_hash=content_hash,
                                fingerprint=cache_fingerprint,
                            )
                            if cached_result is not None:
                                cache_hits.append(cached_result)
                                continue
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
                            warnings=[],
                        )
                    )

        jobs = config.jobs
        if jobs <= 0:
            jobs = WorkerRunner.cpu_count()

        batch_autotuner = BatchAutoTuner(config, logger)
        selected_batch_size = batch_autotuner.choose_batch_size(len(files_to_process), jobs)
        config.worker_batch_size = selected_batch_size

        metrics_process = MetricsProcess(
            MetricsConfig(
                log_level=config.log_level,
                log_file=config.log_file,
                output_path=f"{config.report_path}.metrics.json",
                client_buffer_size=512,
            )
        )
        metrics_client = metrics_process.start(ctx)
        run_config = WorkerRunConfig(config=config, jobs=jobs, metrics=metrics_client, log_queue=log_queue)
        worker_runner = WorkerRunner(run_config)
        shard_merger = CacheShardMerger(config, logger)
        shard_run_token = cache_run_manager.prepare_cache_shards(jobs)
        processed: list[FileResult] = []
        worker_error: Exception | None = None
        worker_elapsed = 0.0
        try:
            worker_start = time.perf_counter()
            processed = worker_runner.run(files_to_process)
            worker_elapsed = time.perf_counter() - worker_start
        except Exception as exc:
            worker_error = exc
        finally:
            finalize_futures: list[Future[None]] = []
            with ThreadPoolExecutor(max_workers=2, thread_name_prefix="mj-finalize") as finalize_pool:
                finalize_futures.append(finalize_pool.submit(metrics_process.stop))
                if shard_run_token:
                    finalize_futures.append(
                        finalize_pool.submit(
                            shard_merger.merge,
                            shard_run_token,
                            all_files,
                            config.shard_merge_workers,
                        )
                    )
                for future in finalize_futures:
                    future.result()
            cache_run_manager.clear_worker_shard_env(shard_run_token)
        if worker_error is not None:
            raise worker_error

        batch_autotuner.record(
            files_processed=len(files_to_process),
            elapsed_s=worker_elapsed,
        )

        if config.cache_enabled:
            for result in processed:
                if result.error:
                    continue
                # In check mode, files that *would* change must not be cached as clean.
                if config.check and result.changed:
                    pass
                else:
                    cache.update(result.path)
                if check_result_cache is not None and check_result_cache.enabled:
                    content_hash = check_hashes.get(result.path)
                    if content_hash is None:
                        content_hash = check_result_cache.hash_file(result.path)
                        if content_hash:
                            check_hashes[result.path] = content_hash
                    if content_hash:
                        check_result_cache.put(
                            path=result.path,
                            content_hash=content_hash,
                            fingerprint=cache_fingerprint,
                            result=result,
                        )

        results = cache_hits + processed
        io_futures: list[Future[None]] = []
        with ThreadPoolExecutor(max_workers=3, thread_name_prefix="mj-io") as io_pool:
            if config.cache_enabled:
                io_futures.append(io_pool.submit(cache.save))
            io_futures.append(io_pool.submit(ReportWriter(config.report_path).write, results))
            if backup_run_id:
                io_futures.append(
                    io_pool.submit(
                        BackupManifest(
                            BackupManifestConfig(
                                backup_dir=config.backup_dir,
                                run_id=backup_run_id,
                                root=config.root,
                                mode=config.backup_mode,
                                suffix=config.backup_suffix,
                            )
                        ).write,
                        results,
                    )
                )
            for future in io_futures:
                future.result()

        elapsed = time.perf_counter() - start_time
        status_code = summary_logger.log_and_status(
            logger,
            SummaryContext(
                results=results,
                check_only=config.check,
                verbose=args.verbose,
                elapsed=elapsed,
                jobs=jobs,
                fail_on_conflict=config.conflict_fail_on_detected,
            ),
        )
        files = len(results)
        changed = sum(1 for item in results if item.changed)
        errors = sum(1 for item in results if item.error)
        try:
            run_journal.finish(
                status="COMPLETED" if status_code in (0, 1) else "FAILED",
                exit_code=status_code,
                files=files,
                changed=changed,
                errors=errors,
            )
        except Exception as exc:
            logger.warning("run journal finalize failed: %s", exc)
        return status_code
    except Exception:
        if run_journal is not None:
            try:
                run_journal.finish(status="FAILED", exit_code=2)
            except Exception:
                pass
        raise
    finally:
        if check_result_cache is not None:
            check_result_cache.close()
        if log_manager is not None:
            log_manager.stop()


if __name__ == "__main__":
    raise SystemExit(main())
