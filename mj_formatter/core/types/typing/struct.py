from __future__ import annotations

from collections.abc import Iterable, Mapping, MutableMapping, MutableSequence, Sequence, Set
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional

from .enums import ParserStrategy


@dataclass(frozen=True)
class Edit:
    policy: str = ""
    line: int = 0
    before: str = ""
    after: str = ""


@dataclass(frozen=True)
class Violation:
    policy: str = ""
    message: str = ""
    line: int = 0
    column: Optional[int] = None


@dataclass(frozen=True)
class ClangFunctionDecl:
    name: str = ""
    start: int = 0
    end: int = 0
    params_start: Optional[int] = None
    params_end: Optional[int] = None
    is_definition: bool = False
    kind: str = ""
    line: int = 0
    column: int = 0


@dataclass(frozen=True)
class ClangParseArgs:
    text: str = ""
    path: str = ""
    args: Sequence[str] = ()
    include_function_bodies: bool = False


@dataclass(frozen=True)
class ClangVarDecl:
    name: str = ""
    start: int = 0
    end: int = 0
    kind: str = ""
    line: int = 0
    column: int = 0


@dataclass(frozen=True)
class CollectTargetsArgs:
    root: Path = Path("")
    include: Sequence[str] = ()
    exclude: Sequence[str] = ()


@dataclass(frozen=True)
class ConflictDetectorConfig:
    enabled: bool = True
    touch_threshold: int = 3


@dataclass(frozen=True)
class EditorConfigSection:
    patterns: Sequence[str] = ()
    properties: Mapping[str, str] = field(default_factory=dict)


@dataclass(frozen=True)
class EditorConfigData:
    sections: Sequence[EditorConfigSection] = ()


@dataclass(frozen=True)
class FileIOConfig:
    root: str = ""
    backup: bool = True
    backup_mode: str = "suffix"
    backup_suffix: str = ".bak"
    backup_dir: str = "backups"


@dataclass
class ParseContext:
    text: str = ""
    path: str = ""
    tree_sitter_tree: Optional[Any] = None
    tree_sitter_lang: Optional[str] = None
    clang_ast: Optional[Any] = None
    warnings: MutableSequence[str] = field(default_factory=list)
    editorconfig: MutableMapping[str, str] = field(default_factory=dict)
    code_context: Optional[Any] = None


@dataclass(frozen=True)
class PolicySourceArgs:
    args: Any = None
    data: Mapping[str, Any] = field(default_factory=dict)
    base_dir: Path = Path("")


@dataclass
class PolicyCacheEntry:
    text: str = ""
    violations: MutableSequence[Any] = field(default_factory=list)
    edits: MutableSequence[Any] = field(default_factory=list)
    warnings: MutableSequence[str] = field(default_factory=list)


@dataclass
class PolicyResult:
    text: str = ""
    violations: MutableSequence[Violation] = field(default_factory=list)
    edits: MutableSequence[Edit] = field(default_factory=list)
    profile: MutableMapping[str, float] = field(default_factory=dict)
    parse_modes: MutableMapping[str, str] = field(default_factory=dict)
    warnings: MutableSequence[str] = field(default_factory=list)


@dataclass
class FileResult:
    path: str = ""
    changed: bool = False
    violations: Sequence[Violation] = field(default_factory=list)
    edits: Sequence[Edit] = field(default_factory=list)
    error: Optional[str] = None
    backup_path: Optional[str] = None
    cache_hit: bool = False
    profile: Optional[Mapping[str, float]] = None
    parse_modes: Optional[Mapping[str, str]] = None
    warnings: Optional[Sequence[str]] = None


@dataclass(frozen=True)
class BackupEntry:
    source: str = ""
    backup: str = ""
    size: int = 0
    mtime_ns: int = 0
    mtime_iso: str = ""
    relative_path: Optional[str] = None


@dataclass(frozen=True)
class BackupManifestConfig:
    backup_dir: str = ""
    run_id: str = ""
    root: str = ""
    mode: str = ""
    suffix: str = ""
    created_at: Optional[str] = None


@dataclass(frozen=True)
class MetricsConfig:
    log_level: str = "INFO"
    log_file: Optional[str] = None
    output_path: Optional[str] = None
    queue_size: int = 10000
    client_buffer_size: int = 0
    include_files: bool = True
    max_files: int = 5000
    include_policies: bool = True
    include_edits: bool = True
    include_parse_modes: bool = True


@dataclass(frozen=True)
class MetricsEvent:
    path: str = ""
    changed: bool = False
    violations: int = 0
    error: bool = False
    cache_hit: bool = False
    duration_ms: float = 0.0
    edits: int = 0
    warnings: int = 0
    policies: Sequence[str] = field(default_factory=list)
    error_message: Optional[str] = None
    parse_modes: Mapping[str, str] = field(default_factory=dict)


