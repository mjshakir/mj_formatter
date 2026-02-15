from __future__ import annotations

import logging
import time

from ...policy import (
    ConflictDetectorConfig,
    PolicyConflictDetector,
    PolicySuppression,
)
from ...types import Edit, ParseBackend, ParseContext, ParseState, PipelineRunnerDeps, PolicyResult, Violation


class PolicyPipelineRunner:
    def __init__(self, deps: PipelineRunnerDeps) -> None:
        self._deps = deps

    def run(
        self,
        *,
        text: str,
        path: str,
        logger: logging.Logger,
        confidence_threshold: float,
        confidence_policies: set[str],
        blocked_policies: set[str],
        use_cache: bool,
        retry_attempt: int,
    ) -> PolicyResult:
        editorconfig: dict[str, str] = {}
        if self._deps.editorconfig_resolver is not None:
            resolver = self._deps.editorconfig_resolver
            editorconfig = resolver.resolve(path)

        context = ParseContext(
            text=text,
            path=path,
            tree_sitter_tree=None,
            tree_sitter_lang=None,
            clang_ast=None,
            warnings=[],
            editorconfig=editorconfig,
        )

        current = context.text
        path_hash = self._deps.policy_cache.hash_text(path) if use_cache else ""
        current_hash = self._deps.policy_cache.hash_text(current) if use_cache else ""
        violations: list[Violation] = []
        edits: list[Edit] = []
        parse_state = ParseState(ts_text=current, clang_text=current, clang_has_bodies=False)
        code_context_text: str | None = None
        code_context_tree_id: int | None = None
        code_context_clang_id: int | None = None
        profile: dict[str, float] = {}
        parse_modes: dict[str, str] = {}
        conflict_detector = PolicyConflictDetector(
            ConflictDetectorConfig(
                enabled=bool(self._deps.config.conflict_detection_enabled),
                touch_threshold=max(2, int(self._deps.config.conflict_touch_threshold)),
            )
        )
        suppression = PolicySuppression()

        for policy in self._deps.policies:
            start = time.perf_counter()
            if policy.name in blocked_policies:
                message = f"retry guard skipped policy '{policy.name}'"
                context.warnings.append(message)
                logger.warning("%s", message)
                parse_modes[policy.name] = ParseBackend.SKIPPED.value
                if self._deps.config.profile_enabled:
                    elapsed = (time.perf_counter() - start) * 1000.0
                    profile[policy.name] = profile.get(policy.name, 0.0) + elapsed
                continue

            policy_needs_code_context = True
            if use_cache:
                settings = self._deps.policy_cache_settings.get(policy.name, {})
                settings_hash = self._deps.policy_cache_settings_hashes.get(policy.name)
                key = self._deps.policy_cache.make_key(
                    policy.name,
                    path,
                    current,
                    settings,
                    path_hash=path_hash,
                    text_hash=current_hash,
                    settings_hash=settings_hash,
                )
                cached = self._deps.policy_cache.get(key)
                if cached is not None:
                    current = cached.text
                    current_hash = self._deps.policy_cache.hash_text(current)
                    violations.extend(cached.violations)
                    edits.extend(cached.edits)
                    if self._deps.config.profile_enabled:
                        elapsed = (time.perf_counter() - start) * 1000.0
                        profile[policy.name] = profile.get(policy.name, 0.0) + elapsed
                    continue

            settings = self._deps.policy_settings.get(policy.name, {})
            include_bodies = bool(settings.get("clang_full_parse", False))
            parse_state, policy_backend = self._deps.parse_coordinator.ensure_parsed(
                policy=policy,
                context=context,
                text=current,
                path=path,
                include_bodies=include_bodies,
                policy_needs_code_context=policy_needs_code_context,
                state=parse_state,
                logger=logger,
            )

            if policy_backend == ParseBackend.SKIPPED:
                message = (
                    f"policy '{policy.name}' skipped: required parser backend unavailable "
                    f"(parse_mode={policy.parse_mode})"
                )
                context.warnings.append(message)
                logger.warning("%s", message)
                parse_modes[policy.name] = policy_backend.value
                if self._deps.config.profile_enabled:
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
                context.code_context = self._deps.code_context_builder.build(
                    path=path,
                    text=current,
                    clang_ast=context.clang_ast,
                    tree_sitter_tree=context.tree_sitter_tree,
                    project_index_cache=self._deps.project_index_cache,
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
                result = self._deps.policy_runtime.apply_line_suppression(before_policy_text, result, disabled_lines)

            guard_violations = self._deps.policy_runtime.guard_policy_edits(
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

            confidence_result = self._deps.policy_runtime.apply_confidence_decision(
                policy=policy,
                context=context,
                result=result,
                before_text=before_policy_text,
                retry_attempt=retry_attempt,
                threshold_override=confidence_threshold,
                policy_filter=confidence_policies,
            )
            if confidence_result is not None:
                result = confidence_result

            current = result.text
            if use_cache:
                current_hash = self._deps.policy_cache.hash_text(current)
            violations.extend(result.violations)
            edits.extend(result.edits)
            violations.extend(conflict_detector.observe(policy.name, result.edits))
            parse_modes[policy.name] = policy_backend.value
            if self._deps.config.profile_enabled:
                elapsed = (time.perf_counter() - start) * 1000.0
                profile[policy.name] = profile.get(policy.name, 0.0) + elapsed
            if use_cache:
                settings = self._deps.policy_cache_settings.get(policy.name, {})
                settings_hash = self._deps.policy_cache_settings_hashes.get(policy.name)
                key = self._deps.policy_cache.make_key(
                    policy.name,
                    path,
                    context.text,
                    settings,
                    path_hash=path_hash,
                    text_hash=cache_input_hash,
                    settings_hash=settings_hash,
                )
                self._deps.policy_cache.put(key, result)

        return PolicyResult(
            text=current,
            violations=violations,
            edits=edits,
            profile=profile,
            parse_modes=parse_modes,
            warnings=list(dict.fromkeys(context.warnings)),
        )


__all__ = ["PipelineRunnerDeps", "PolicyPipelineRunner"]
