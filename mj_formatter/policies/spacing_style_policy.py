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

    def __init__(self, config: dict[str, object]) -> None:
        super().__init__(config)
        self._use_editorconfig = self._required_bool("use_editorconfig")
        self._indent_style = self._required_indent_style("indent_style")
        self._tab_width = self._required_int("tab_width", minimum=1)

    def apply(self, context: ParseContext) -> PolicyResult:
        use_editorconfig = self._use_editorconfig
        indent_style = self._indent_style
        tab_width = self._tab_width
        if use_editorconfig:
            ec_style = context.editorconfig.get("indent_style", "").strip().lower()
            ec_size = context.editorconfig.get("indent_size")
            ec_width = context.editorconfig.get("tab_width")
            if ec_style == "tab":
                indent_style = "tabs"
            elif ec_style == "space":
                try:
                    size = int(ec_size) if ec_size else int(ec_width) if ec_width else tab_width
                except ValueError:
                    size = tab_width
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

    def _required_bool(self, key: str) -> bool:
        value = self._config.get(key)
        if not isinstance(value, bool):
            raise ValueError(f"spacing_style: missing required boolean config key '{key}'")
        return value

    def _required_int(self, key: str, *, minimum: int | None = None) -> int:
        value = self._config.get(key)
        if value is None:
            raise ValueError(f"spacing_style: missing required integer config key '{key}'")
        try:
            parsed = int(value)
        except (TypeError, ValueError) as exc:
            raise ValueError(f"spacing_style: invalid integer config key '{key}'") from exc
        if minimum is not None and parsed < minimum:
            raise ValueError(f"spacing_style: config key '{key}' must be >= {minimum}")
        return parsed

    def _required_indent_style(self, key: str) -> str:
        value = self._config.get(key)
        if value is None:
            raise ValueError(f"spacing_style: missing required config key '{key}'")
        style = str(value).strip().lower()
        if style not in {"spaces_2", "spaces_4", "tabs"}:
            raise ValueError(
                "spacing_style: config key 'indent_style' must be one of "
                "'spaces_2', 'spaces_4', or 'tabs'"
            )
        return style
