from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass
class AppConfig:
    root: str
    include_patterns: tuple[str, ...]
    exclude_patterns: tuple[str, ...]
    jobs: int
    check: bool
    backup: bool
    backup_mode: str
    backup_suffix: str
    backup_dir: str
    report_path: str
    cache_enabled: bool
    cache_path: str
    log_level: str
    log_file: str | None
    policies_default: str
    policies_enabled: frozenset[str]
    policies_disabled: frozenset[str]
    policies_order: tuple[str, ...]
    policy_settings: dict[str, dict[str, object]]


@dataclass(frozen=True)
class Violation:
    policy: str
    message: str
    line: int
    column: int | None = None


@dataclass(frozen=True)
class Edit:
    policy: str
    line: int
    before: str
    after: str


@dataclass
class PolicyResult:
    text: str
    violations: list[Violation]
    edits: list[Edit]


@dataclass
class FileResult:
    path: str
    changed: bool
    violations: list[Violation]
    edits: list[Edit]
    error: str | None
    backup_path: str | None
    cache_hit: bool


@dataclass
class ParseContext:
    text: str
    path: str
    tree_sitter_tree: Any | None
    tree_sitter_lang: str | None
    clang_ast: Any | None
    warnings: list[str]


@dataclass(frozen=True)
class FileIOConfig:
    root: str
    backup: bool
    backup_mode: str
    backup_suffix: str
    backup_dir: str


@dataclass(frozen=True)
class TableData:
    headers: list[str]
    rows: list[list[str]]


@dataclass(frozen=True)
class TableStyle:
    use_color: bool = True
    padding: int = 2
    max_width: int = 120


@dataclass
class RegistryValidation:
    modules_without_policies: list[str] = field(default_factory=list)
    policies_without_name: list[str] = field(default_factory=list)
    duplicate_names: dict[str, list[str]] = field(default_factory=dict)
