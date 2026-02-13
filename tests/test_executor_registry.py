from __future__ import annotations

from mj_formatter.core.processing.executor_registry import ExecutorRegistry


def test_executor_registry_is_singleton() -> None:
    first = ExecutorRegistry()
    second = ExecutorRegistry()

    assert first is second

    parse_a = first.get_parse_pool(2)
    parse_b = second.get_parse_pool(2)
    assert parse_a is parse_b

    parse_c = first.get_parse_pool(3)
    assert parse_c is not parse_a

    post_a = first.get_post_edit_pool()
    post_b = second.get_post_edit_pool()
    assert post_a is post_b
