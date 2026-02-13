from __future__ import annotations

from typing import Any

from ...parsing.code_context import CodeContext, build_code_context


class CodeContextBuilder:
    def build(
        self,
        path: str,
        text: str,
        clang_ast: Any | None,
        tree_sitter_tree: Any | None,
        project_index_cache: Any | None = None,
    ) -> CodeContext:
        return build_code_context(
            path=path,
            text=text,
            clang_ast=clang_ast,
            tree_sitter_tree=tree_sitter_tree,
            project_index_cache=project_index_cache,
        )
