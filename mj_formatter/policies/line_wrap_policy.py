from __future__ import annotations

from .stub_policy import StubPolicy


class LineWrapPolicy(StubPolicy):
    name = "line_wrap"
    description = "Wrap lines to a max column width"
    parse_mode = "text"
    warning_message = "line wrapping not implemented yet"