@dataclass
class RegistryValidation:
    modules_without_policies: MutableSequence[str] = field(default_factory=list)
    policies_without_name: MutableSequence[str] = field(default_factory=list)
    duplicate_names: MutableMapping[str, Sequence[str]] = field(default_factory=dict)


@dataclass(frozen=True)
class TableData:
    headers: Sequence[str] = field(default_factory=list)
    rows: Sequence[Sequence[str]] = field(default_factory=list)


@dataclass(frozen=True)
class TableStyle:
    use_color: bool = True
    padding: int = 2
    max_width: int = 120


@dataclass
class AppConfig:
    root: str = ""
    include_patterns: Sequence[str] = ()
    exclude_patterns: Sequence[str] = ()
    jobs: int = 0
    check: bool = False
    backup: bool = True
    backup_mode: str = "suffix"
    backup_suffix: str = ".bak"
    backup_dir: str = "backups"
    report_path: str = "report.toml"
    run_journal_dir: str = "scripts/mj_formatter/runs"
    cache_enabled: bool = True
    cache_path: str = "cache"
    check_result_cache_enabled: bool = True
    check_result_cache_path: str = "scripts/mj_formatter/.cache/check_results"
    check_result_cache_l1_size: int = 2048
    log_level: str = "INFO"
    log_file: Optional[str] = None
    profile_enabled: bool = False
    policy_cache_path: str = "styles/cache"
    sort_results: bool = True
    clang_args: Sequence[str] = ()
    clang_compdb_path: Optional[str] = None
    clang_args_mode: str = "merge"
    policies_default: str = "on"
    policies_enabled: Set[str] = frozenset()
    policies_disabled: Set[str] = frozenset()
    policies_order: Sequence[str] = ()
    policy_settings: Mapping[str, Mapping[str, object]] = field(default_factory=dict)
    async_logging: bool = True
    log_queue_size: int = 10000
    shard_merge_workers: int = 2
    conflict_detection_enabled: bool = True
    conflict_touch_threshold: int = 3
    conflict_fail_on_detected: bool = False
    parser_strategy: ParserStrategy = ParserStrategy.HYBRID
    clang_library_paths: Sequence[str] = ()
    parse_pool_workers: int = 2
    worker_batch_size: int = 2
    worker_batch_prefetch: bool = True
    worker_batch_smart: bool = True
    worker_batch_autotune_enabled: bool = False
    worker_batch_autotune_path: str = "scripts/mj_formatter/.cache/worker_batch_autotune.json"
    worker_batch_autotune_candidates: Sequence[int] = (1, 2, 4, 8)
    worker_batch_autotune_probe_interval: int = 12
    worker_batch_autotune_min_files: int = 16
    post_edit_check_enabled: bool = True
    post_edit_retry_enabled: bool = True
    post_edit_retry_max_attempts: int = 6
    post_edit_retry_confidence_step: float = 0.05
    post_edit_retry_confidence_max: float = 1.00
    retry_snapshot_cache_size: int = 128
    confidence_blocking_enabled: bool = True
    confidence_blocking_min: float = 0.70
    confidence_blocking_policies: Set[str] = frozenset({"naming_conventions", "snake_case"})


@dataclass(frozen=True)
class SemanticReference:
    usr: str = ""
    start: int = 0
    end: int = 0
    line: int = 0
    column: int = 0
    is_declaration: bool = False
    scope_usr: Optional[str] = None
    parser_source: str = "clang"


@dataclass(frozen=True)
class SemanticSymbol:
    usr: str = ""
    name: str = ""
    kind: str = ""
    scope_kind: str = ""
    scope_name: Optional[str] = None
    line: int = 0
    column: int = 0
    start: int = 0
    end: int = 0
    is_static: bool = False
    is_const: bool = False
    is_constexpr: bool = False
    is_consteval: bool = False
    is_atomic: bool = False
    is_pointer: bool = False
    smart_ptr: Optional[str] = None
    is_std_function: bool = False
    is_template_type: bool = False
    scope_usr: Optional[str] = None
    parser_consensus: float = 1.0


@dataclass(frozen=True)
class SemanticContext:
    symbols: Sequence[SemanticSymbol] = ()
    references: Sequence[SemanticReference] = ()
    consensus_by_usr: Sequence[tuple[str, float]] = ()
    reference_count_by_usr: Sequence[tuple[str, int]] = ()
    scope_purity_by_usr: Sequence[tuple[str, float]] = ()


