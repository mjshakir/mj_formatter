from __future__ import annotations

import logging


class WarnOnceRegistry:
    def __init__(self) -> None:
        self._warned: set[str] = set()

    def warn_once(self, key: str, message: str) -> None:
        if key in self._warned:
            return
        self._warned.add(key)
        logging.getLogger("mj_formatter").warning("%s", message)
