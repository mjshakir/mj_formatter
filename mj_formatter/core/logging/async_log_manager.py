from __future__ import annotations

from logging.handlers import QueueListener
from multiprocessing.queues import Queue
from typing import Any

from .log_setup import LogSetup


class AsyncLogManager:
    def __init__(self, level: str, log_file: str | None, ctx: Any, queue_size: int = 10000) -> None:
        self._level = level
        self._log_file = log_file
        self._ctx = ctx
        self._queue_size = max(100, int(queue_size))
        self._queue: Queue[Any] | None = None
        self._listener: QueueListener | None = None
        self._started = False

    @property
    def queue(self) -> Queue[Any] | None:
        return self._queue

    def start(self) -> Queue[Any]:
        if self._started and self._queue is not None:
            return self._queue
        setup = LogSetup()
        handlers = setup.build_handlers(self._log_file)
        self._queue = self._ctx.Queue(maxsize=self._queue_size)
        self._listener = QueueListener(self._queue, *handlers, respect_handler_level=True)
        self._listener.start()
        setup.configure_queue_handler(self._level, self._queue, force=True)
        self._started = True
        return self._queue

    def stop(self) -> None:
        if self._listener is not None:
            self._listener.stop()
            self._listener = None
        if self._queue is not None:
            try:
                self._queue.close()
                self._queue.join_thread()
            except Exception:
                pass
            self._queue = None
        self._started = False
