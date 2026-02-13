from __future__ import annotations

import argparse
from types import SimpleNamespace

from .config_loader import ConfigLoader


def _parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Validate formatter config TOML.")
    parser.add_argument("--config", default="config/config.toml", help="Path to TOML config")
    parser.add_argument("--root", default=None, help="Optional root override")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = _parse_args(argv)
    cli = SimpleNamespace(
        config=args.config,
        style=None,
        root=args.root,
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
        parser_strategy=None,
        parse_pool_workers=None,
        post_edit_check=None,
        backup=None,
        cache=None,
        list_styles=False,
        list_policies=False,
        validate_registry=False,
        undo=False,
        undo_no_delete=False,
    )
    config = ConfigLoader().load(cli)
    print(f"config ok: root={config.root} parser_strategy={config.parser_strategy.value} jobs={config.jobs}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

