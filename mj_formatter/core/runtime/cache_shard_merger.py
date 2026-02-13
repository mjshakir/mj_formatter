from __future__ import annotations

import logging
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Iterable

from ..types import AppConfig


class CacheShardMerger:
    def __init__(self, config: AppConfig, logger: logging.Logger) -> None:
        self._config = config
        self._logger = logger

    def merge(self, run_token: str, valid_files: Iterable[str], merge_workers: int = 2) -> None:
        from ..policy import PolicyCache, ProjectIndexCache

        shard_root = Path(self._config.policy_cache_path).parent / ".shards" / run_token
        if not shard_root.exists():
            return

        policy_name = Path(self._config.policy_cache_path).name
        policy_files = sorted(shard_root.glob(f"{policy_name}.*.bin"))
        index_path = Path(self._config.policy_cache_path).with_name("project_index_cache.bin")
        index_files = sorted(shard_root.glob("project_index_cache.*.bin"))

        def merge_policy() -> int:
            if not policy_files or not self._config.cache_enabled:
                return 0
            policy_cache = PolicyCache(self._config.policy_cache_path, enabled=True)
            policy_cache.load()
            merged = 0
            for item in policy_files:
                policy_cache.merge_file(item)
                merged += 1
            policy_cache.save()
            return merged

        def merge_index() -> int:
            index_cache = ProjectIndexCache(str(index_path), enabled=True)
            index_cache.load()
            merged = 0
            for item in index_files:
                index_cache.merge_file(item)
                merged += 1
            index_cache.prune_to_files(set(valid_files))
            index_cache.save()
            return merged

        worker_count = max(1, int(merge_workers))
        with ThreadPoolExecutor(max_workers=worker_count, thread_name_prefix="mj-shard-merge") as pool:
            policy_future = pool.submit(merge_policy)
            index_future = pool.submit(merge_index)
            merged_policy = int(policy_future.result())
            merged_index = int(index_future.result())

        if merged_policy or merged_index:
            self._logger.info("merged cache shards: policy=%d index=%d", merged_policy, merged_index)

        for item in shard_root.glob("*"):
            try:
                item.unlink(missing_ok=True)
            except Exception:
                pass
        try:
            shard_root.rmdir()
        except Exception:
            pass