@dataclass(frozen=True)
class TreeDeclaration:
    name: str = ""
    kind: str = ""
    scope_kind: str = ""
    scope_name: Optional[str] = None
    start: int = 0
    end: int = 0
    line: int = 0
    column: int = 0


@dataclass(frozen=True)
class CodeBlock:
    kind: str = ""
    label: str = ""
    short_label: str = ""
    start: int = 0
    end: int = 0
    open_line: int = 0
    close_line: int = 0
    source: str = "tree"
    confidence: float = 0.70


@dataclass(frozen=True)
class TreeContextData:
    root_type: Optional[str] = None
    node_count: int = 0
    identifier_spans: Mapping[tuple[int, int], str] = field(default_factory=dict)
    declarations: Sequence[TreeDeclaration] = ()
    blocks: Sequence[CodeBlock] = ()


@dataclass(frozen=True)
class CodeContext:
    clang_functions: Sequence[ClangFunctionDecl] = ()
    clang_variables: Sequence[ClangVarDecl] = ()
    semantic_context: Optional[SemanticContext] = None
    semantic_refs_by_usr: Mapping[str, Sequence[SemanticReference]] = field(default_factory=dict)
    semantic_non_declaration_ref_counts: Mapping[str, int] = field(default_factory=dict)
    semantic_function_symbols: Sequence[SemanticSymbol] = ()
    semantic_class_names: Sequence[str] = ()
    semantic_file_counts: Mapping[str, int] = field(default_factory=dict)
    tree_root_type: Optional[str] = None
    tree_node_count: int = 0
    tree_identifier_count: int = 0
    tree_declarations: Sequence[TreeDeclaration] = ()
    hybrid_blocks: Sequence[CodeBlock] = ()
    semantic_consensus_scores: Mapping[str, float] = field(default_factory=dict)
    semantic_reference_consensus_scores: Mapping[str, float] = field(default_factory=dict)
    semantic_declaration_consensus_scores: Mapping[str, float] = field(default_factory=dict)
    semantic_reference_counts: Mapping[str, int] = field(default_factory=dict)
    semantic_scope_purity: Mapping[str, float] = field(default_factory=dict)
    semantic_project_reference_counts: Mapping[str, int] = field(default_factory=dict)
    semantic_project_consensus_scores: Mapping[str, float] = field(default_factory=dict)
    semantic_hybrid_confidence: float = 0.0


@dataclass(frozen=True)
class SummaryContext:
    results: Iterable[FileResult] = ()
    check_only: bool = False
    verbose: bool = False
    elapsed: float = 0.0
    jobs: int = 0
    fail_on_conflict: bool = False


@dataclass(frozen=True)
class WorkerRunConfig:
    config: AppConfig = field(default_factory=AppConfig)
    jobs: int = 0
    metrics: Optional[Any] = None
    log_queue: Optional[Any] = None


@dataclass(frozen=True)
class VariantSpec:
    name: str = ""
    description: str = ""
    parser_strategy: Optional[str] = None
    parse_pool_workers: Optional[int] = None
    post_edit_check_enabled: Optional[bool] = None
    jobs: Optional[int] = None
    root: Optional[str] = None
    check: bool = True
    profile: bool = True
    cache_enabled: bool = False
    extra_cli_args: Sequence[str] = ()


@dataclass(frozen=True)
class VariantResult:
    name: str = ""
    description: str = ""
    parser_strategy: str = ""
    parse_pool_workers: int = 0
    post_edit_check_enabled: bool = True
    jobs: Optional[int] = None
    files: int = 0
    changed: int = 0
    violations: int = 0
    errors: int = 0
    conflicts: int = 0
    warnings: int = 0
    cache_hits: int = 0
    elapsed_s: float = 0.0
    throughput_files_s: float = 0.0
    parse_modes: str = ""
    exit_code: int = 0
    command: str = ""


__all__ = [
    "AppConfig",
    "BackupEntry",
    "BackupManifestConfig",
    "ClangFunctionDecl",
    "ClangParseArgs",
    "ClangVarDecl",
    "CodeBlock",
    "CodeContext",
    "CollectTargetsArgs",
    "ConflictDetectorConfig",
    "Edit",
    "EditorConfigData",
    "EditorConfigSection",
    "FileIOConfig",
    "FileResult",
    "MetricsConfig",
    "MetricsEvent",
    "ParseContext",
    "PolicyCacheEntry",
    "PolicyResult",
    "PolicySourceArgs",
    "RegistryValidation",
    "SemanticContext",
    "SemanticReference",
    "SemanticSymbol",
    "SummaryContext",
    "TableData",
    "TableStyle",
    "TreeContextData",
    "TreeDeclaration",
    "VariantResult",
    "VariantSpec",
    "Violation",
    "WorkerRunConfig",
]
