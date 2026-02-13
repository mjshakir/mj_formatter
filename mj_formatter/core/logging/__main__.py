from __future__ import annotations

import logging

from .log_setup import LogSetup


def main() -> int:
    logger = LogSetup().configure("DEBUG", None, force=True)
    logger.debug("debug color preview")
    logger.info("info default preview")
    logger.warning("warning color preview")
    logger.error("error color preview")
    logger.critical("critical color preview")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

