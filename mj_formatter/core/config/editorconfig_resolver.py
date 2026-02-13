from __future__ import annotations

from fnmatch import fnmatchcase
from pathlib import Path

from ..types import EditorConfigData, EditorConfigSection


class EditorConfigResolver:
    def __init__(self, root: Path, data: EditorConfigData) -> None:
        self._root = root.resolve()
        self._data = data

    @classmethod
    def discover(cls, root: str | Path) -> "EditorConfigResolver | None":
        root_path = Path(root).resolve()
        candidate = root_path / ".editorconfig"
        if not candidate.exists():
            return None
        data = cls._parse(candidate)
        return cls(root_path, data)

    def resolve(self, path: str | Path) -> dict[str, str]:
        file_path = Path(path).resolve()
        try:
            rel = file_path.relative_to(self._root).as_posix()
        except ValueError:
            rel = file_path.name
        base = file_path.name

        resolved: dict[str, str] = {}
        for section in self._data.sections:
            if any(self._match(pattern, rel, base) for pattern in section.patterns):
                resolved.update(section.properties)
        return resolved

    @classmethod
    def _parse(cls, path: Path) -> EditorConfigData:
        sections: list[EditorConfigSection] = []
        current_patterns: tuple[str, ...] | None = None
        current_props: dict[str, str] = {}

        for raw_line in path.read_text(encoding="utf-8").splitlines():
            line = raw_line.strip()
            if not line or line.startswith("#") or line.startswith(";"):
                continue
            if line.startswith("[") and line.endswith("]"):
                if current_patterns is not None:
                    sections.append(EditorConfigSection(patterns=current_patterns, properties=dict(current_props)))
                pattern_expr = line[1:-1].strip()
                current_patterns = cls._parse_patterns(pattern_expr)
                current_props = {}
                continue
            if "=" not in line or current_patterns is None:
                continue
            key, value = line.split("=", 1)
            key_norm = key.strip().lower()
            value_norm = value.strip()
            if key_norm:
                current_props[key_norm] = value_norm

        if current_patterns is not None:
            sections.append(EditorConfigSection(patterns=current_patterns, properties=dict(current_props)))
        return EditorConfigData(sections=tuple(sections))

    @classmethod
    def _parse_patterns(cls, expr: str) -> tuple[str, ...]:
        chunks: list[str] = []
        current: list[str] = []
        brace_depth = 0
        for ch in expr:
            if ch == "{":
                brace_depth += 1
            elif ch == "}":
                brace_depth = max(0, brace_depth - 1)
            if ch == "," and brace_depth == 0:
                item = "".join(current).strip()
                if item:
                    chunks.extend(cls._expand_braces(item))
                current = []
                continue
            current.append(ch)
        tail = "".join(current).strip()
        if tail:
            chunks.extend(cls._expand_braces(tail))
        return tuple(dict.fromkeys(chunks))

    @classmethod
    def _expand_braces(cls, pattern: str) -> list[str]:
        start = pattern.find("{")
        if start < 0:
            return [pattern]
        depth = 0
        end = -1
        for idx in range(start, len(pattern)):
            ch = pattern[idx]
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    end = idx
                    break
        if end < 0:
            return [pattern]

        prefix = pattern[:start]
        body = pattern[start + 1 : end]
        suffix = pattern[end + 1 :]
        parts = cls._split_brace_options(body)
        expanded: list[str] = []
        for part in parts:
            expanded.extend(cls._expand_braces(prefix + part + suffix))
        return expanded

    @classmethod
    def _split_brace_options(cls, body: str) -> list[str]:
        options: list[str] = []
        current: list[str] = []
        depth = 0
        for ch in body:
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth = max(0, depth - 1)
            if ch == "," and depth == 0:
                options.append("".join(current))
                current = []
                continue
            current.append(ch)
        options.append("".join(current))
        return options

    def _match(self, pattern: str, rel_path: str, base_name: str) -> bool:
        normalized = pattern.replace("\\", "/").strip()
        if not normalized:
            return False
        target = rel_path if "/" in normalized else base_name
        if fnmatchcase(target, normalized):
            return True
        if normalized.startswith("**/"):
            alt = normalized[3:]
            if fnmatchcase(base_name, alt):
                return True
        return False
