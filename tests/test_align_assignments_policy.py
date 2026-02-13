from __future__ import annotations

from mj_formatter.core.types import ParseContext
from mj_formatter.policies.align_assignments_policy import AlignAssignmentsPolicy


def test_align_assignments_skips_deleted_defaulted_special_members() -> None:
    text = (
        "class Hasher {\n"
        "public:\n"
        "    Hasher(const Hasher&) = delete;\n"
        "    Hasher& operator=(const Hasher&) = delete;\n"
        "    Hasher(Hasher&&) = default;\n"
        "};\n"
    )
    policy = AlignAssignmentsPolicy({})
    result = policy.apply(ParseContext(text=text, path="Hasher.hpp"))
    assert result.text == text


def test_align_assignments_skips_template_default_parameter_alignment() -> None:
    text = (
        "template<size_t N = HAZARD_POINTERS>\n"
        "template<size_t M = N>\n"
        "void f();\n"
    )
    policy = AlignAssignmentsPolicy({})
    result = policy.apply(ParseContext(text=text, path="X.hpp"))
    assert result.text == text

