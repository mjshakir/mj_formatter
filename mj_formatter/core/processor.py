from __future__ import annotations

import logging

from .app_config import AppConfig
from .file_io import FileIO
from .file_result import FileResult
from .formatter_engine import FormatterEngine
from .structs import FileIOConfig


class FileProcessor:
    def __init__(self, config: AppConfig) -> None:
        self._config = config
        self._engine: FormatterEngine | None = None
        self._file_io: FileIO | None = None

    def __call__(self, path: str) -> FileResult:
        logger = logging.getLogger("mj_formatter")
        if not logger.handlers:
            from .log_setup import LogSetup
            LogSetup().configure(self._config.log_level, self._config.log_file)
            logger = logging.getLogger("mj_formatter")
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
                logger.error("read failed for %s: %s", path, exc)
                return FileResult(
                    path=path,
                    changed=False,
                    violations=[],
                    edits=[],
                    error=f"read failed: {exc}",
                    backup_path=None,
                    cache_hit=False,
                )

            try:
                result = self._engine.apply(original, path)
            except Exception as exc:
                logger.error("policy failure for %s: %s", path, exc)
                return FileResult(
                    path=path,
                    changed=False,
                    violations=[],
                    edits=[],
                    error=f"policy failure: {exc}",
                    backup_path=None,
                    cache_hit=False,
                )

            changed = result.text != original

            if changed and not self._config.check:
                backup_path, error = self._file_io.write_text(path, result.text)
                if error:
                    logger.error("write failed for %s: %s", path, error)
                    return FileResult(
                        path=path,
                        changed=False,
                        violations=result.violations,
                        edits=result.edits,
                        error=error,
                        backup_path=backup_path,
                        cache_hit=False,
                    )
                return FileResult(
                    path=path,
                    changed=True,
                    violations=result.violations,
                    edits=result.edits,
                    error=None,
                    backup_path=backup_path,
                    cache_hit=False,
                )

            return FileResult(
                path=path,
                changed=changed,
                violations=result.violations,
                edits=result.edits,
                error=None,
                backup_path=None,
                cache_hit=False,
            )
        except Exception as exc:
            logger.error("unexpected failure for %s: %s", path, exc)
            return FileResult(
                path=path,
                changed=False,
                violations=[],
                edits=[],
                error=f"unexpected failure: {exc}",
                backup_path=None,
                cache_hit=False,
            )
