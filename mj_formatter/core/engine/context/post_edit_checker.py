from __future__ import annotations

from dataclasses import dataclass
from typing import Any


@dataclass(frozen=True)
class PostEditCheckResult:
    accepted: bool
    messages: tuple[str, ...] = ()


class PostEditChecker:
    def __init__(self, parser_manager: Any, clang_args_resolver: Any) -> None:
        self._parser_manager = parser_manager
        self._clang_args = clang_args_resolver

    def validate(self, *, path: str, before_text: str, after_text: str) -> PostEditCheckResult:
        if before_text == after_text:
            return PostEditCheckResult(accepted=True, messages=())

        messages: list[str] = []

        before_tree, _, before_tree_warning = self._parser_manager.parse_tree_sitter(before_text, path)
        after_tree, _, after_tree_warning = self._parser_manager.parse_tree_sitter(after_text, path)

        before_tree_error = self._tree_has_error(before_tree)
        after_tree_error = self._tree_has_error(after_tree)
        if not before_tree_error and after_tree_error:
            messages.append("post-edit check failed: tree-sitter parse quality regressed")

        if before_tree_warning:
            messages.append(f"post-edit check warning (before): {before_tree_warning}")
        if after_tree_warning:
            messages.append(f"post-edit check warning (after): {after_tree_warning}")

        clang_args = tuple(self._clang_args.get_args(path))
        before_tu, before_clang_warning = self._parser_manager.parse_clang(
            self._parser_manager.ClangParseArgs(
                text=before_text,
                path=path,
                args=clang_args,
                include_function_bodies=False,
            )
        )
        after_tu, after_clang_warning = self._parser_manager.parse_clang(
            self._parser_manager.ClangParseArgs(
                text=after_text,
                path=path,
                args=clang_args,
                include_function_bodies=False,
            )
        )

        before_errors = self._clang_error_count(before_tu)
        after_errors = self._clang_error_count(after_tu)

        if before_errors is not None and after_errors is not None and after_errors > before_errors:
            messages.append(
                "post-edit check failed: clang diagnostics increased "
                f"({before_errors} -> {after_errors})"
            )

        if before_clang_warning:
            messages.append(f"post-edit check warning (before): {before_clang_warning}")
        if after_clang_warning:
            messages.append(f"post-edit check warning (after): {after_clang_warning}")

        failed = any(message.startswith("post-edit check failed") for message in messages)
        return PostEditCheckResult(accepted=not failed, messages=tuple(messages))

    def _tree_has_error(self, tree: Any | None) -> bool:
        root = getattr(tree, "root_node", None)
        if root is None:
            return False
        has_error = getattr(root, "has_error", None)
        if callable(has_error):
            try:
                return bool(has_error())
            except Exception:
                return False
        return bool(has_error)

    def _clang_error_count(self, translation_unit: Any | None) -> int | None:
        if translation_unit is None:
            return None
        diagnostics = getattr(translation_unit, "diagnostics", None)
        if diagnostics is None:
            return None
        total = 0
        for diagnostic in diagnostics:
            severity = int(getattr(diagnostic, "severity", 0) or 0)
            # clang.cindex severities: 3=Error, 4=Fatal
            if severity >= 3:
                total += 1
        return total
