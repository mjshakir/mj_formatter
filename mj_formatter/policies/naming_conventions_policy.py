from __future__ import annotations

from .stub_policy import StubPolicy


class NamingConventionsPolicy(StubPolicy):
    name = "naming_conventions"
    description = "Enforce naming prefixes (m_, s_, p_, etc.)"
    parse_mode = "tree_sitter"
    warning_message = "naming convention enforcement not implemented yet"
