from __future__ import annotations

from mj_formatter.core.engine.context import PolicyConfidenceEngine
from mj_formatter.core.types import (
    AppConfig,
    CodeBlock,
    CodeContext,
    Edit,
    PolicyDecisionOutcome,
    PolicyEnforcement,
)


def _confidence_profile() -> dict[str, object]:
    return {
        "low_score_downgrade_threshold": 0.45,
        "high_score_upgrade_threshold": 0.90,
        "high_consensus_upgrade_threshold": 0.90,
        "low_consensus_downgrade_threshold": 0.60,
        "project_consensus_upgrade_threshold": 0.92,
        "project_score_upgrade_threshold": 0.82,
        "reason_low_consensus_threshold": 0.60,
        "risk_limit_hard": 0.15,
        "risk_limit_soft": 0.30,
        "risk_limit_advisory": 0.95,
        "risk_limit_floor": 0.05,
        "delta_risk_penalty_cap": 0.15,
        "retry_risk_penalty": 0.05,
        "line_confidence_default": 0.50,
        "line_index_weight": 0.65,
        "line_block_weight": 0.35,
        "line_neighbor_deltas": [1, 2],
        "line_neighbor_decay": 0.15,
        "block_confidence_default": 0.70,
        "context_bonus_clang": 0.02,
        "context_refs_threshold_low": 32,
        "context_bonus_refs_low": 0.02,
        "context_refs_threshold_high": 128,
        "context_bonus_refs_high": 0.02,
        "context_scope_threshold_low": 0.85,
        "context_bonus_scope_low": 0.02,
        "context_scope_threshold_high": 0.95,
        "context_bonus_scope_high": 0.02,
        "context_hybrid_threshold": 0.85,
        "context_bonus_hybrid": 0.01,
        "context_coverage_threshold": 0.80,
        "context_bonus_coverage": 0.01,
    }


def test_confidence_engine_honors_must_enforcement() -> None:
    config = AppConfig(root=".", confidence_blocking_min=0.70)
    engine = PolicyConfidenceEngine(
        config,
        {"naming_conventions": {"enforcement": "must", "confidence_profile": _confidence_profile()}},
    )
    decision = engine.evaluate(
        policy_name="naming_conventions",
        policy_parse_mode="clang",
        code_context=CodeContext(semantic_hybrid_confidence=0.20),
        edits=[Edit(policy="naming_conventions", line=1, before="x", after="y")],
        policy_filter={"naming_conventions"},
    )

    assert decision is not None
    assert decision.base_enforcement == PolicyEnforcement.MUST
    assert decision.effective_enforcement == PolicyEnforcement.MUST
    assert decision.outcome == PolicyDecisionOutcome.APPLY


def test_confidence_engine_can_relax_hard_policy_by_context() -> None:
    config = AppConfig(root=".", confidence_blocking_min=0.70, confidence_strict_delta=0.05)
    engine = PolicyConfidenceEngine(
        config,
        {"snake_case": {"enforcement": "hard", "confidence_profile": _confidence_profile()}},
    )
    decision = engine.evaluate(
        policy_name="snake_case",
        policy_parse_mode="tree_sitter",
        code_context=CodeContext(semantic_hybrid_confidence=0.72),
        edits=[Edit(policy="snake_case", line=4, before="Value", after="_value")],
        retry_attempt=1,
        policy_filter={"snake_case"},
    )

    assert decision is not None
    assert decision.base_enforcement == PolicyEnforcement.HARD
    assert decision.effective_enforcement in {PolicyEnforcement.SOFT, PolicyEnforcement.ADVISORY}


