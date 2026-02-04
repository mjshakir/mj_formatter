from __future__ import annotations

from .stub_policy import StubPolicy


class SpacingStylePolicy(StubPolicy):
    name = "spacing_style"
    description = "Enforce spacing/indentation style"
    parse_mode = "text"
    warning_message = "spacing style enforcement not implemented yet"
