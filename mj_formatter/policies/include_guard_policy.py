from __future__ import annotations

from .stub_policy import StubPolicy


class IncludeGuardPolicy(StubPolicy):
    name = "include_guards"
    description = "Ensure include guards or #pragma once"
    parse_mode = "text"
    warning_message = "include guard enforcement not implemented yet"
