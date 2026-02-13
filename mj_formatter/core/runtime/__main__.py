from __future__ import annotations

import argparse
from pathlib import Path


def _parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="List recent run journals.")
    parser.add_argument("--runs", default="scripts/mj_formatter/runs", help="Run journal directory")
    parser.add_argument("--limit", type=int, default=5, help="Max entries to print")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = _parse_args(argv)
    runs_dir = Path(args.runs)
    if not runs_dir.exists():
        print(f"run journal dir not found: {runs_dir}")
        return 2
    files = sorted(runs_dir.glob("*.toml"), reverse=True)[: max(1, int(args.limit))]
    if not files:
        print("no run journals found")
        return 0
    for path in files:
        print(path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