def test_confidence_engine_can_harden_soft_policy_by_context() -> None:
    config = AppConfig(root=".", confidence_blocking_min=0.70, confidence_context_bonus_cap=0.08)
    engine = PolicyConfidenceEngine(
        config,
        {"naming_conventions": {"enforcement": "soft", "confidence_profile": _confidence_profile()}},
    )
    decision = engine.evaluate(
        policy_name="naming_conventions",
        policy_parse_mode="clang",
        code_context=CodeContext(
            semantic_hybrid_confidence=0.95,
            semantic_context=object(),
            semantic_consensus_scores={"usr": 0.98},
            semantic_reference_counts={"usr": 256},
            semantic_scope_purity={"usr": 0.99},
        ),
        edits=[Edit(policy="naming_conventions", line=8, before="XValue", after="m_x_value")],
        policy_filter={"naming_conventions"},
    )

    assert decision is not None
    assert decision.base_enforcement == PolicyEnforcement.SOFT
    assert decision.effective_enforcement in {PolicyEnforcement.HARD, PolicyEnforcement.MUST}
    assert decision.outcome == PolicyDecisionOutcome.APPLY


def test_confidence_engine_partial_apply_drops_high_risk_lines() -> None:
    config = AppConfig(root=".", confidence_blocking_min=0.70, confidence_strict_delta=0.05)
    engine = PolicyConfidenceEngine(
        config,
        {"pointer_bind_style": {"enforcement": "hard", "confidence_profile": _confidence_profile()}},
    )
    context = CodeContext(
        semantic_hybrid_confidence=0.50,
        hybrid_blocks=(
            CodeBlock(kind="function", open_line=1, close_line=10, confidence=0.95),
            CodeBlock(kind="function", open_line=20, close_line=40, confidence=0.20),
        ),
    )
    decision = engine.evaluate(
        policy_name="pointer_bind_style",
        policy_parse_mode="tree_sitter",
        code_context=context,
        edits=[
            Edit(policy="pointer_bind_style", line=4, before="T *x", after="T* x"),
            Edit(policy="pointer_bind_style", line=24, before="U *y", after="U* y"),
        ],
        policy_filter={"pointer_bind_style"},
    )

    assert decision is not None
    assert decision.outcome == PolicyDecisionOutcome.APPLY_PARTIAL
    assert 24 in decision.dropped_lines


def test_confidence_engine_uses_context_score_when_semantic_score_is_low() -> None:
    config = AppConfig(root=".", confidence_blocking_min=0.70)
    engine = PolicyConfidenceEngine(
        config,
        {"snake_case": {"enforcement": "soft", "confidence_profile": _confidence_profile()}},
    )
    decision = engine.evaluate(
        policy_name="snake_case",
        policy_parse_mode="tree_sitter",
        code_context=CodeContext(
            semantic_hybrid_confidence=0.35,
            semantic_consensus_summary=0.86,
            hybrid_context_score=0.90,
        ),
        edits=[Edit(policy="snake_case", line=12, before="Value", after="_value")],
        policy_filter={"snake_case"},
    )

    assert decision is not None
    assert decision.outcome == PolicyDecisionOutcome.APPLY


def test_confidence_engine_line_index_improves_partial_drop_accuracy() -> None:
    config = AppConfig(root=".", confidence_blocking_min=0.70, confidence_strict_delta=0.05)
    engine = PolicyConfidenceEngine(
        config,
        {"pointer_bind_style": {"enforcement": "hard", "confidence_profile": _confidence_profile()}},
    )
    decision = engine.evaluate(
        policy_name="pointer_bind_style",
        policy_parse_mode="tree_sitter",
        code_context=CodeContext(
            semantic_hybrid_confidence=0.72,
            semantic_line_confidence={5: 0.95, 25: 0.10},
            hybrid_blocks=(
                CodeBlock(kind="function", open_line=1, close_line=40, confidence=0.95),
            ),
        ),
        edits=[
            Edit(policy="pointer_bind_style", line=5, before="T *x", after="T* x"),
            Edit(policy="pointer_bind_style", line=25, before="U *y", after="U* y"),
        ],
        policy_filter={"pointer_bind_style"},
    )

    assert decision is not None
    assert decision.outcome == PolicyDecisionOutcome.APPLY_PARTIAL
    assert 25 in decision.dropped_lines
