from __future__ import annotations

from datetime import datetime, timezone
import os
from pathlib import Path

from ..config import TomlStore
from ..types import AppConfig


class RunJournal:
    def __init__(self, run_dir: str, logger) -> None:
        self._dir = Path(run_dir)
        self._logger = logger
        self._toml = TomlStore()
        self._path: Path | None = None
        self._run_id = ""

    def start(self, config: AppConfig) -> str:
        self._dir.mkdir(parents=True, exist_ok=True)
        self._warn_stale_runs()
        self._run_id = datetime.now(timezone.utc).strftime("%Y%m%d_%H%M%S_%f")
        self._path = self._dir / f"{self._run_id}.toml"
        payload = {
            "run": {
                "run_id": self._run_id,
                "status": "RUNNING",
                "started_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
                "pid": int(os.getpid()),
                "root": config.root,
                "check": bool(config.check),
                "jobs": int(config.jobs),
                "parser_strategy": str(config.parser_strategy.value if hasattr(config.parser_strategy, "value") else config.parser_strategy),
                "report_path": config.report_path,
            }
        }
        self._toml.write(self._path, payload)
        return self._run_id

    def finish(self, *, status: str, exit_code: int, files: int = 0, changed: int = 0, errors: int = 0) -> None:
        if self._path is None:
            return
        try:
            data = self._toml.load(self._path)
        except Exception:
            data = {}
        run = data.get("run", {})
        if not isinstance(run, dict):
            run = {}
        run.update(
            {
                "status": status,
                "finished_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
                "exit_code": int(exit_code),
                "files": int(files),
                "changed": int(changed),
                "errors": int(errors),
            }
        )
        data["run"] = run
        self._toml.write(self._path, data)

    def _warn_stale_runs(self) -> None:
        for path in sorted(self._dir.glob("*.toml")):
            try:
                data = self._toml.load(path)
            except Exception:
                continue
            run = data.get("run", {})
            if not isinstance(run, dict):
                continue
            status = str(run.get("status", "")).upper()
            if status == "RUNNING":
                run_id = str(run.get("run_id", path.stem))
                started = str(run.get("started_at", "unknown"))
                self._logger.warning("stale running journal detected: run_id=%s started_at=%s", run_id, started)
