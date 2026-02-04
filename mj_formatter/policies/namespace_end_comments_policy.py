from __future__ import annotations

from .stub_policy import StubPolicy


class NamespaceEndCommentsPolicy(StubPolicy):
    name = "namespace_end_comments"
    description = "Add end comments for namespace/class/struct/function blocks"
    parse_mode = "tree_sitter"
    warning_message = "end-comment enforcement not implemented yet"
