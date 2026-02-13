from __future__ import annotations

from mj_formatter.core.types import Edit
from mj_formatter.core.policy import PolicyConflictDetector


def test_policy_conflict_detector_reports_revert() -> None:
    detector = PolicyConflictDetector()
    first = [Edit(policy="policy_a", line=10, before="int x = 1;", after="int x=1;")]
    second = [Edit(policy="policy_b", line=10, before="int x=1;", after="int x = 1;")]

    assert detector.observe("policy_a", first) == []
    violations = detector.observe("policy_b", second)
    assert any("appears to revert policy 'policy_a'" in item.message for item in violations)


def test_policy_conflict_detector_reports_high_touch_lines() -> None:
    detector = PolicyConflictDetector()
    edits_a = [Edit(policy="policy_a", line=4, before="A", after="B")]
    edits_b = [Edit(policy="policy_b", line=4, before="B", after="C")]
    edits_c = [Edit(policy="policy_c", line=4, before="C", after="D")]

    detector.observe("policy_a", edits_a)
    detector.observe("policy_b", edits_b)
    violations = detector.observe("policy_c", edits_c)
    assert any("touched by multiple policies" in item.message for item in violations)

