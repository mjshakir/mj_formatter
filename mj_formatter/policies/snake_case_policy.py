from __future__ import annotations

from .stub_policy import StubPolicy


class SnakeCasePolicy(StubPolicy):
    name = "snake_case"
    description = "Enforce snake_case for variables/functions"
    parse_mode = "tree_sitter"
    warning_message = "snake_case enforcement not implemented yet"
