from __future__ import annotations

import os
import sys
from concurrent.futures import ProcessPoolExecutor
from multiprocessing import get_context
from collections.abc import Iterable

from ..logging import LogSetup
from ..processing import FileProcessor
from ..reporting import MetricsClient
from ..types import FileResult, WorkerRunConfig

_WORKER_PROCESSOR: FileProcessor | None = None
_WORKER_METRICS: MetricsClient | None = None


def _init_worker(config, metrics_queue, log_queue) -> None:
    global _WORKER_PROCESSOR
    global _WORKER_METRICS
    run_token = os.environ.get("MJ_FORMATTER_CACHE_RUN")
    if run_token:
        os.environ["MJ_FORMATTER_CACHE_SHARD"] = str(os.getpid())
    if log_queue is not None:
        LogSetup().configure_queue_handler(config.log_level, log_queue, force=True)
    metrics_client = MetricsClient(metrics_queue) if metrics_queue is not None else None
    _WORKER_METRICS = metrics_client
    _WORKER_PROCESSOR = FileProcessor(config, metrics_client)


def _process_path(path: str) -> FileResult:
    if _WORKER_PROCESSOR is None:
        raise RuntimeError("Worker not initialized")
    return _WORKER_PROCESSOR(path)


def _process_batch(paths: list[str]) -> list[FileResult]:
    if _WORKER_PROCESSOR is None:
        raise RuntimeError("Worker not initialized")
    return _WORKER_PROCESSOR.process_batch(paths)


class WorkerRunner:
    def __init__(self, run_config: WorkerRunConfig) -> None:
        self._run_config = run_config

    @staticmethod
    def cpu_count() -> int:
        if hasattr(os, "sched_getaffinity"):
            try:
                return len(os.sched_getaffinity(0))
            except Exception:
                pass
        return os.cpu_count() or 1

    @staticmethod
    def get_mp_context():
        if sys.platform.startswith("linux"):
            try:
                return get_context("fork")
            except ValueError:
                pass
        return get_context("spawn")

    def run(self, paths: list[str]) -> list[FileResult]:
        if not paths:
            return []
        jobs = min(self._run_config.jobs, len(paths))
        batch_size = max(1, int(getattr(self._run_config.config, "worker_batch_size", 1)))
        smart_batching = bool(getattr(self._run_config.config, "worker_batch_smart", True))
        if jobs <= 1:
            processor = FileProcessor(self._run_config.config, self._run_config.metrics)
            if batch_size <= 1:
                return [processor(path) for path in paths]
            batches = self._build_batches(paths, batch_size, smart_batching)
            results: list[FileResult] = []
            for batch in batches:
                results.extend(processor.process_batch(batch))
            return results
        if batch_size <= 1:
            chunksize = max(1, len(paths) // (jobs * 4) or 1)
            with ProcessPoolExecutor(
                max_workers=jobs,
                mp_context=self.get_mp_context(),
                initializer=_init_worker,
                initargs=(
                    self._run_config.config,
                    self._run_config.metrics.queue if self._run_config.metrics else None,
                    self._run_config.log_queue,
                ),
            ) as executor:
                return list(executor.map(_process_path, paths, chunksize=chunksize))

        batches = self._build_batches(paths, batch_size, smart_batching)
        chunksize = max(1, len(batches) // (jobs * 4) or 1)
        with ProcessPoolExecutor(
            max_workers=jobs,
            mp_context=self.get_mp_context(),
            initializer=_init_worker,
            initargs=(
                self._run_config.config,
                self._run_config.metrics.queue if self._run_config.metrics else None,
                self._run_config.log_queue,
            ),
        ) as executor:
            mapped = executor.map(_process_batch, batches, chunksize=chunksize)
            results: list[FileResult] = []
            for batch_results in mapped:
                results.extend(batch_results)
            return results

    @classmethod
    def _build_batches(cls, paths: list[str], batch_size: int, smart: bool) -> list[list[str]]:
        if not smart:
            return list(cls._iter_batches(paths, batch_size))
        size = max(1, int(batch_size))
        if len(paths) <= size:
            return [list(paths)]

        bucket_count = (len(paths) + size - 1) // size
        buckets: list[list[tuple[int, str]]] = [[] for _ in range(bucket_count)]
        bucket_loads = [0 for _ in range(bucket_count)]

        weighted: list[tuple[int, str, int]] = []
        for index, path in enumerate(paths):
            weighted.append((index, path, cls._file_size_hint(path)))
        weighted.sort(key=lambda item: (-item[2], item[0]))

        for index, path, weight in weighted:
            candidate_indices = [idx for idx in range(bucket_count) if len(buckets[idx]) < size]
            if not candidate_indices:
                candidate_indices = list(range(bucket_count))
            target = min(
                candidate_indices,
                key=lambda idx: (bucket_loads[idx], len(buckets[idx]), idx),
            )
            buckets[target].append((index, path))
            bucket_loads[target] += max(1, weight)

        result: list[list[str]] = []
        for bucket in buckets:
            if not bucket:
                continue
            bucket.sort(key=lambda item: item[0])
            result.append([path for _, path in bucket])
        return result

    @staticmethod
    def _file_size_hint(path: str) -> int:
        try:
            size = os.path.getsize(path)
        except OSError:
            return 0
        return max(0, int(size))

    @staticmethod
    def _iter_batches(paths: list[str], batch_size: int) -> Iterable[list[str]]:
        size = max(1, int(batch_size))
        for index in range(0, len(paths), size):
            yield paths[index : index + size]
