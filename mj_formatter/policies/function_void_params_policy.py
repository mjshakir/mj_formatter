from __future__ import annotations

from .stub_policy import StubPolicy


class FunctionVoidParamsPolicy(StubPolicy):
    name = "function_void_params"
    description = "Require (void) for empty parameter lists and no space before parens"
    parse_mode = "tree_sitter"
    warning_message = "function parameter enforcement not implemented yet"
