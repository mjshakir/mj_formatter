from __future__ import annotations

from .stub_policy import StubPolicy


class PointerBindStylePolicy(StubPolicy):
    name = "pointer_bind_style"
    description = "Enforce pointer/reference binding to type"
    parse_mode = "tree_sitter"
    warning_message = "pointer/reference binding enforcement not implemented yet"
