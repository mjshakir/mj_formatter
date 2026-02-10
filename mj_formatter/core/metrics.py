from __future__ import annotations

import json
import logging
from collections import deque
from dataclasses import dataclass
from multiprocessing import Queue
from typing import Any

from .log_setup import LogSetup


@dataclass(frozen=True)
class MetricsConfig:
    log_level: str
    log_file: str | None
    output_path: str | None
    queue_size: int = 10000
    client_buffer_size: int = 0
    include_files: bool = True
    max_files: int = 5000
    include_policies: bool = True
    include_edits: bool = True
    include_parse_modes: bool = True


@dataclass(frozen=True)
class MetricsEvent:
    path: str
    changed: bool
    violations: int
    error: bool
    cache_hit: bool
    duration_ms: float
    edits: int
    policies: list[str]
    error_message: str | None
    parse_modes: dict[str, str]


class MetricsClient:
    def __init__(self, queue: Queue | None, buffer_size: int = 0) -> None:
        self._queue = queue
        self._buffer = deque(maxlen=max(0, buffer_size))

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
            return True
        except Exception:
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


class MetricsProcess:
    def __init__(self, config: MetricsConfig) -> None:
        self._config = config
        self._process = None
        self._queue: Queue | None = None

    def start(self, ctx) -> MetricsClient:
        self._queue = ctx.Queue(maxsize=self._config.queue_size)
        self._process = ctx.Process(
            target=_metrics_worker,
            args=(self._queue, self._config),
            name="mj-metrics",
            daemon=True,
        )
        self._process.start()
        return MetricsClient(self._queue, buffer_size=self._config.client_buffer_size)

    def stop(self, timeout: float = 5.0) -> None:
        if self._queue is None or self._process is None:
            return
        try:
            self._queue.put_nowait(None)
        except Exception:
            pass
        self._process.join(timeout=timeout)


def _metrics_worker(queue: Queue, config: MetricsConfig) -> None:
    logger = LogSetup().configure(config.log_level, config.log_file)
    summary = {
        "files": 0,
        "changed": 0,
        "violations": 0,
        "errors": 0,
        "cache_hits": 0,
        "duration_ms": 0.0,
        "edits": 0,
    }
    per_policy: dict[str, int] = {}
    per_backend: dict[str, int] = {}
    per_file: list[dict[str, Any]] = []

    while True:
        try:
            item = queue.get()
        except (EOFError, OSError):
            break
        if item is None:
            break
        if not isinstance(item, MetricsEvent):
            continue
        summary["files"] += 1
        summary["changed"] += 1 if item.changed else 0
        summary["violations"] += item.violations
        summary["errors"] += 1 if item.error else 0
        summary["cache_hits"] += 1 if item.cache_hit else 0
        summary["duration_ms"] += float(item.duration_ms)
        summary["edits"] += int(item.edits)
        if config.include_policies:
            for name in item.policies:
                per_policy[name] = per_policy.get(name, 0) + 1
        if config.include_parse_modes:
            for _, backend in item.parse_modes.items():
                per_backend[backend] = per_backend.get(backend, 0) + 1
        if config.include_files and len(per_file) < config.max_files:
            per_file.append(
                {
                    "path": item.path,
                    "changed": item.changed,
                    "violations": item.violations,
                    "edits": item.edits,
                    "error": item.error,
                    "error_message": item.error_message,
                    "cache_hit": item.cache_hit,
                    "duration_ms": item.duration_ms,
                    "policies": item.policies if config.include_policies else [],
                    "parse_modes": item.parse_modes if config.include_parse_modes else {},
                }
            )

    logger.info("metrics files: %s", summary["files"])
    logger.info("metrics changed: %s", summary["changed"])
    logger.info("metrics violations: %s", summary["violations"])
    logger.info("metrics errors: %s", summary["errors"])
    logger.info("metrics cache hits: %s", summary["cache_hits"])
    logger.info("metrics duration_ms: %.2f", summary["duration_ms"])
    logger.info("metrics edits: %s", summary["edits"])
    if per_policy:
        top = sorted(per_policy.items(), key=lambda item: item[1], reverse=True)[:5]
        logger.info("metrics top policies: %s", ", ".join(f"{name}={count}" for name, count in top))

    if config.output_path:
        try:
            with open(config.output_path, "w", encoding="utf-8") as handle:
                json.dump(
                    {
                        "summary": summary,
                        "policies": per_policy if config.include_policies else {},
                        "parse_modes": per_backend if config.include_parse_modes else {},
                        "files": per_file if config.include_files else [],
                    },
                    handle,
                    indent=2,
                )
        except Exception as exc:
            logger.warning("metrics write failed: %s", exc)
