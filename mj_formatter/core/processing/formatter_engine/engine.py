from __future__ import annotations

import inspect
import logging
import os
from collections import defaultdict, deque
from pathlib import Path

from ...config import EditorConfigResolver
from ...engine.context import CodeContextBuilder, EditGuard, PolicyConfidenceEngine, PostEditChecker
from ...parsing import ParseControl, ParserManager
from ...parsing.clang_args import ClangArgsResolver
from ...policy import PolicyCache, PolicyFactory, PolicySelector, ProjectIndexCache
from ...types import AppConfig, Edit, PipelineRunnerDeps, PolicyResult, Violation
from ....policies.policy_base import Policy
from ....policies.registry import PolicyRegistry
from ..executor_registry import ExecutorRegistry
from ..retry_snapshot_cache import RetrySnapshotCache
from .parse_coordinator import ParseCoordinator
from .pipeline_runner import PolicyPipelineRunner
from .policy_runtime import PolicyRuntime


class FormatterEngine:
    def __init__(self, config: AppConfig) -> None:
        self._config = config
        self._registry = PolicyRegistry()
        self._factory = PolicyFactory(config)
        selector = PolicySelector(config, self._registry)
        self._policy_names = selector.resolve()
        self._policies = self._build_policies()
        self._parser_manager = ParserManager(config.clang_library_paths)
        self._validate_required_parsers()
        policy_cache_path, index_path = self._resolve_cache_paths()
        # In check mode, final file-level check-result cache is faster and safer than
        # per-policy cache churn; keep policy cache for write mode only.
        self._policy_cache_enabled = bool(config.cache_enabled and not config.check)
        # Save policy cache less frequently; we flush explicitly at worker shutdown.
        self._policy_cache = PolicyCache(policy_cache_path, enabled=self._policy_cache_enabled, save_interval=500)
        self._policy_cache.load()
        self._clang_args = ClangArgsResolver(config)
        self._parse_control = ParseControl()
        self._project_index_cache = ProjectIndexCache(index_path, enabled=True)
        self._project_index_cache.load()
        self._code_context_builder = CodeContextBuilder()
        self._edit_guard = EditGuard()
        self._editorconfig = EditorConfigResolver.discover(Path(config.root))
        executor_registry = ExecutorRegistry()
        self._parse_pool = executor_registry.get_parse_pool(config.parse_pool_workers)
        self._post_edit_pool = executor_registry.get_post_edit_pool()
        self._post_edit_checker = PostEditChecker(self._parser_manager, self._clang_args)
        self._retry_snapshot_cache = RetrySnapshotCache(config.retry_snapshot_cache_size)
        self._policy_settings = {
            name: (self._config.policy_settings.get(name, {}) or {})
            for name in self._policy_names
        }
        self._confidence_engine = PolicyConfidenceEngine(self._config, self._policy_settings)
        self._policy_cache_versions = {
            policy.name: self._policy_cache_version(policy)
            for policy in self._policies
        }
        self._policy_cache_settings = {
            name: self._cache_settings(name)
            for name in self._policy_names
        }
        self._policy_cache_settings_hashes = {
            name: self._policy_cache.hash_settings(settings)
            for name, settings in self._policy_cache_settings.items()
        }

        parse_coordinator = ParseCoordinator(
            parser_manager=self._parser_manager,
            clang_args=self._clang_args,
            parse_control=self._parse_control,
            parse_pool=self._parse_pool,
        )
        policy_runtime = PolicyRuntime(
            config=self._config,
            policy_settings=self._policy_settings,
            edit_guard=self._edit_guard,
            confidence_engine=self._confidence_engine,
        )
        self._pipeline_runner = PolicyPipelineRunner(
            PipelineRunnerDeps(
                config=self._config,
                policies=tuple(self._policies),
                policy_settings=self._policy_settings,
                policy_cache_enabled=self._policy_cache_enabled,
                policy_cache=self._policy_cache,
                policy_cache_settings=self._policy_cache_settings,
                policy_cache_settings_hashes=self._policy_cache_settings_hashes,
                parse_coordinator=parse_coordinator,
                code_context_builder=self._code_context_builder,
                project_index_cache=self._project_index_cache,
                policy_runtime=policy_runtime,
                editorconfig_resolver=self._editorconfig,
            )
        )

    def flush_caches(self) -> None:
        self._policy_cache.save()
        self._project_index_cache.save()

    def apply(self, text: str, path: str) -> PolicyResult:
        logger = logging.getLogger("mj_formatter")
        base_confidence_threshold = float(self._config.confidence_blocking_min)
        max_confidence_threshold = float(self._config.post_edit_retry_confidence_max)
        max_confidence_threshold = max(0.0, min(1.0, max_confidence_threshold))
        retry_step = float(self._config.post_edit_retry_confidence_step)
        retry_step = max(0.0, retry_step)
        max_attempts = max(0, int(self._config.post_edit_retry_max_attempts))
        retry_enabled = bool(self._config.post_edit_retry_enabled)

        retry_queue: deque[tuple[int, float, frozenset[str]]] = deque(
            [(0, base_confidence_threshold, frozenset())]
        )
        retry_warnings: list[str] = []
        final_result = PolicyResult(text=text, violations=[], edits=[], profile={}, parse_modes={}, warnings=[])

        while retry_queue:
            attempt_index, confidence_threshold, blocked = retry_queue.popleft()
            blocked_policies = set(blocked)
            confidence_policies = (
                set(self._policy_names) if attempt_index > 0 else set(self._config.confidence_blocking_policies)
            )
            use_cache = bool(self._policy_cache_enabled and attempt_index == 0 and not blocked_policies)
            cached_pass_result = self._retry_snapshot_cache.get(
                path=path,
                text=text,
                confidence_threshold=confidence_threshold,
                confidence_policies=confidence_policies,
                blocked_policies=blocked_policies,
            )
            if cached_pass_result is not None:
                pass_result = cached_pass_result
            else:
                pass_result = self._pipeline_runner.run(
                    text=text,
                    path=path,
                    logger=logger,
                    confidence_threshold=confidence_threshold,
                    confidence_policies=confidence_policies,
                    blocked_policies=blocked_policies,
                    use_cache=use_cache,
                    retry_attempt=attempt_index,
                )
                self._retry_snapshot_cache.put(
                    path=path,
                    text=text,
                    confidence_threshold=confidence_threshold,
                    confidence_policies=confidence_policies,
                    blocked_policies=blocked_policies,
                    result=pass_result,
                )

            merged_warnings = list(dict.fromkeys([*retry_warnings, *(pass_result.warnings or [])]))
            if not self._config.post_edit_check_enabled or pass_result.text == text:
                final_result = PolicyResult(
                    text=pass_result.text,
                    violations=pass_result.violations,
                    edits=pass_result.edits,
                    profile=pass_result.profile,
                    parse_modes=pass_result.parse_modes,
                    warnings=merged_warnings,
                )
                break

            future = self._post_edit_pool.submit(
                self._post_edit_checker.validate,
                path=path,
                before_text=text,
                after_text=pass_result.text,
            )
            check = future.result()
            for message in check.messages:
                logger.warning("%s", message)

            if check.accepted:
                final_result = PolicyResult(
                    text=pass_result.text,
                    violations=pass_result.violations,
                    edits=pass_result.edits,
                    profile=pass_result.profile,
                    parse_modes=pass_result.parse_modes,
                    warnings=list(dict.fromkeys([*merged_warnings, *check.messages])),
                )
                break

            retry_warnings.extend(check.messages)
            retry_warnings.append(
                f"post-edit retry: attempt {attempt_index + 1} failed at confidence {confidence_threshold:.2f}"
            )
            can_retry = (
                retry_enabled
                and retry_step > 0.0
                and attempt_index < max_attempts
                and confidence_threshold < max_confidence_threshold
            )
            if can_retry:
                next_confidence = min(max_confidence_threshold, confidence_threshold + retry_step)
                next_blocked = self._collect_retry_blocked_policies(blocked_policies, pass_result.edits)
                retry_warnings.append(
                    "post-edit retry: scheduling attempt "
                    f"{attempt_index + 2} with confidence {next_confidence:.2f}"
                )
                logger.warning(
                    "post-edit retry scheduled for %s (attempt=%d, confidence=%.2f, blocked=%s)",
                    path,
                    attempt_index + 2,
                    next_confidence,
                    ", ".join(sorted(next_blocked)) if next_blocked else "<none>",
                )
                retry_queue.append((attempt_index + 1, next_confidence, frozenset(next_blocked)))
                continue

            final_result = PolicyResult(
                text=text,
                violations=[
                    *pass_result.violations,
                    Violation(
                        policy="post_edit_check",
                        message=(
                            "Post-edit parser check failed after retries; reverted file changes "
                            f"(attempts={attempt_index + 1}, confidence={confidence_threshold:.2f})"
                        ),
                        line=1,
                        column=1,
                    ),
                ],
                edits=[],
                profile=pass_result.profile,
                parse_modes=pass_result.parse_modes,
                warnings=list(dict.fromkeys([*merged_warnings, *check.messages])),
            )
            break

        return final_result

    def _collect_retry_blocked_policies(self, blocked: set[str], edits: list[Edit]) -> set[str]:
        if not edits:
            return set(blocked)
        counts: dict[str, int] = defaultdict(int)
        for edit in edits:
            if edit.policy:
                counts[edit.policy] += 1
        if not counts:
            return set(blocked)
        top_policy = sorted(counts.items(), key=lambda item: (-item[1], item[0]))[0][0]
        next_blocked = set(blocked)
        next_blocked.add(top_policy)
        return next_blocked

    def _build_policies(self) -> list[Policy]:
        policies: list[Policy] = []
        for name in self._policy_names:
            policy_config = {}
            if isinstance(self._config.policy_settings.get(name), dict):
                policy_config = self._config.policy_settings.get(name, {})
            policy = self._factory.create(name, policy_config)
            policies.append(policy)
        return policies

    def _cache_settings(self, policy_name: str) -> dict[str, object]:
        settings = dict(self._policy_settings.get(policy_name, {}))
        settings["_policy_impl_version"] = self._policy_cache_versions.get(policy_name, "unknown")
        return settings

    def _policy_cache_version(self, policy: Policy) -> str:
        source = inspect.getsourcefile(policy.__class__)
        if source is None:
            return f"{policy.__class__.__module__}:{policy.__class__.__qualname__}"
        try:
            mtime_ns = Path(source).stat().st_mtime_ns
        except OSError:
            mtime_ns = 0
        return f"{policy.__class__.__module__}:{policy.__class__.__qualname__}:{mtime_ns}"

    def _resolve_cache_paths(self) -> tuple[str, str]:
        policy_cache_path = self._config.policy_cache_path
        index_path = str(Path(self._config.policy_cache_path).with_name("project_index_cache.bin"))
        run_token = os.environ.get("MJ_FORMATTER_CACHE_RUN")
        shard = os.environ.get("MJ_FORMATTER_CACHE_SHARD")
        if not run_token or not shard:
            return policy_cache_path, index_path

        base = Path(policy_cache_path)
        shard_root = base.parent / ".shards" / run_token
        policy_cache_path = str(shard_root / f"{base.name}.{shard}.bin")
        index_path = str(shard_root / f"project_index_cache.{shard}.bin")
        return policy_cache_path, index_path

    def _validate_required_parsers(self) -> None:
        failures: list[str] = []
        if not self._parser_manager.has_tree_sitter():
            reason = self._parser_manager.tree_sitter_error() or "unknown tree-sitter initialization failure"
            failures.append(f"tree-sitter unavailable: {reason}")
        if not self._parser_manager.has_clang():
            reason = self._parser_manager.clang_error() or "unknown clang initialization failure"
            failures.append(f"clang unavailable: {reason}")
        if failures:
            details = "; ".join(failures)
            raise RuntimeError(
                "Required parser backends missing. "
                f"Hybrid mode requires both tree-sitter and clang. Details: {details}"
            )


__all__ = ["FormatterEngine"]
