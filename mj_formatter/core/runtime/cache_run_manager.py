from __future__ import annotations

import hashlib
import json
import os
from datetime import datetime, timezone
from pathlib import Path

from ..types import AppConfig


class CacheRunManager:
    def __init__(self, config: AppConfig) -> None:
        self._config = config

    def ensure_backup_run_id(self) -> str | None:
        if not self._config.backup or self._config.check:
            return None
        run_id = os.environ.get("MJ_FORMATTER_BACKUP_RUN")
        if run_id:
            return run_id
        run_id = datetime.now(timezone.utc).strftime("%Y%m%d_%H%M%S")
        os.environ["MJ_FORMATTER_BACKUP_RUN"] = run_id
        return run_id

    def cache_fingerprint(self) -> str:
        payload = {
            "root": self._config.root,
            "include": self._config.include_patterns,
            "exclude": self._config.exclude_patterns,
            "clang_args": self._config.clang_args,
            "clang_compdb_path": self._config.clang_compdb_path,
            "clang_args_mode": self._config.clang_args_mode,
            "policies_default": self._config.policies_default,
            "policies_enabled": sorted(self._config.policies_enabled),
            "policies_disabled": sorted(self._config.policies_disabled),
            "policies_order": self._config.policies_order,
            "policy_settings": self._config.policy_settings,
            "conflict_detection_enabled": self._config.conflict_detection_enabled,
            "conflict_touch_threshold": self._config.conflict_touch_threshold,
        }
        encoded = json.dumps(payload, sort_keys=True, default=str).encode("utf-8")
        return hashlib.blake2b(encoded, digest_size=16).hexdigest()

    def prepare_cache_shards(self, jobs: int) -> str | None:
        if not self._config.cache_enabled or jobs <= 1:
            return None
        run_token = datetime.now(timezone.utc).strftime("%Y%m%d_%H%M%S_%f")
        os.environ["MJ_FORMATTER_CACHE_RUN"] = run_token
        shard_root = Path(self._config.policy_cache_path).parent / ".shards" / run_token
        shard_root.mkdir(parents=True, exist_ok=True)
        return run_token

    def clear_worker_shard_env(self, run_token: str | None) -> None:
        os.environ.pop("MJ_FORMATTER_CACHE_SHARD", None)
        if run_token:
            os.environ.pop("MJ_FORMATTER_CACHE_RUN", None)
