from __future__ import annotations

import logging
import time

from .app_config import AppConfig
from .edit import Edit
from .parse_context import ParseContext
from .parser_manager import ParserManager
from .policy_cache import PolicyCache
from .policy_result import PolicyResult
from .violation import Violation
from ..policies.policy_base import Policy
from ..policies.registry import PolicyRegistry
from .policy_selector import PolicySelector
from .policy_factory import PolicyFactory
from .clang_args import ClangArgsResolver
from .parse_control import ParseControl


class FormatterEngine:
    def __init__(self, config: AppConfig) -> None:
        self._config = config
        self._registry = PolicyRegistry()
        self._factory = PolicyFactory(config)
        selector = PolicySelector(config, self._registry)
        self._policy_names = selector.resolve()
        self._policies = self._build_policies()
        self._parser_manager = ParserManager()
        self._policy_cache = PolicyCache(config.policy_cache_path, enabled=config.cache_enabled)
        self._policy_cache.load()
        self._clang_args = ClangArgsResolver(config)
        self._parse_control = ParseControl()
        self._policy_settings = {
            name: (self._config.policy_settings.get(name, {}) or {})
            for name in self._policy_names
        }

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
        profile: dict[str, float] = {}
        parse_modes: dict[str, str] = {}
        clang_attempted = False

        for policy in self._policies:
            start = time.perf_counter()
            if self._config.cache_enabled:
                settings = self._policy_settings.get(policy.name, {})
                key = self._policy_cache.make_key(policy.name, path, current, settings)
                cached = self._policy_cache.get(key)
                if cached is not None:
                    current = cached.text
                    violations.extend(cached.violations)
                    edits.extend(cached.edits)
                    if self._config.profile_enabled:
                        elapsed = (time.perf_counter() - start) * 1000.0
                        profile[policy.name] = profile.get(policy.name, 0.0) + elapsed
                    continue
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
                if context.clang_ast is None and not clang_attempted:
                    clang_ast, warning = self._parser_manager.parse_clang(
                        ParserManager.ClangParseArgs(
                            text=current,
                            path=path,
                            args=tuple(self._clang_args.get_args(path)),
                        )
                    )
                    context.clang_ast = clang_ast
                    clang_attempted = True
                    if warning:
                        context.warnings.append(warning)
                        logger.warning("%s", warning)

            context.text = current
            result = policy.apply(context)
            current = result.text
            violations.extend(result.violations)
            edits.extend(result.edits)
            parse_modes[policy.name] = self._parse_control.backend_for_policy(policy, context).value
            if self._config.profile_enabled:
                elapsed = (time.perf_counter() - start) * 1000.0
                profile[policy.name] = profile.get(policy.name, 0.0) + elapsed
            if self._config.cache_enabled:
                settings = self._policy_settings.get(policy.name, {})
                key = self._policy_cache.make_key(policy.name, path, context.text, settings)
                self._policy_cache.put(key, result)

        self._policy_cache.save()
        return PolicyResult(
            text=current,
            violations=violations,
            edits=edits,
            profile=profile,
            parse_modes=parse_modes,
        )

    def _build_policies(self) -> list[Policy]:
        policies: list[Policy] = []
        for name in self._policy_names:
            policy_config = {}
            if isinstance(self._config.policy_settings.get(name), dict):
                policy_config = self._config.policy_settings.get(name, {})
            policy = self._factory.create(name, policy_config)
            policies.append(policy)
        return policies
