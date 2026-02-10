from __future__ import annotations

import json
from typing import Any
from dataclasses import asdict
from pathlib import Path
from typing import Iterable

from .file_result import FileResult


class ReportWriter:
    def __init__(self, report_path: str) -> None:
        self._report_path = Path(report_path)
        self._json_dumps = json.dumps
        self._json_dump = json.dump
        try:  # optional faster serializer
            import orjson  # type: ignore

            self._json_dumps = lambda obj, **_: orjson.dumps(obj).decode("utf-8")
            self._json_dump = lambda obj, handle, **_: handle.write(  # type: ignore[assignment]
                orjson.dumps(obj, option=orjson.OPT_INDENT_2).decode("utf-8")
            )
        except Exception:
            pass

    def write(self, results: Iterable[FileResult]) -> None:
        self._report_path.parent.mkdir(parents=True, exist_ok=True)
        summary = {
            "files": 0,
            "changed": 0,
            "errors": 0,
            "violations": 0,
            "policies": {},
        }

        with self._report_path.open("w", encoding="utf-8") as handle:
            for result in results:
                summary["files"] += 1
                if result.changed:
                    summary["changed"] += 1
                if result.error:
                    summary["errors"] += 1
                summary["violations"] += len(result.violations)
                for violation in result.violations:
                    summary["policies"].setdefault(violation.policy, 0)
                    summary["policies"][violation.policy] += 1
                handle.write(self._json_dumps(asdict(result), ensure_ascii=False))
                handle.write("\n")

        summary_path = self._report_path.with_suffix(".summary.json")
        with summary_path.open("w", encoding="utf-8") as handle:
            self._json_dump(summary, handle, indent=2)
