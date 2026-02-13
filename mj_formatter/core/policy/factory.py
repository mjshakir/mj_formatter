from __future__ import annotations

from typing import Any

from ...policies.align_assignments_policy import AlignAssignmentsPolicy
from ...policies.trim_trailing_whitespace_policy import TrimTrailingWhitespacePolicy
from ...policies.regex_replace_policy import RegexReplacePolicy
from ...policies.registry import PolicyRegistry
from ..types import AppConfig


class PolicyFactory:
    def __init__(self, config: AppConfig) -> None:
        self._config = config
        self._registry = PolicyRegistry()
        self._type_map = {
            "align_columns": AlignAssignmentsPolicy,
            "trim_trailing_whitespace": TrimTrailingWhitespacePolicy,
            "regex_replace": RegexReplacePolicy,
            "python": None,
            "lua": None,
        }

    def available_names(self) -> list[str]:
        if self._config.policy_settings:
            return list(self._config.policy_settings.keys())
        return list(self._registry.names())

    def create(self, name: str, settings: dict[str, object]) -> Any:
        policy_type = str(settings.get("type", "python")).lower()
        if policy_type == "python":
            policy = self._registry.create(name, settings)
            return policy
        if policy_type == "lua":
            return self._create_lua_policy(name, settings)
        if policy_type in self._type_map and self._type_map[policy_type] is not None:
            cls = self._type_map[policy_type]
            policy = cls(settings)
            policy.name = name
            return policy
        raise KeyError(f"Unknown policy type: {policy_type}")

    def describe(self, name: str, settings: dict[str, object]) -> dict[str, str]:
        policy_type = str(settings.get("type", "python")).lower()
        if policy_type == "python":
            if name in self._registry.names():
                cls = dict(self._registry.items()).get(name)
                if cls:
                    return {
                        "type": "python",
                        "parse_mode": getattr(cls, "parse_mode", "tree_sitter"),
                        "description": str(getattr(cls, "description", "")),
                    }
            return {"type": "python", "parse_mode": "tree_sitter", "description": ""}
        if policy_type == "lua":
            return {"type": "lua", "parse_mode": "tree_sitter", "description": "Lua policy"}
        if policy_type in self._type_map and self._type_map[policy_type] is not None:
            cls = self._type_map[policy_type]
            return {
                "type": policy_type,
                "parse_mode": getattr(cls, "parse_mode", "tree_sitter"),
                "description": str(getattr(cls, "description", "")),
            }
        return {"type": policy_type, "parse_mode": "tree_sitter", "description": ""}

    def _create_lua_policy(self, name: str, settings: dict[str, object]) -> Any:
        try:
            from ...policies.lua_policy import LuaPolicy
        except Exception as exc:  # pragma: no cover
            raise RuntimeError(f"Lua policy requires optional dependency: {exc}") from exc
        policy = LuaPolicy(settings)
        policy.name = name
        return policy
