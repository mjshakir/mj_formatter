from __future__ import annotations

import logging

from .app_config import AppConfig
from .edit import Edit
from .parse_context import ParseContext
from .parser_manager import ParserManager
from .policy_result import PolicyResult
from .violation import Violation
from ..policies.policy_base import Policy
from ..policies.registry import PolicyRegistry
from .policy_selector import PolicySelector


class FormatterEngine:
    def __init__(self, config: AppConfig) -> None:
        self._config = config
        self._registry = PolicyRegistry()
        selector = PolicySelector(config, self._registry)
        self._policy_names = selector.resolve()
        self._policies = self._build_policies()
        self._parser_manager = ParserManager()

    def apply(self, text: str, path: str) -> PolicyResult:
        logger = logging.getLogger("mj_formatter")
        context = ParseContext(
            text=text,
            path=path,
            tree_sitter_tree=None,
            tree_sitter_lang=None,
            clang_ast=None,
            warnings=[],
        )

        current = context.text
        violations: list[Violation] = []
        edits: list[Edit] = []
        ts_text = current

        for policy in self._policies:
            if policy.parse_mode == "tree_sitter":
                if context.tree_sitter_tree is None or current != ts_text:
                    tree, lang, warning = self._parser_manager.parse_tree_sitter(current, path)
                    context.tree_sitter_tree = tree
                    context.tree_sitter_lang = lang
                    ts_text = current
                    if warning:
                        context.warnings.append(warning)
                        logger.warning("%s", warning)

            if policy.parse_mode == "clang":
                if context.clang_ast is None:
                    context.warnings.append("clang parsing requested but not implemented")
                    logger.warning("clang parsing requested but not implemented")

            context.text = current
            result = policy.apply(context)
            current = result.text
            violations.extend(result.violations)
            edits.extend(result.edits)

        return PolicyResult(text=current, violations=violations, edits=edits)

    def _build_policies(self) -> list[Policy]:
        policies: list[Policy] = []
        for name in self._policy_names:
            policy_config = {}
            if isinstance(self._config.policy_settings.get(name), dict):
                policy_config = self._config.policy_settings.get(name, {})
            policy = self._registry.create(name, policy_config)
            policies.append(policy)
        return policies
