from __future__ import annotations

import logging
import time
from pathlib import Path
import inspect
import os
from collections import defaultdict, deque
from concurrent.futures import Future

from ..types import AppConfig
from ..types import Edit
from ..types import ParseContext
from ..types import ParseBackend
from ..parsing import ParserManager, ParseControl, ParserStrategy
from ..parsing.clang_args import ClangArgsResolver
from ..policy import (
    ConflictDetectorConfig,
    PolicyCache,
    PolicyConflictDetector,
    PolicyFactory,
    PolicySelector,
    PolicySuppression,
    ProjectIndexCache,
)
from ..types import PolicyResult
from ..types import Violation
from ...policies.policy_base import Policy
from ...policies.registry import PolicyRegistry
from ..engine.context import CodeContextBuilder, EditGuard, PostEditChecker, TouchContract
from ..config import EditorConfigResolver
from .executor_registry import ExecutorRegistry
from .retry_snapshot_cache import RetrySnapshotCache


class FormatterEngine:
    def __init__(self, config: AppConfig) -> None:
        self._config = config
        self._registry = PolicyRegistry()
        self._factory = PolicyFactory(config)
        selector = PolicySelector(config, self._registry)
        self._policy_names = selector.resolve()
        self._policies = self._build_policies()
        self._parser_manager = ParserManager(config.clang_library_paths)
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
        self._parser_strategy = ParserStrategy.from_value(config.parser_strategy)
        executor_registry = ExecutorRegistry()
        self._parse_pool = executor_registry.get_parse_pool(config.parse_pool_workers)
        self._post_edit_pool = executor_registry.get_post_edit_pool()
        self._post_edit_checker = PostEditChecker(self._parser_manager, self._clang_args)
        self._retry_snapshot_cache = RetrySnapshotCache(config.retry_snapshot_cache_size)
        self._policy_settings = {
            name: (self._config.policy_settings.get(name, {}) or {})
            for name in self._policy_names
        }
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
                pass_result = self._run_policy_pipeline(
                    text=text,
                    path=path,
                    logger=logger,
                    confidence_threshold=confidence_threshold,
                    confidence_policies=confidence_policies,
                    blocked_policies=blocked_policies,
                    use_cache=use_cache,
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

            future: Future = self._post_edit_pool.submit(
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

    def _run_policy_pipeline(
        self,
        *,
        text: str,
        path: str,
        logger: logging.Logger,
        confidence_threshold: float,
        confidence_policies: set[str],
        blocked_policies: set[str],
        use_cache: bool,
    ) -> PolicyResult:
        context = ParseContext(
            text=text,
            path=path,
            tree_sitter_tree=None,
            tree_sitter_lang=None,
            clang_ast=None,
            warnings=[],
            editorconfig=self._editorconfig.resolve(path) if self._editorconfig is not None else {},
        )

        current = context.text
        path_hash = self._policy_cache.hash_text(path) if use_cache else ""
        current_hash = self._policy_cache.hash_text(current) if use_cache else ""
        violations: list[Violation] = []
        edits: list[Edit] = []
        ts_text = current
        clang_text = current
        clang_has_bodies = False
        code_context_text: str | None = None
        code_context_tree_id: int | None = None
        code_context_clang_id: int | None = None
        profile: dict[str, float] = {}
        parse_modes: dict[str, str] = {}
        conflict_detector = PolicyConflictDetector(
            ConflictDetectorConfig(
                enabled=bool(self._config.conflict_detection_enabled),
                touch_threshold=max(2, int(self._config.conflict_touch_threshold)),
            )
        )
        suppression = PolicySuppression()

        for policy in self._policies:
            start = time.perf_counter()
            if policy.name in blocked_policies:
                message = f"retry guard skipped policy '{policy.name}'"
                context.warnings.append(message)
                logger.warning("%s", message)
                parse_modes[policy.name] = ParseBackend.SKIPPED.value
                if self._config.profile_enabled:
                    elapsed = (time.perf_counter() - start) * 1000.0
                    profile[policy.name] = profile.get(policy.name, 0.0) + elapsed
                continue

            policy_needs_code_context = policy.parse_mode == "clang" or bool(
                getattr(policy, "requires_code_context", False)
            )
            if use_cache:
                settings = self._policy_cache_settings.get(policy.name, self._cache_settings(policy.name))
                settings_hash = self._policy_cache_settings_hashes.get(policy.name)
                key = self._policy_cache.make_key(
                    policy.name,
                    path,
                    current,
                    settings,
                    path_hash=path_hash,
                    text_hash=current_hash,
                    settings_hash=settings_hash,
                )
                cached = self._policy_cache.get(key)
                if cached is not None:
                    current = cached.text
                    current_hash = self._policy_cache.hash_text(current)
                    violations.extend(cached.violations)
                    edits.extend(cached.edits)
                    if self._config.profile_enabled:
                        elapsed = (time.perf_counter() - start) * 1000.0
                        profile[policy.name] = profile.get(policy.name, 0.0) + elapsed
                    continue
            target_tree, target_clang = self._resolve_parse_targets(policy, policy_needs_code_context)
            settings = self._policy_settings.get(policy.name, {})
            include_bodies = bool(settings.get("clang_full_parse", False))
            need_tree_parse = target_tree and (context.tree_sitter_tree is None or current != ts_text)
            need_clang_parse = target_clang and (
                context.clang_ast is None
                or current != clang_text
                or (include_bodies and not clang_has_bodies)
            )

            if need_tree_parse and need_clang_parse:
                self._parse_tree_and_clang_parallel(
                    context=context,
                    text=current,
                    path=path,
                    include_bodies=include_bodies,
                    logger=logger,
                )
                ts_text = current
                clang_text = current
                clang_has_bodies = include_bodies
            else:
                if need_tree_parse:
                    tree, lang, warning = self._parser_manager.parse_tree_sitter(current, path)
                    context.tree_sitter_tree = tree
                    context.tree_sitter_lang = lang
                    ts_text = current
                    if warning:
                        context.warnings.append(warning)
                        logger.warning("%s", warning)

                if need_clang_parse:
                    clang_ast, warning = self._parser_manager.parse_clang(
                        ParserManager.ClangParseArgs(
                            text=current,
                            path=path,
                            args=tuple(self._clang_args.get_args(path)),
                            include_function_bodies=include_bodies,
                        )
                    )
                    context.clang_ast = clang_ast
                    clang_text = current
                    clang_has_bodies = include_bodies
                    if warning:
                        context.warnings.append(warning)
                        logger.warning("%s", warning)

            policy_backend = self._parse_control.backend_for_policy(policy, context)
            if policy_backend == ParseBackend.SKIPPED:
                message = (
                    f"policy '{policy.name}' skipped: required parser backend unavailable "
                    f"(parse_mode={policy.parse_mode})"
                )
                context.warnings.append(message)
                logger.warning("%s", message)
                parse_modes[policy.name] = policy_backend.value
                if self._config.profile_enabled:
                    elapsed = (time.perf_counter() - start) * 1000.0
                    profile[policy.name] = profile.get(policy.name, 0.0) + elapsed
                continue

            current_tree_id = id(context.tree_sitter_tree) if context.tree_sitter_tree is not None else None
            current_clang_id = id(context.clang_ast) if context.clang_ast is not None else None
            needs_code_context = policy_needs_code_context and (
                context.tree_sitter_tree is not None or context.clang_ast is not None
            )
            if needs_code_context and (
                context.code_context is None
                or current != code_context_text
                or current_tree_id != code_context_tree_id
                or current_clang_id != code_context_clang_id
            ):
                context.code_context = self._code_context_builder.build(
                    path=path,
                    text=current,
                    clang_ast=context.clang_ast,
                    tree_sitter_tree=context.tree_sitter_tree,
                    project_index_cache=self._project_index_cache,
                )
                code_context_text = current
                code_context_tree_id = current_tree_id
                code_context_clang_id = current_clang_id

            before_policy_text = current
            cache_input_hash = current_hash
            context.text = before_policy_text
            result = policy.apply(context)
            disabled_lines = suppression.disabled_lines(before_policy_text, policy.name)
            if disabled_lines:
                result = self._apply_line_suppression(before_policy_text, result, disabled_lines)

            guard_violations = self._guard_policy_edits(
                policy=policy,
                context=context,
                edits=result.edits,
            )
            if guard_violations:
                result = PolicyResult(
                    text=before_policy_text,
                    violations=[*result.violations, *guard_violations],
                    edits=[],
                    profile=result.profile,
                    parse_modes=result.parse_modes,
                    warnings=result.warnings,
                )

            confidence_violation = self._confidence_block_violation(
                policy,
                context,
                result,
                threshold_override=confidence_threshold,
                policy_filter=confidence_policies,
            )
            if confidence_violation is not None:
                result = PolicyResult(
                    text=before_policy_text,
                    violations=[*result.violations, confidence_violation],
                    edits=[],
                    profile=result.profile,
                    parse_modes=result.parse_modes,
                    warnings=result.warnings,
                )

            current = result.text
            if use_cache:
                current_hash = self._policy_cache.hash_text(current)
            violations.extend(result.violations)
            edits.extend(result.edits)
            violations.extend(conflict_detector.observe(policy.name, result.edits))
            parse_modes[policy.name] = policy_backend.value
            if self._config.profile_enabled:
                elapsed = (time.perf_counter() - start) * 1000.0
                profile[policy.name] = profile.get(policy.name, 0.0) + elapsed
            if use_cache:
                settings = self._policy_cache_settings.get(policy.name, self._cache_settings(policy.name))
                settings_hash = self._policy_cache_settings_hashes.get(policy.name)
                key = self._policy_cache.make_key(
                    policy.name,
                    path,
                    context.text,
                    settings,
                    path_hash=path_hash,
                    text_hash=cache_input_hash,
                    settings_hash=settings_hash,
                )
                self._policy_cache.put(key, result)

        return PolicyResult(
            text=current,
            violations=violations,
            edits=edits,
            profile=profile,
            parse_modes=parse_modes,
            warnings=list(dict.fromkeys(context.warnings)),
        )

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

    def _resolve_parse_targets(self, policy: Policy, needs_code_context: bool) -> tuple[bool, bool]:
        policy_wants_tree = policy.parse_mode == "tree_sitter" or bool(getattr(policy, "_use_tree_sitter", False))
        policy_wants_clang = policy.parse_mode == "clang"
        strategy = self._parser_strategy
        match strategy:
            case ParserStrategy.POLICY:
                return policy_wants_tree, policy_wants_clang
            case ParserStrategy.TREE_ONLY:
                return policy_wants_tree or needs_code_context, False
            case ParserStrategy.CLANG_ONLY:
                return False, policy_wants_clang or needs_code_context
            case _:
                # hybrid (default)
                return policy_wants_tree or needs_code_context, policy_wants_clang or needs_code_context

    def _policy_touch_contract(self, policy: Policy) -> TouchContract:
        default_contracts = {
            "naming_conventions": TouchContract.ANY.value,
            "snake_case": TouchContract.CODE_ONLY.value,
            "pointer_bind_style": TouchContract.CODE_ONLY.value,
            "include_order": TouchContract.PREPROCESSOR_ONLY.value,
            "include_guards": TouchContract.PREPROCESSOR_ONLY.value,
            "pragma_once_spacing": TouchContract.PREPROCESSOR_ONLY.value,
            "clang_format": TouchContract.ANY.value,
        }
        settings = self._policy_settings.get(policy.name, {})
        raw = settings.get(
            "touch_contract",
            getattr(policy, "touch_contract", default_contracts.get(policy.name, TouchContract.ANY.value)),
        )
        return TouchContract.from_value(raw)

    def _guard_policy_edits(
        self,
        *,
        policy: Policy,
        context: ParseContext,
        edits: list[Edit],
    ) -> list[Violation]:
        contract = self._policy_touch_contract(policy)
        if contract == TouchContract.ANY or not edits:
            return []
        if context.tree_sitter_tree is None:
            return [
                Violation(
                    policy="edit_guard",
                    message=(
                        f"Blocked edits from '{policy.name}': touch contract '{contract.value}' requires tree-sitter context"
                    ),
                    line=int(edits[0].line),
                    column=1,
                )
            ]
        return self._edit_guard.validate(
            policy_name=policy.name,
            contract=contract,
            edits=edits,
            parse_context=context,
        )

    def _confidence_block_violation(
        self,
        policy: Policy,
        context: ParseContext,
        result: PolicyResult,
        *,
        threshold_override: float | None = None,
        policy_filter: set[str] | None = None,
    ) -> Violation | None:
        if not self._config.confidence_blocking_enabled:
            return None
        if not result.edits:
            return None
        blocked_policies = self._config.confidence_blocking_policies if policy_filter is None else policy_filter
        if blocked_policies and policy.name not in blocked_policies:
            return None
        code_context = getattr(context, "code_context", None)
        if code_context is None:
            return None
        score = float(getattr(code_context, "semantic_hybrid_confidence", 0.0) or 0.0)
        threshold = float(self._config.confidence_blocking_min if threshold_override is None else threshold_override)
        if score <= 0.0 or score >= threshold:
            return None
        return Violation(
            policy="confidence_guard",
            message=(
                f"Blocked edits from '{policy.name}': hybrid confidence {score:.2f} below threshold {threshold:.2f}"
            ),
            line=int(result.edits[0].line),
            column=1,
        )

    def _parse_tree_and_clang_parallel(
        self,
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

        tree_future = self._parse_pool.submit(parse_tree)
        clang_future = self._parse_pool.submit(parse_clang)
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

    def _apply_line_suppression(self, before_text: str, result: PolicyResult, disabled_lines: set[int]) -> PolicyResult:
        if not disabled_lines:
            return result
        kept_violations = [item for item in result.violations if int(item.line) not in disabled_lines]
        kept_edits = [item for item in result.edits if int(item.line) not in disabled_lines]
        if len(kept_edits) == len(result.edits):
            return PolicyResult(
                text=result.text,
                violations=kept_violations,
                edits=kept_edits,
                profile=result.profile,
                parse_modes=result.parse_modes,
                warnings=result.warnings,
            )

        before_lines = before_text.splitlines(keepends=True)
        after_lines = result.text.splitlines(keepends=True)
        max_count = min(len(before_lines), len(after_lines))
        for line_no in disabled_lines:
            idx = int(line_no) - 1
            if 0 <= idx < max_count:
                after_lines[idx] = before_lines[idx]
        text = "".join(after_lines)
        return PolicyResult(
            text=text,
            violations=kept_violations,
            edits=kept_edits,
            profile=result.profile,
            parse_modes=result.parse_modes,
            warnings=result.warnings,
        )
