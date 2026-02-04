from __future__ import annotations

from .stub_policy import StubPolicy


class ClassLayoutPolicy(StubPolicy):
    name = "class_layout"
    description = "Enforce class access section ordering"
    parse_mode = "tree_sitter"
    warning_message = "class layout enforcement not implemented yet"
