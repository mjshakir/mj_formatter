#!/usr/bin/env python3
from __future__ import annotations

import argparse
import time
from dataclasses import dataclass
from pathlib import Path
from types import SimpleNamespace
import sys

PROJECT_ROOT = Path(__file__).resolve().parents[1]
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

from mj_formatter.core.parsing import ClangArgsResolver
from mj_formatter.core.config import ConfigLoader
from mj_formatter.core.parsing import ParserManager


@dataclass(frozen=True)
class ParseStats:
    files: int
    ok: int
    failed: int
    total_ms: float


def _parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Profile tree-sitter vs clang parser backends")
    parser.add_argument("--config", default="config/config.toml", help="Path to formatter config TOML")
    parser.add_argument("--root", required=True, help="Project root")
    parser.add_argument("--max-files", type=int, default=0, help="Optional cap for number of files")
    return parser.parse_args()


def _build_config(config_path: str, root: str):
    loader = ConfigLoader()
    args = SimpleNamespace(
        config=config_path,
        style=None,
        root=root,
        include=None,
        exclude=None,
        enable=None,
        disable=None,
        jobs=None,
        check=False,
        report=None,
        log_level=None,
        log_file=None,
        verbose=False,
        profile=False,
        backup=None,
        cache=None,
        list_styles=False,
        list_policies=False,
        validate_registry=False,
        undo=False,
        undo_no_delete=False,
    )
    return loader.load(args)


def _collect_files(root: Path, max_files: int) -> list[Path]:
    exts = {".h", ".hh", ".hpp", ".hxx", ".c", ".cc", ".cpp", ".cxx"}
    files = sorted(path for path in root.rglob("*") if path.is_file() and path.suffix.lower() in exts)
    if max_files > 0:
        return files[:max_files]
    return files


def _format_stats(name: str, stats: ParseStats) -> str:
    avg_ms = stats.total_ms / stats.files if stats.files else 0.0
    success_rate = (stats.ok / stats.files * 100.0) if stats.files else 0.0
    return (
        f"{name:12} files={stats.files:4d}  ok={stats.ok:4d}  "
        f"failed={stats.failed:4d}  total_ms={stats.total_ms:10.2f}  "
        f"avg_ms={avg_ms:8.3f}  success={success_rate:6.2f}%"
    )


def main() -> int:
    args = _parse_args()
    config = _build_config(args.config, args.root)
    root = Path(config.root).resolve()
    files = _collect_files(root, args.max_files)
    if not files:
        print(f"no C/C++ files found under {root}")
        return 1

    parser_manager = ParserManager()
    clang_args = ClangArgsResolver(config)

    tree_ok = 0
    tree_failed = 0
    tree_total_ms = 0.0
    clang_ok = 0
    clang_failed = 0
    clang_total_ms = 0.0

    for path in files:
        text = path.read_text(encoding="utf-8", errors="ignore")

        start = time.perf_counter()
        _, _, tree_warning = parser_manager.parse_tree_sitter(text, str(path))
        tree_total_ms += (time.perf_counter() - start) * 1000.0
        if tree_warning:
            tree_failed += 1
        else:
            tree_ok += 1

        start = time.perf_counter()
        _, clang_warning = parser_manager.parse_clang(
            ParserManager.ClangParseArgs(
                text=text,
                path=str(path),
                args=tuple(clang_args.get_args(str(path))),
            )
        )
        clang_total_ms += (time.perf_counter() - start) * 1000.0
        if clang_warning:
            clang_failed += 1
        else:
            clang_ok += 1

    tree_stats = ParseStats(
        files=len(files),
        ok=tree_ok,
        failed=tree_failed,
        total_ms=tree_total_ms,
    )
    clang_stats = ParseStats(
        files=len(files),
        ok=clang_ok,
        failed=clang_failed,
        total_ms=clang_total_ms,
    )

    print(f"root: {root}")
    print(f"files profiled: {len(files)}")
    print(_format_stats("tree-sitter", tree_stats))
    print(_format_stats("clang", clang_stats))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
