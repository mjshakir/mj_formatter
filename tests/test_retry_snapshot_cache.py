from __future__ import annotations

from mj_formatter.core.processing.retry_snapshot_cache import RetrySnapshotCache
from mj_formatter.core.types import Edit, PolicyResult, Violation


def _sample_result(label: str) -> PolicyResult:
    return PolicyResult(
        text=f"formatted-{label}",
        violations=[Violation(policy="p", message=f"v-{label}", line=1, column=1)],
        edits=[Edit(policy="p", line=1, before="a", after="b")],
        profile={"p": 1.2},
        parse_modes={"p": "hybrid"},
        warnings=[f"w-{label}"],
    )


def test_retry_snapshot_cache_round_trip_isolated_payload() -> None:
    cache = RetrySnapshotCache(max_entries=8)
    result = _sample_result("x")
    cache.put(
        path="a.cpp",
        text="int main() {}",
        confidence_threshold=0.70,
        confidence_policies={"naming_conventions"},
        blocked_policies=set(),
        result=result,
    )

    first = cache.get(
        path="a.cpp",
        text="int main() {}",
        confidence_threshold=0.70,
        confidence_policies={"naming_conventions"},
        blocked_policies=set(),
    )
    assert first is not None
    assert first.text == "formatted-x"
    assert first.warnings == ["w-x"]

    first.warnings.append("mutated")
    second = cache.get(
        path="a.cpp",
        text="int main() {}",
        confidence_threshold=0.70,
        confidence_policies={"naming_conventions"},
        blocked_policies=set(),
    )
    assert second is not None
    assert second.warnings == ["w-x"]


def test_retry_snapshot_cache_keys_include_retry_constraints() -> None:
    cache = RetrySnapshotCache(max_entries=8)
    cache.put(
        path="a.cpp",
        text="int main() {}",
        confidence_threshold=0.70,
        confidence_policies={"naming_conventions"},
        blocked_policies={"naming_conventions"},
        result=_sample_result("x"),
    )

    miss = cache.get(
        path="a.cpp",
        text="int main() {}",
        confidence_threshold=0.75,
        confidence_policies={"naming_conventions"},
        blocked_policies={"naming_conventions"},
    )
    assert miss is None

    miss = cache.get(
        path="a.cpp",
        text="int main() {}",
        confidence_threshold=0.70,
        confidence_policies={"naming_conventions"},
        blocked_policies=set(),
    )
    assert miss is None


def test_retry_snapshot_cache_eviction() -> None:
    cache = RetrySnapshotCache(max_entries=1)
    cache.put(
        path="one.cpp",
        text="1",
        confidence_threshold=0.70,
        confidence_policies={"naming_conventions"},
        blocked_policies=set(),
        result=_sample_result("one"),
    )
    cache.put(
        path="two.cpp",
        text="2",
        confidence_threshold=0.70,
        confidence_policies={"naming_conventions"},
        blocked_policies=set(),
        result=_sample_result("two"),
    )

    assert cache.get(
        path="one.cpp",
        text="1",
        confidence_threshold=0.70,
        confidence_policies={"naming_conventions"},
        blocked_policies=set(),
    ) is None
    assert cache.get(
        path="two.cpp",
        text="2",
        confidence_threshold=0.70,
        confidence_policies={"naming_conventions"},
        blocked_policies=set(),
    ) is not None
