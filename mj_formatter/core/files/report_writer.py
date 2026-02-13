from __future__ import annotations

import json
from typing import Any
from dataclasses import asdict
from pathlib import Path
from typing import Iterable

from ..types import FileResult
from ..utilities import AtomicWriter


class ReportWriter:
    def __init__(self, report_path: str) -> None:
        self._report_path = Path(report_path)
        self._json_dumps = json.dumps
        try:  # optional faster serializer
            import orjson  # type: ignore

            self._json_dumps = lambda obj, **_: orjson.dumps(obj).decode("utf-8")
        except Exception:
            pass

    def write(self, results: Iterable[FileResult]) -> None:
        summary = {
            "files": 0,
            "changed": 0,
            "errors": 0,
            "violations": 0,
            "policies": {},
        }
        lines: list[str] = []

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
            lines.append(self._json_dumps(asdict(result), ensure_ascii=False))
        report_text = "\n".join(lines) + ("\n" if lines else "")
        AtomicWriter.write_text(self._report_path, report_text)

        summary_path = self._report_path.with_suffix(".summary.json")
        summary_text = self._json_dumps(summary, ensure_ascii=False)
        AtomicWriter.write_text(summary_path, summary_text + "\n")
