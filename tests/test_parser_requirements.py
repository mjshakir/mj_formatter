from __future__ import annotations

import pytest

from mj_formatter.core.parsing import ParserManager
from mj_formatter.core.processing import FormatterEngine
from mj_formatter.core.types import AppConfig


def _no_tree(self: ParserManager) -> None:
    self._ts_available = False
    self._ts_error = "disabled for test"


def _no_clang(self: ParserManager) -> None:
    self._clang_available = False
    self._clang_error = "disabled for test"


def test_formatter_engine_requires_parsers(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(ParserManager, "_init_tree_sitter", _no_tree)
    monkeypatch.setattr(ParserManager, "_init_clang", _no_clang)
    config = AppConfig(
        root=".",
        policies_default="none",
        policies_enabled=frozenset({"snake_case"}),
        policy_settings={
            "snake_case": {
                "type": "python",
                "enabled": True,
                "touch_contract": "code_only",
                "apply_to": "both",
                "exclude_class_namespace": True,
                "prefer_clang": True,
                "use_tree_sitter": True,
            }
        },
    )
    with pytest.raises(RuntimeError, match="Required parser backends missing"):
        FormatterEngine(config)
