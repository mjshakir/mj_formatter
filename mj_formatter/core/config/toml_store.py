from __future__ import annotations

from collections.abc import Mapping
from pathlib import Path
from typing import Any

from ..utilities import AtomicWriter


class TomlStore:
    def load(self, path: Path) -> dict[str, Any]:
        try:
            import tomllib  # Python 3.11+
        except ModuleNotFoundError:  # pragma: no cover - fallback
            import tomli as tomllib  # type: ignore

        with path.open("rb") as handle:
            data = tomllib.load(handle)
        if not isinstance(data, dict):
            raise ValueError(f"Invalid TOML object at {path}")
        return data

    def write(self, path: Path, payload: Mapping[str, Any] | str) -> None:
        if isinstance(payload, str):
            content = payload
        else:
            content = self.dumps(payload)
        AtomicWriter.write_text(path, content)

    def dumps(self, payload: Mapping[str, Any]) -> str:
        lines = self._render_table(payload, prefix=None)
        return "\n".join(lines).rstrip() + "\n"

    def _render_table(self, table: Mapping[str, Any], prefix: str | None) -> list[str]:
        lines: list[str] = []
        simple_items: list[tuple[str, Any]] = []
        table_items: list[tuple[str, Mapping[str, Any]]] = []
        array_table_items: list[tuple[str, list[Mapping[str, Any]]]] = []

        for key, value in table.items():
            if isinstance(value, Mapping):
                table_items.append((str(key), value))
                continue
            if self._is_array_of_tables(value):
                entries = [item for item in value if isinstance(item, Mapping)]
                array_table_items.append((str(key), entries))
                continue
            simple_items.append((str(key), value))

        if prefix is not None:
            lines.append(f"[{prefix}]")

        for key, value in simple_items:
            lines.append(f"{key} = {self._format_value(value)}")

        for key, value in table_items:
            full_key = f"{prefix}.{key}" if prefix else key
            if lines:
                lines.append("")
            lines.extend(self._render_table(value, prefix=full_key))

        for key, entries in array_table_items:
            full_key = f"{prefix}.{key}" if prefix else key
            for entry in entries:
                if lines:
                    lines.append("")
                lines.append(f"[[{full_key}]]")
                entry_lines = self._render_table(entry, prefix=None)
                lines.extend(entry_lines)

        return lines

    def _is_array_of_tables(self, value: Any) -> bool:
        if not isinstance(value, list):
            return False
        if not value:
            return False
        return all(isinstance(item, Mapping) for item in value)

    def _format_value(self, value: Any) -> str:
        if isinstance(value, bool):
            return "true" if value else "false"
        if isinstance(value, int):
            return str(value)
        if isinstance(value, float):
            return repr(value)
        if isinstance(value, str):
            escaped = value.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")
            return f'"{escaped}"'
        if isinstance(value, list):
            return "[" + ", ".join(self._format_value(item) for item in value) + "]"
        raise TypeError(f"Unsupported TOML value type: {type(value)!r}")
