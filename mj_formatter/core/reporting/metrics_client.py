from __future__ import annotations

import logging
from collections import deque
from multiprocessing import Queue

from ..types import MetricsEvent


class MetricsClient:
    def __init__(self, queue: Queue | None, buffer_size: int = 0) -> None:
        self._queue = queue
        self._buffer = deque(maxlen=max(0, buffer_size))
        self._dropped = 0
        self._submitted = 0

    @property
    def queue(self) -> Queue | None:
        return self._queue

    def submit(self, event: MetricsEvent) -> bool:
        if self._queue is None:
            return False
        if self._buffer:
            if self._flush_buffer():
                return self._enqueue(event)
            self._buffer.append(event)
            return True
        return self._enqueue(event)

    def _enqueue(self, event: MetricsEvent) -> bool:
        try:
            self._queue.put_nowait(event)
            self._submitted += 1
            return True
        except Exception:
            self._dropped += 1
            if self._dropped in {1, 10, 100, 1000} or self._dropped % 5000 == 0:
                logging.getLogger("mj_formatter").warning(
                    "metrics queue drop count=%d submitted=%d",
                    self._dropped,
                    self._submitted,
                )
            return False

    def _flush_buffer(self) -> bool:
        if self._queue is None:
            return False
        try:
            while self._buffer:
                self._queue.put_nowait(self._buffer.popleft())
            return True
        except Exception:
            return False

