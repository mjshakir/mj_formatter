from __future__ import annotations

import atexit
import logging
import time
from collections.abc import Sequence
from concurrent.futures import Future, ThreadPoolExecutor

from ..files import FileIO
from ..reporting.metrics import MetricsClient, MetricsEvent
from ..types import AppConfig, FileIOConfig, FileResult
from .formatter_engine import FormatterEngine


class FileProcessor:
    def __init__(self, config: AppConfig, metrics: MetricsClient | None = None) -> None:
        self._config = config
        self._engine: FormatterEngine | None = None
        self._file_io: FileIO | None = None
        self._logger = logging.getLogger("mj_formatter")
        self._metrics = metrics
        self._cache_flush_registered = False
        if not self._logger.handlers:
            from ..logging import LogSetup

            LogSetup().configure(self._config.log_level, self._config.log_file)
            self._logger = logging.getLogger("mj_formatter")

    def __call__(self, path: str) -> FileResult:
        start = time.perf_counter()
        result = self._process_single(path)
        self._submit_metrics(path, result, (time.perf_counter() - start) * 1000.0)
        return result

    def process_batch(self, paths: Sequence[str]) -> list[FileResult]:
        if not paths:
            return []
        batch_paths = list(paths)
        if len(batch_paths) == 1 or not bool(self._config.worker_batch_prefetch):
            return [self(path) for path in batch_paths]

        self._ensure_runtime()
        results: list[FileResult] = []
        with ThreadPoolExecutor(max_workers=1, thread_name_prefix="mj-read-prefetch") as pool:
            future: Future[tuple[str, str | None, str | None]] = pool.submit(
                self._read_for_batch, batch_paths[0]
            )
            for index, _ in enumerate(batch_paths):
                path, original, read_error = future.result()
                next_future: Future[tuple[str, str | None, str | None]] | None = None
                if index + 1 < len(batch_paths):
                    next_future = pool.submit(self._read_for_batch, batch_paths[index + 1])

                start = time.perf_counter()
                if read_error is not None or original is None:
                    result = self._error_result(path, f"read failed: {read_error or 'unknown read failure'}")
                else:
                    result = self._process_with_original(path, original)
                self._submit_metrics(path, result, (time.perf_counter() - start) * 1000.0)
                results.append(result)

                if next_future is not None:
                    future = next_future
        return results

    def _ensure_runtime(self) -> None:
        if self._engine is None:
            self._engine = FormatterEngine(self._config)
            if not self._cache_flush_registered:
                atexit.register(self._flush_engine_caches)
                self._cache_flush_registered = True
        if self._file_io is None:
            io_config = FileIOConfig(
                root=self._config.root,
                backup=self._config.backup,
                backup_mode=self._config.backup_mode,
                backup_suffix=self._config.backup_suffix,
                backup_dir=self._config.backup_dir,
            )
            self._file_io = FileIO(io_config)

    def _process_single(self, path: str) -> FileResult:
        try:
            self._ensure_runtime()
            original = self._read_text(path)
        except Exception as exc:
            self._logger.error("read failed for %s: %s", path, exc)
            return self._error_result(path, f"read failed: {exc}")
        return self._process_with_original(path, original)

    def _process_with_original(self, path: str, original: str) -> FileResult:
        try:
            self._ensure_runtime()
            assert self._engine is not None
            assert self._file_io is not None
            policy_result = self._engine.apply(original, path)
        except Exception as exc:
            self._logger.error("policy failure for %s: %s", path, exc)
            return self._error_result(path, f"policy failure: {exc}")

        changed = policy_result.text != original
        if changed and not self._config.check:
            backup_path, error = self._file_io.write_text(path, policy_result.text)
            if error:
                self._logger.error("write failed for %s: %s", path, error)
                return FileResult(
                    path=path,
                    changed=False,
                    violations=policy_result.violations,
                    edits=policy_result.edits,
                    error=error,
                    backup_path=backup_path,
                    cache_hit=False,
                    profile=policy_result.profile,
                    parse_modes=policy_result.parse_modes,
                    warnings=policy_result.warnings,
                )
            return FileResult(
                path=path,
                changed=True,
                violations=policy_result.violations,
                edits=policy_result.edits,
                error=None,
                backup_path=backup_path,
                cache_hit=False,
                profile=policy_result.profile,
                parse_modes=policy_result.parse_modes,
                warnings=policy_result.warnings,
            )

        return FileResult(
            path=path,
            changed=changed,
            violations=policy_result.violations,
            edits=policy_result.edits,
            error=None,
            backup_path=None,
            cache_hit=False,
            profile=policy_result.profile,
            parse_modes=policy_result.parse_modes,
            warnings=policy_result.warnings,
        )

    def _read_text(self, path: str) -> str:
        assert self._file_io is not None
        return self._file_io.read_text(path)

    def _read_for_batch(self, path: str) -> tuple[str, str | None, str | None]:
        try:
            text = self._read_text(path)
            return path, text, None
        except Exception as exc:
            self._logger.error("read failed for %s: %s", path, exc)
            return path, None, str(exc)

    def _error_result(self, path: str, message: str) -> FileResult:
        return FileResult(
            path=path,
            changed=False,
            violations=[],
            edits=[],
            error=message,
            backup_path=None,
            cache_hit=False,
            profile=None,
            parse_modes=None,
            warnings=[],
        )

    def _submit_metrics(self, path: str, result: FileResult, duration_ms: float) -> None:
        if self._metrics is None:
            return
        policy_names = [violation.policy for violation in result.violations]
        event = MetricsEvent(
            path=path,
            changed=result.changed,
            violations=len(result.violations),
            error=bool(result.error),
            cache_hit=result.cache_hit,
            duration_ms=duration_ms,
            edits=len(result.edits),
            warnings=len(result.warnings or []),
            policies=policy_names,
            error_message=result.error,
            parse_modes=result.parse_modes or {},
        )
        self._metrics.submit(event)

    def _flush_engine_caches(self) -> None:
        if self._engine is None:
            return
        try:
            self._engine.flush_caches()
        except Exception:
            # Best effort during process teardown.
            return
