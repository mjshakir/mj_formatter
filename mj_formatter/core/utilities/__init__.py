from __future__ import annotations

from .atomic_writer import AtomicWriter
from .warn_once_registry import WarnOnceRegistry

_WARN_ONCE = WarnOnceRegistry()


def warn_once(key: str, message: str) -> None:
    _WARN_ONCE.warn_once(key, message)


__all__ = ["AtomicWriter", "WarnOnceRegistry", "warn_once"]
