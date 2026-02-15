from __future__ import annotations

import logging
import sys
from logging.handlers import QueueHandler
from multiprocessing.queues import Queue
from typing import Any


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

