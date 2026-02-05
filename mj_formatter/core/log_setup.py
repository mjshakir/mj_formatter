from __future__ import annotations

import logging
import os
from pathlib import Path


class _ColorFormatter(logging.Formatter):
    _RESET = "\x1b[0m"
    _COLORS = {
        logging.DEBUG: "\x1b[90m",
        logging.WARNING: "\x1b[33m",
        logging.ERROR: "\x1b[31m",
        logging.CRITICAL: "\x1b[31m",
    }

    def __init__(self, fmt: str, use_color: bool) -> None:
        super().__init__(fmt)
        self._use_color = use_color

    def format(self, record: logging.LogRecord) -> str:
        message = super().format(record)
        if not self._use_color:
            return message
        color = self._COLORS.get(record.levelno)
        if not color:
            return message
        return f"{color}{message}{self._RESET}"


class LogSetup:
    def configure(self, level: str, log_file: str | None) -> logging.Logger:
        logger = logging.getLogger("mj_formatter")
        if logger.handlers:
            return logger

        logger.setLevel(self._level_from_string(level))
        base_format = "%(asctime)s [%(levelname)s] %(message)s"
        formatter = logging.Formatter(base_format)

        stream_handler = logging.StreamHandler()
        use_color = (
            getattr(stream_handler.stream, "isatty", lambda: False)()
            and os.environ.get("NO_COLOR") is None
        )
        stream_handler.setFormatter(_ColorFormatter(base_format, use_color))
        logger.addHandler(stream_handler)

        if log_file:
            path = Path(log_file)
            path.parent.mkdir(parents=True, exist_ok=True)
            file_handler = logging.FileHandler(path, encoding="utf-8")
            file_handler.setFormatter(formatter)
            logger.addHandler(file_handler)

        return logger

    def _level_from_string(self, level: str) -> int:
        return getattr(logging, level.upper(), logging.INFO)
