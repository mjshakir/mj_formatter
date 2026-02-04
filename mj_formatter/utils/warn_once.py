from __future__ import annotations

import logging

_warned: set[str] = set()


def warn_once(key: str, message: str) -> None:
    if key in _warned:
        return
    _warned.add(key)
    logging.getLogger("mj_formatter").warning("%s", message)
