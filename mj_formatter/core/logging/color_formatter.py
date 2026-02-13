from __future__ import annotations

import logging


class ColorFormatter(logging.Formatter):
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
