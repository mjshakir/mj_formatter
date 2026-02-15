from __future__ import annotations

import logging
from concurrent.futures import Future

from ...parsing import ParseControl, ParserManager
from ...parsing.clang_args import ClangArgsResolver
from ...types import ParseBackend, ParseContext, ParseState
from ....policies.policy_base import Policy


class ParseCoordinator:
    def __init__(
        self,
        *,
        parser_manager: ParserManager,
        clang_args: ClangArgsResolver,
        parse_control: ParseControl,
        parse_pool: object,
    ) -> None:
        self._parser_manager = parser_manager
        self._clang_args = clang_args
        self._parse_control = parse_control
        self._parse_pool = parse_pool

    def ensure_parsed(
        self,
        *,
        policy: Policy,
        context: ParseContext,
        text: str,
        path: str,
        include_bodies: bool,
        policy_needs_code_context: bool,
        state: ParseState,
        logger: logging.Logger,
    ) -> tuple[ParseState, ParseBackend]:
        target_tree, target_clang = self._resolve_parse_targets(policy, policy_needs_code_context)

        need_tree_parse = target_tree and (context.tree_sitter_tree is None or text != state.ts_text)
        need_clang_parse = target_clang and (
            context.clang_ast is None
            or text != state.clang_text
            or (include_bodies and not state.clang_has_bodies)
        )

        ts_text = state.ts_text
        clang_text = state.clang_text
        clang_has_bodies = state.clang_has_bodies

        if need_tree_parse and need_clang_parse:
            self._parse_tree_and_clang_parallel(
                context=context,
                text=text,
                path=path,
                include_bodies=include_bodies,
                logger=logger,
            )
            ts_text = text
            clang_text = text
            clang_has_bodies = include_bodies
        else:
            if need_tree_parse:
                tree, lang, warning = self._parser_manager.parse_tree_sitter(text, path)
                context.tree_sitter_tree = tree
                context.tree_sitter_lang = lang
                ts_text = text
                if warning:
                    context.warnings.append(warning)
                    logger.warning("%s", warning)

            if need_clang_parse:
                clang_ast, warning = self._parser_manager.parse_clang(
                    ParserManager.ClangParseArgs(
                        text=text,
                        path=path,
                        args=tuple(self._clang_args.get_args(path)),
                        include_function_bodies=include_bodies,
                    )
                )
                context.clang_ast = clang_ast
                clang_text = text
                clang_has_bodies = include_bodies
                if warning:
                    context.warnings.append(warning)
                    logger.warning("%s", warning)

        policy_backend = self._parse_control.backend_for_policy(policy, context)
        next_state = ParseState(ts_text=ts_text, clang_text=clang_text, clang_has_bodies=clang_has_bodies)
        return next_state, policy_backend

    def _resolve_parse_targets(self, policy: Policy, needs_code_context: bool) -> tuple[bool, bool]:
        _ = policy
        _ = needs_code_context
        # Hybrid-only architecture: always build both syntactic and semantic context.
        return True, True

    def _parse_tree_and_clang_parallel(
        self,
        *,
        context: ParseContext,
        text: str,
        path: str,
        include_bodies: bool,
        logger: logging.Logger,
    ) -> None:
        def parse_tree() -> tuple[object | None, str | None, str | None]:
            return self._parser_manager.parse_tree_sitter(text, path)

        def parse_clang() -> tuple[object | None, str | None]:
            return self._parser_manager.parse_clang(
                ParserManager.ClangParseArgs(
                    text=text,
                    path=path,
                    args=tuple(self._clang_args.get_args(path)),
                    include_function_bodies=include_bodies,
                )
            )

        tree_future: Future = self._parse_pool.submit(parse_tree)
        clang_future: Future = self._parse_pool.submit(parse_clang)
        tree, lang, tree_warning = tree_future.result()
        clang_ast, clang_warning = clang_future.result()

        context.tree_sitter_tree = tree
        context.tree_sitter_lang = lang
        if tree_warning:
            context.warnings.append(tree_warning)
            logger.warning("%s", tree_warning)

        context.clang_ast = clang_ast
        if clang_warning:
            context.warnings.append(clang_warning)
            logger.warning("%s", clang_warning)


__all__ = ["ParseCoordinator", "ParseState"]
