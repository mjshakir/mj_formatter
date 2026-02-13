from __future__ import annotations

import hashlib
import json
import logging
from pathlib import Path
from typing import Any

from ..types import AppConfig
from ..utilities import AtomicWriter


class BatchAutoTuner:
    def __init__(self, config: AppConfig, logger: logging.Logger) -> None:
        self._config = config
        self._logger = logger
        self._enabled = bool(config.worker_batch_autotune_enabled)
        self._path = Path(config.worker_batch_autotune_path)
        self._selected_batch_size = int(config.worker_batch_size)
        self._selected_reason = "configured"
        self._profile_key = self._make_profile_key(config)
        self._loaded_data: dict[str, Any] | None = None

    @property
    def enabled(self) -> bool:
        return self._enabled

    def choose_batch_size(self, files_to_process: int, jobs: int) -> int:
        configured = max(1, int(self._config.worker_batch_size))
        self._selected_batch_size = configured
        self._selected_reason = "configured"
        if not self._enabled:
            return configured
        if jobs <= 1:
            self._selected_reason = "single_worker"
            return configured
        if files_to_process < int(self._config.worker_batch_autotune_min_files):
            self._selected_reason = "small_workload"
            return configured

        candidates = [value for value in self._config.worker_batch_autotune_candidates if int(value) >= 1]
        if not candidates:
            candidates = [configured]
        unique_candidates = sorted(
            set(
                int(value)
                for value in candidates
                if int(value) <= max(1, int(files_to_process))
            )
        )
        if not unique_candidates:
            unique_candidates = [configured]

        data = self._load()
        profiles = data.setdefault("profiles", {})
        profile = profiles.setdefault(
            self._profile_key,
            {"runs": 0, "next_probe_idx": 0, "stats": {}},
        )
        stats = profile.get("stats", {})
        if not isinstance(stats, dict):
            stats = {}
            profile["stats"] = stats

        missing = [value for value in unique_candidates if str(value) not in stats]
        if missing:
            selected = missing[0]
            reason = "probe_bootstrap"
        else:
            probe_interval = max(1, int(self._config.worker_batch_autotune_probe_interval))
            runs = int(profile.get("runs", 0))
            if runs > 0 and runs % probe_interval == 0:
                next_idx = int(profile.get("next_probe_idx", 0))
                selected = unique_candidates[next_idx % len(unique_candidates)]
                profile["next_probe_idx"] = (next_idx + 1) % len(unique_candidates)
                reason = "probe_interval"
            else:
                selected = self._best_candidate(unique_candidates, stats, configured)
                reason = "best_historical"

        self._selected_batch_size = int(selected)
        self._selected_reason = reason
        self._logger.info(
            "batch autotune selected worker_batch_size=%d (%s)",
            self._selected_batch_size,
            self._selected_reason,
        )
        return self._selected_batch_size

    def record(self, *, files_processed: int, elapsed_s: float) -> None:
        if not self._enabled:
            return
        if files_processed < int(self._config.worker_batch_autotune_min_files):
            return
        if elapsed_s <= 0.0:
            return

        data = self._load()
        profiles = data.setdefault("profiles", {})
        profile = profiles.setdefault(
            self._profile_key,
            {"runs": 0, "next_probe_idx": 0, "stats": {}},
        )
        stats = profile.setdefault("stats", {})
        if not isinstance(stats, dict):
            stats = {}
            profile["stats"] = stats
        key = str(int(self._selected_batch_size))
        entry = stats.setdefault(
            key,
            {
                "runs": 0,
                "total_files": 0,
                "total_seconds": 0.0,
                "ema_files_per_s": 0.0,
            },
        )
        runs = int(entry.get("runs", 0)) + 1
        total_files = int(entry.get("total_files", 0)) + int(files_processed)
        total_seconds = float(entry.get("total_seconds", 0.0)) + float(elapsed_s)
        files_per_s = float(files_processed) / float(elapsed_s)
        ema_prev = float(entry.get("ema_files_per_s", 0.0))
        alpha = 0.35
        ema = files_per_s if ema_prev <= 0.0 else ((1.0 - alpha) * ema_prev) + (alpha * files_per_s)

        entry["runs"] = runs
        entry["total_files"] = total_files
        entry["total_seconds"] = total_seconds
        entry["ema_files_per_s"] = ema
        profile["runs"] = int(profile.get("runs", 0)) + 1
        profile["last_selected"] = int(self._selected_batch_size)
        profile["last_reason"] = self._selected_reason
        profile["last_files"] = int(files_processed)
        profile["last_elapsed_s"] = float(elapsed_s)

        self._save(data)
        self._logger.info(
            "batch autotune recorded size=%d files=%d elapsed=%.3fs rate=%.2f files/s",
            int(self._selected_batch_size),
            int(files_processed),
            float(elapsed_s),
            files_per_s,
        )

    def _best_candidate(self, candidates: list[int], stats: dict[str, Any], fallback: int) -> int:
        best_candidate = int(fallback)
        best_score = -1.0
        for candidate in candidates:
            entry = stats.get(str(candidate), {})
            if not isinstance(entry, dict):
                continue
            ema = float(entry.get("ema_files_per_s", 0.0))
            if ema <= 0.0:
                total_files = int(entry.get("total_files", 0))
                total_seconds = float(entry.get("total_seconds", 0.0))
                if total_files > 0 and total_seconds > 0.0:
                    ema = float(total_files) / float(total_seconds)
            if ema > best_score:
                best_score = ema
                best_candidate = int(candidate)
        return best_candidate

    def _make_profile_key(self, config: AppConfig) -> str:
        payload = {
            "root": str(Path(config.root).resolve()),
            "jobs": int(config.jobs),
            "check": bool(config.check),
            "parser_strategy": str(config.parser_strategy),
            "parse_pool_workers": int(config.parse_pool_workers),
        }
        encoded = json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")
        return hashlib.blake2b(encoded, digest_size=16).hexdigest()

    def _load(self) -> dict[str, Any]:
        if self._loaded_data is not None:
            return self._loaded_data
        data: dict[str, Any] = {"version": 1, "profiles": {}}
        if self._path.exists():
            try:
                with self._path.open("r", encoding="utf-8") as handle:
                    loaded = json.load(handle)
                if isinstance(loaded, dict):
                    data = loaded
            except Exception:
                data = {"version": 1, "profiles": {}}
        if not isinstance(data.get("profiles"), dict):
            data["profiles"] = {}
        self._loaded_data = data
        return data

    def _save(self, data: dict[str, Any]) -> None:
        payload = json.dumps(data, ensure_ascii=False, separators=(",", ":"), sort_keys=True)
        AtomicWriter.write_text(self._path, payload + "\n")
