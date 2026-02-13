from __future__ import annotations

from typing import Iterable

from ..types import AppConfig
from ...policies.registry import PolicyRegistry


class PolicySelector:
    def __init__(self, config: AppConfig, registry: PolicyRegistry) -> None:
        self._config = config
        self._registry = registry

    def resolve(self) -> list[str]:
        available = list(self._config.policy_settings.keys())
        if not available:
            available = list(self._registry.names())
        enabled: set[str] = set()

        if self._config.policies_default != "none":
            enabled.update(available)

        for name, settings in self._config.policy_settings.items():
            if not isinstance(settings, dict):
                continue
            if "enabled" not in settings:
                continue
            if settings.get("enabled"):
                enabled.add(name)
            else:
                enabled.discard(name)

        enabled |= set(self._config.policies_enabled)
        enabled -= set(self._config.policies_disabled)

        order = self._resolve_order(available)
        return [name for name in order if name in enabled]

    def _resolve_order(self, available: Iterable[str]) -> list[str]:
        if self._config.policies_order:
            ordered = [name for name in self._config.policies_order if name in available]
            remaining = [name for name in available if name not in ordered]
            return ordered + remaining
        ordered = list(available)
        # Keep clang-format as the final normalizer when enabled.
        if "clang_format" in ordered:
            ordered = [name for name in ordered if name != "clang_format"] + ["clang_format"]
        return ordered
