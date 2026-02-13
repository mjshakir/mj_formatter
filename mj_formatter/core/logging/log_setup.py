from __future__ import annotations

import logging
import os
import sys
from logging.handlers import QueueHandler
from multiprocessing.queues import Queue
from pathlib import Path
from typing import Any

from .color_formatter import ColorFormatter


class LogSetup:
    def configure(self, level: str, log_file: str | None, force: bool = False) -> logging.Logger:
        logger = logging.getLogger("mj_formatter")
        if force:
            self._clear_handlers(logger)
        if logger.handlers:
            return logger

        logger.setLevel(self._level_from_string(level))
        logger.propagate = False
        for handler in self.build_handlers(log_file):
            logger.addHandler(handler)
        return logger

    def configure_queue_handler(self, level: str, log_queue: Queue[Any], force: bool = True) -> logging.Logger:
        logger = logging.getLogger("mj_formatter")
        if force:
            self._clear_handlers(logger)
        else:
            for handler in logger.handlers:
                if isinstance(handler, QueueHandler):
                    return logger
        logger.setLevel(self._level_from_string(level))
        logger.propagate = False
        logger.addHandler(DroppingQueueHandler(log_queue))
        return logger

    def build_handlers(self, log_file: str | None) -> list[logging.Handler]:
        base_format = "%(asctime)s [%(levelname)s] %(message)s"
        formatter = logging.Formatter(base_format)

        stream_handler = logging.StreamHandler()
        use_color = (
            getattr(stream_handler.stream, "isatty", lambda: False)()
            and os.environ.get("NO_COLOR") is None
        )
        stream_handler.setFormatter(ColorFormatter(base_format, use_color))
        handlers: list[logging.Handler] = [stream_handler]

        if log_file:
            path = Path(log_file)
            path.parent.mkdir(parents=True, exist_ok=True)
            file_handler = logging.FileHandler(path, encoding="utf-8")
            file_handler.setFormatter(formatter)
            handlers.append(file_handler)
        return handlers

    def _clear_handlers(self, logger: logging.Logger) -> None:
        for handler in list(logger.handlers):
            logger.removeHandler(handler)
            try:
                handler.close()
            except Exception:
                continue

    def _level_from_string(self, level: str) -> int:
        return getattr(logging, level.upper(), logging.INFO)


class DroppingQueueHandler(QueueHandler):
    def __init__(self, queue: Queue[Any]) -> None:
        super().__init__(queue)
        self._dropped = 0

    def enqueue(self, record: logging.LogRecord) -> None:
        try:
            self.queue.put_nowait(record)
        except Exception:
            self._dropped += 1
            if self._dropped in {1, 10, 100, 1000} or self._dropped % 5000 == 0:
                try:
                    sys.stderr.write(
                        f"mj_formatter warning: async log queue drops={self._dropped}\n"
                    )
                    sys.stderr.flush()
                except Exception:
                    pass
