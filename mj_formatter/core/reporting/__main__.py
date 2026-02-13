from __future__ import annotations

import argparse
import json
from pathlib import Path


def _parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Show compact summary from metrics JSON.")
    parser.add_argument(
        "--metrics",
        default="scripts/mj_formatter/reports/format_report.jsonl.metrics.json",
        help="Path to metrics JSON",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = _parse_args(argv)
    path = Path(args.metrics)
    if not path.exists():
        print(f"metrics file not found: {path}")
        return 2
    data = json.loads(path.read_text(encoding="utf-8"))
    summary = data.get("summary", {}) if isinstance(data, dict) else {}
    if not isinstance(summary, dict):
        summary = {}
    print(
        "files={files} changed={changed} violations={violations} errors={errors} duration_ms={duration_ms}".format(
            files=summary.get("files", 0),
            changed=summary.get("changed", 0),
            violations=summary.get("violations", 0),
            errors=summary.get("errors", 0),
            duration_ms=f"{float(summary.get('duration_ms', 0.0)):.2f}",
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
