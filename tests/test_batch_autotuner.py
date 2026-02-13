from __future__ import annotations

import logging
from pathlib import Path

from mj_formatter.core.runtime.batch_autotuner import BatchAutoTuner
from mj_formatter.core.types import AppConfig


def _config(path: Path, *, enabled: bool) -> AppConfig:
    return AppConfig(
        root=".",
        jobs=4,
        worker_batch_size=2,
        worker_batch_autotune_enabled=enabled,
        worker_batch_autotune_path=str(path),
        worker_batch_autotune_candidates=(1, 2),
        worker_batch_autotune_probe_interval=12,
        worker_batch_autotune_min_files=4,
    )


def test_batch_autotuner_disabled_returns_configured(tmp_path: Path) -> None:
    tuner = BatchAutoTuner(_config(tmp_path / "tune.json", enabled=False), logging.getLogger("test"))
    assert tuner.choose_batch_size(files_to_process=100, jobs=4) == 2
    tuner.record(files_processed=100, elapsed_s=10.0)
    assert not (tmp_path / "tune.json").exists()


def test_batch_autotuner_bootstrap_then_best(tmp_path: Path) -> None:
    path = tmp_path / "tune.json"
    cfg = _config(path, enabled=True)
    logger = logging.getLogger("test")

    run1 = BatchAutoTuner(cfg, logger)
    assert run1.choose_batch_size(files_to_process=100, jobs=4) == 1
    run1.record(files_processed=100, elapsed_s=100.0)  # 1 file/s
    assert path.exists()

    run2 = BatchAutoTuner(cfg, logger)
    assert run2.choose_batch_size(files_to_process=100, jobs=4) == 2
    run2.record(files_processed=100, elapsed_s=20.0)  # 5 files/s

    run3 = BatchAutoTuner(cfg, logger)
    assert run3.choose_batch_size(files_to_process=100, jobs=4) == 2
