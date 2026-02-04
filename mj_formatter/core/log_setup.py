from __future__ import annotations

import logging
from pathlib import Path


class LogSetup:
    def configure(self, level: str, log_file: str | None) -> logging.Logger:
        logger = logging.getLogger("mj_formatter")
        if logger.handlers:
            return logger

        logger.setLevel(self._level_from_string(level))
        formatter = logging.Formatter("%(asctime)s [%(levelname)s] %(message)s")

        stream_handler = logging.StreamHandler()
        stream_handler.setFormatter(formatter)
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
