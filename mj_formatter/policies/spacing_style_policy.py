from __future__ import annotations

from ..core.types import Edit
from ..core.types import ParseContext
from ..core.types import PolicyResult
from ..core.types import Violation
from .policy_base import Policy


class SpacingStylePolicy(Policy):
    name = "spacing_style"
    description = "Enforce spacing/indentation style"
    parse_mode = "tree_sitter"

    def apply(self, context: ParseContext) -> PolicyResult:
        use_editorconfig = bool(self._config.get("use_editorconfig", True))
        indent_style = str(self._config.get("indent_style", "spaces_4")).lower()
        tab_width = int(self._config.get("tab_width", 4))
        if use_editorconfig:
            ec_style = context.editorconfig.get("indent_style", "").strip().lower()
            ec_size = context.editorconfig.get("indent_size")
            ec_width = context.editorconfig.get("tab_width")
            if ec_style == "tab":
                indent_style = "tabs"
            elif ec_style == "space":
                try:
                    size = int(ec_size) if ec_size else int(ec_width) if ec_width else 4
                except ValueError:
                    size = 4
                indent_style = "spaces_2" if size <= 2 else "spaces_4"
            for raw in (ec_width, ec_size):
                if raw is None:
                    continue
                try:
                    tab_width = max(1, int(raw))
                    break
                except ValueError:
                    continue
        lines = context.text.splitlines(keepends=True)
        if not lines:
            return PolicyResult(text=context.text, violations=[], edits=[])

        updated_lines: list[str] = []
        edits: list[Edit] = []
        changed = False

        for idx, line in enumerate(lines):
            stripped = line.lstrip("\t ")
            leading = line[: len(line) - len(stripped)]
            if not leading:
                updated_lines.append(line)
                continue
            indent_cols = 0
            for ch in leading:
                indent_cols += tab_width if ch == "\t" else 1
            if indent_style == "tabs":
                tabs = indent_cols // tab_width
                spaces = indent_cols % tab_width
                new_leading = ("\t" * tabs) + (" " * spaces)
            else:
                new_leading = " " * indent_cols
            new_line = new_leading + stripped
            if new_line != line:
                changed = True
                edits.append(
                    Edit(
                        policy=self.name,
                        line=idx + 1,
                        before=line.rstrip("\r\n"),
                        after=new_line.rstrip("\r\n"),
                    )
                )
            updated_lines.append(new_line)

        if not changed:
            return PolicyResult(text=context.text, violations=[], edits=[])

        violation = Violation(
            policy=self.name,
            message=f"Indentation normalized to {indent_style}",
            line=1,
            column=1,
        )
        return PolicyResult(text="".join(updated_lines), violations=[violation], edits=edits)
