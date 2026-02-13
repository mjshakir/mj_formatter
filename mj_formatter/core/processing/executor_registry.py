from __future__ import annotations

import atexit
from concurrent.futures import ThreadPoolExecutor
from threading import Lock


class ExecutorRegistry:
    """Process-local singleton registry for shared executors."""

    _instance: "ExecutorRegistry | None" = None
    _instance_lock = Lock()

    def __new__(cls) -> "ExecutorRegistry":
        if cls._instance is None:
            with cls._instance_lock:
                if cls._instance is None:
                    instance = super().__new__(cls)
                    instance._initialized = False
                    cls._instance = instance
        return cls._instance

    def __init__(self) -> None:
        if self._initialized:
            return
        self._lock = Lock()
        self._parse_pool: ThreadPoolExecutor | None = None
        self._parse_pool_size = 0
        self._post_edit_pool: ThreadPoolExecutor | None = None
        atexit.register(self.shutdown)
        self._initialized = True

    def get_parse_pool(self, workers: int) -> ThreadPoolExecutor:
        count = max(1, int(workers))
        with self._lock:
            if self._parse_pool is not None and self._parse_pool_size == count:
                return self._parse_pool
            old_pool = self._parse_pool
            self._parse_pool = ThreadPoolExecutor(max_workers=count, thread_name_prefix="mj-parse")
            self._parse_pool_size = count
        if old_pool is not None:
            old_pool.shutdown(wait=False, cancel_futures=False)
        return self._parse_pool

    def get_post_edit_pool(self) -> ThreadPoolExecutor:
        with self._lock:
            if self._post_edit_pool is None:
                self._post_edit_pool = ThreadPoolExecutor(max_workers=1, thread_name_prefix="mj-post-edit")
            return self._post_edit_pool

    def shutdown(self) -> None:
        with self._lock:
            parse_pool = self._parse_pool
            post_edit_pool = self._post_edit_pool
            self._parse_pool = None
            self._post_edit_pool = None
            self._parse_pool_size = 0
        if parse_pool is not None:
            parse_pool.shutdown(wait=False, cancel_futures=False)
        if post_edit_pool is not None:
            post_edit_pool.shutdown(wait=False, cancel_futures=False)
