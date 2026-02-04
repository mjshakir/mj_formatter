from __future__ import annotations

from .stub_policy import StubPolicy


class BraceStylePolicy(StubPolicy):
    name = "brace_style"
    description = "Enforce brace style (e.g., K&R)"
    parse_mode = "tree_sitter"
    warning_message = "brace style enforcement not implemented yet"
