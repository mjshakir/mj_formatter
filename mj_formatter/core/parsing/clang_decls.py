from __future__ import annotations

from pathlib import Path
from typing import Any, Iterable

from ..types import ClangFunctionDecl, ClangVarDecl


class ClangDeclCollector:
    def __init__(self, tu: Any, path: str) -> None:
        self._tu = tu
        self._path = str(Path(path).resolve())

    def functions(self) -> list[ClangFunctionDecl]:
        if self._tu is None:
            return []
        from clang import cindex

        decls: list[ClangFunctionDecl] = []
        for cursor in self._walk(self._tu.cursor):
            if cursor.kind not in {
                cindex.CursorKind.FUNCTION_DECL,
                cindex.CursorKind.CXX_METHOD,
                cindex.CursorKind.CONSTRUCTOR,
                cindex.CursorKind.DESTRUCTOR,
                cindex.CursorKind.FUNCTION_TEMPLATE,
            }:
                continue
            if not self._is_in_main_file(cursor):
                continue
            name = cursor.spelling or cursor.displayname or ""
            if not name:
                continue
            extent = cursor.extent
            params_start, params_end = self._parameter_span(cursor)
            decls.append(
                ClangFunctionDecl(
                    name=name,
                    start=extent.start.offset,
                    end=extent.end.offset,
                    params_start=params_start,
                    params_end=params_end,
                    is_definition=cursor.is_definition(),
                    kind=str(cursor.kind),
                    line=int(getattr(cursor.location, "line", 1) or 1),
                    column=int(getattr(cursor.location, "column", 1) or 1),
                )
            )
        return decls

    def variables(self) -> list[ClangVarDecl]:
        if self._tu is None:
            return []
        from clang import cindex

        decls: list[ClangVarDecl] = []
        for cursor in self._walk(self._tu.cursor):
            if cursor.kind not in {
                cindex.CursorKind.VAR_DECL,
                cindex.CursorKind.FIELD_DECL,
            }:
                continue
            if not self._is_in_main_file(cursor):
                continue
            name = cursor.spelling or ""
            if not name:
                continue
            extent = cursor.extent
            kind = "field" if cursor.kind == cindex.CursorKind.FIELD_DECL else "var"
            decls.append(
                ClangVarDecl(
                    name=name,
                    start=extent.start.offset,
                    end=extent.end.offset,
                    kind=kind,
                    line=int(getattr(cursor.location, "line", 1) or 1),
                    column=int(getattr(cursor.location, "column", 1) or 1),
                )
            )
        return decls

    def _parameter_span(self, cursor: Any) -> tuple[int | None, int | None]:
        try:
            tokens = list(cursor.get_tokens())
        except Exception:
            return None, None
        start = None
        depth = 0
        for token in tokens:
            if token.spelling == "(":
                if start is None:
                    start = token.extent.start.offset
                    depth = 1
                else:
                    depth += 1
                continue
            if token.spelling == ")":
                if start is None:
                    continue
                depth -= 1
                if depth == 0:
                    return start, token.extent.end.offset
        return None, None

    def _walk(self, cursor: Any) -> Iterable[Any]:
        stack = [cursor]
        while stack:
            current = stack.pop()
            yield current
            try:
                children = list(current.get_children())
            except Exception:
                children = []
            stack.extend(reversed(children))

    def _is_in_main_file(self, cursor: Any) -> bool:
        loc = cursor.location
        if loc is None or loc.file is None:
            return False
        try:
            return str(Path(str(loc.file)).resolve()) == self._path
        except Exception:
            return False
