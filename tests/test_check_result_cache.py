from __future__ import annotations

from pathlib import Path

from mj_formatter.core.files.check_result_cache import CheckResultCache
from mj_formatter.core.types import Edit, FileResult, Violation


def _sample_result(path: str) -> FileResult:
    return FileResult(
        path=path,
        changed=False,
        violations=[Violation(policy="p", message="v", line=2, column=3)],
        edits=[Edit(policy="p", line=2, before="a", after="b")],
        error=None,
        backup_path=None,
        cache_hit=False,
        profile={"p": 1.1},
        parse_modes={"p": "hybrid"},
        warnings=["w"],
    )


def test_check_result_cache_round_trip(tmp_path: Path) -> None:
    cache = CheckResultCache(str(tmp_path / "check_cache"), enabled=True, l1_size=8)
    try:
        result = _sample_result("a.cpp")
        cache.put(path="a.cpp", content_hash="hash1", fingerprint="fp", result=result)
        loaded = cache.get(path="a.cpp", content_hash="hash1", fingerprint="fp")
        assert loaded is not None
        assert loaded.path == "a.cpp"
        assert loaded.violations[0].message == "v"
        assert loaded.profile == {"p": 1.1}
        assert loaded.cache_hit is True
    finally:
        cache.close()


def test_check_result_cache_skips_errors(tmp_path: Path) -> None:
    cache = CheckResultCache(str(tmp_path / "check_cache"), enabled=True, l1_size=8)
    try:
        result = _sample_result("b.cpp")
        result.error = "boom"
        cache.put(path="b.cpp", content_hash="hash2", fingerprint="fp", result=result)
        assert cache.get(path="b.cpp", content_hash="hash2", fingerprint="fp") is None
    finally:
        cache.close()
