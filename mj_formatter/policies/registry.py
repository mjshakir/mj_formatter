from __future__ import annotations

import importlib
import inspect
import logging
import pkgutil
from pathlib import Path

from .policy_base import Policy
from ..core.types import RegistryValidation


class PolicyRegistry:
    _ignore_modules = {"registry", "policy_base", "stub_policy"}

    def __init__(self) -> None:
        self._policies = self._discover()

    def names(self) -> tuple[str, ...]:
        return tuple(self._policies.keys())

    def items(self) -> list[tuple[str, type[Policy]]]:
        return list(self._policies.items())

    def create(self, name: str, config: dict[str, object]) -> Policy:
        if name not in self._policies:
            raise KeyError(f"Unknown policy: {name}")
        return self._policies[name](config)

    def validate(self) -> RegistryValidation:
        validation = RegistryValidation()
        name_map: dict[str, list[str]] = {}
        package_name = __package__
        if not package_name:
            return validation

        package_path = Path(__file__).resolve().parent
        for module_info in pkgutil.iter_modules([str(package_path)]):
            module_name = module_info.name
            if module_name.startswith("_"):
                continue
            if module_name in self._ignore_modules:
                continue
            module = importlib.import_module(f"{package_name}.{module_name}")
            policies = []
            for _, obj in inspect.getmembers(module, inspect.isclass):
                if not issubclass(obj, Policy) or obj is Policy:
                    continue
                policy_name = str(getattr(obj, "name", ""))
                if not policy_name:
                    validation.policies_without_name.append(f"{obj.__module__}.{obj.__name__}")
                    continue
                policies.append(policy_name)
                name_map.setdefault(policy_name, []).append(obj.__module__)

            if not policies:
                validation.modules_without_policies.append(module.__name__)

        for name, modules in name_map.items():
            if len(modules) > 1:
                validation.duplicate_names[name] = modules

        return validation

    def _discover(self) -> dict[str, type[Policy]]:
        logger = logging.getLogger("mj_formatter")
        policies: dict[str, type[Policy]] = {}
        package_name = __package__
        if not package_name:
            return policies

        package_path = Path(__file__).resolve().parent
        for module_info in pkgutil.iter_modules([str(package_path)]):
            module_name = module_info.name
            if module_name.startswith("_"):
                continue
            if module_name in self._ignore_modules:
                continue
            module = importlib.import_module(f"{package_name}.{module_name}")
            for _, obj in inspect.getmembers(module, inspect.isclass):
                if not issubclass(obj, Policy) or obj is Policy:
                    continue
                policy_name = str(getattr(obj, "name", ""))
                if not policy_name:
                    continue
                if policy_name in policies and policies[policy_name] is not obj:
                    logger.warning(
                        "policy name collision: %s from %s overrides %s",
                        policy_name,
                        obj.__module__,
                        policies[policy_name].__module__,
                    )
                policies[policy_name] = obj

        if not policies:
            logger.warning("no policies discovered")
        return policies
