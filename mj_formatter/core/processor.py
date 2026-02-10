from __future__ import annotations

import logging
import time

from .app_config import AppConfig
from .file_io import FileIO
from .file_result import FileResult
from .formatter_engine import FormatterEngine
from .structs import FileIOConfig
from .metrics import MetricsClient, MetricsEvent


class FileProcessor:
    def __init__(self, config: AppConfig, metrics: MetricsClient | None = None) -> None:
        self._config = config
        self._engine: FormatterEngine | None = None
        self._file_io: FileIO | None = None
        self._logger = logging.getLogger("mj_formatter")
        self._metrics = metrics
        if not self._logger.handlers:
            from .log_setup import LogSetup
            LogSetup().configure(self._config.log_level, self._config.log_file)
            self._logger = logging.getLogger("mj_formatter")

    def __call__(self, path: str) -> FileResult:
        start = time.perf_counter()
        result: FileResult | None = None
        try:
            if self._engine is None:
                self._engine = FormatterEngine(self._config)
            if self._file_io is None:
                io_config = FileIOConfig(
                    root=self._config.root,
                    backup=self._config.backup,
                    backup_mode=self._config.backup_mode,
                    backup_suffix=self._config.backup_suffix,
                    backup_dir=self._config.backup_dir,
                )
                self._file_io = FileIO(io_config)

            try:
                original = self._file_io.read_text(path)
            except Exception as exc:
                self._logger.error("read failed for %s: %s", path, exc)
                result = FileResult(
                    path=path,
                    changed=False,
                    violations=[],
                    edits=[],
                    error=f"read failed: {exc}",
                    backup_path=None,
                    cache_hit=False,
                    profile=None,
                    parse_modes=None,
                )
                return result

            try:
                result = self._engine.apply(original, path)
            except Exception as exc:
                self._logger.error("policy failure for %s: %s", path, exc)
                result = FileResult(
                    path=path,
                    changed=False,
                    violations=[],
                    edits=[],
                    error=f"policy failure: {exc}",
                    backup_path=None,
                    cache_hit=False,
                    profile=None,
                    parse_modes=None,
                )
                return result

            changed = result.text != original

            if changed and not self._config.check:
                backup_path, error = self._file_io.write_text(path, result.text)
                if error:
                    self._logger.error("write failed for %s: %s", path, error)
                    result = FileResult(
                        path=path,
                        changed=False,
                        violations=result.violations,
                        edits=result.edits,
                        error=error,
                        backup_path=backup_path,
                        cache_hit=False,
                        profile=result.profile,
                        parse_modes=result.parse_modes,
                    )
                    return result
                result = FileResult(
                    path=path,
                    changed=True,
                    violations=result.violations,
                    edits=result.edits,
                    error=None,
                    backup_path=backup_path,
                    cache_hit=False,
                    profile=result.profile,
                    parse_modes=result.parse_modes,
                )
                return result

            result = FileResult(
                path=path,
                changed=changed,
                violations=result.violations,
                edits=result.edits,
                error=None,
                backup_path=None,
                cache_hit=False,
                profile=result.profile,
                parse_modes=result.parse_modes,
            )
            return result
        except Exception as exc:
            self._logger.error("unexpected failure for %s: %s", path, exc)
            result = FileResult(
                path=path,
                changed=False,
                violations=[],
                edits=[],
                error=f"unexpected failure: {exc}",
                backup_path=None,
                cache_hit=False,
                profile=None,
                parse_modes=None,
            )
            return result
        finally:
            if self._metrics is not None:
                duration_ms = (time.perf_counter() - start) * 1000.0
                if result is None:
                    event = MetricsEvent(
                        path=path,
                        changed=False,
                        violations=0,
                        error=True,
                        cache_hit=False,
                        duration_ms=duration_ms,
                        edits=0,
                        policies=[],
                        error_message="unexpected error",
                        parse_modes={},
                    )
                else:
                    policy_names = [v.policy for v in result.violations]
                    event = MetricsEvent(
                        path=path,
                        changed=result.changed,
                        violations=len(result.violations),
                        error=bool(result.error),
                        cache_hit=result.cache_hit,
                        duration_ms=duration_ms,
                        edits=len(result.edits),
                        policies=policy_names,
                        error_message=result.error,
                        parse_modes=result.parse_modes or {},
                    )
                self._metrics.submit(event)
