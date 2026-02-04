from __future__ import annotations

import re

from .structs import TableData, TableStyle


class TablePrinter:
    _ansi_re = re.compile(r"\x1b\[[0-9;]*m")

    def __init__(self, style: TableStyle) -> None:
        self._style = style

    def render(self, table: TableData) -> str:
        rows = self._normalize_rows(table)
        widths = self._column_widths(rows)
        widths = self._constrain_widths(widths, len(rows[0]))
        lines = [self._format_row(rows[0], widths)]
        lines.append(self._separator_line(widths))
        for row in rows[1:]:
            lines.append(self._format_row(row, widths))
        return "\n".join(lines)

    def _normalize_rows(self, table: TableData) -> list[list[str]]:
        headers = table.headers
        rows = [headers] + table.rows
        cols = len(headers)
        normalized: list[list[str]] = []
        for row in rows:
            row = list(row)
            if len(row) < cols:
                row.extend([""] * (cols - len(row)))
            normalized.append(row[:cols])
        return normalized

    def _column_widths(self, rows: list[list[str]]) -> list[int]:
        widths = [0] * len(rows[0])
        for row in rows:
            for idx, cell in enumerate(row):
                widths[idx] = max(widths[idx], self._visible_len(cell))
        return widths

    def _constrain_widths(self, widths: list[int], cols: int) -> list[int]:
        total = sum(widths) + self._style.padding * (cols - 1)
        if total <= self._style.max_width:
            return widths
        if cols == 0:
            return widths
        overflow = total - self._style.max_width
        last = max(10, widths[-1] - overflow)
        widths[-1] = last
        return widths

    def _format_row(self, row: list[str], widths: list[int]) -> str:
        padded: list[str] = []
        for idx, cell in enumerate(row):
            cell_text = self._truncate(cell, widths[idx])
            pad = widths[idx] - self._visible_len(cell_text)
            padded.append(cell_text + " " * pad)
        return (" " * self._style.padding).join(padded)

    def _separator_line(self, widths: list[int]) -> str:
        parts = ["-" * width for width in widths]
        return (" " * self._style.padding).join(parts)

    def _visible_len(self, text: str) -> int:
        return len(self._ansi_re.sub("", text))

    def _truncate(self, text: str, width: int) -> str:
        plain = self._ansi_re.sub("", text)
        if len(plain) <= width:
            return text
        if width <= 3:
            return plain[:width]
        return plain[: width - 3] + "..."
