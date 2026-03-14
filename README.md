# formatter

`formatter` is a Rust-native C/C++ formatter pipeline.

## Status

Python implementation has been removed from the repository.
The active implementation is in `src/` and built from the workspace root.

## Build

```bash
cargo build
```

Release build:

```bash
cargo build --release
```

## Run

Check mode:

```bash
cargo run -- --config config/config.toml --check
```

Apply changes:

```bash
cargo run -- --config config/config.toml
```

Verbose diagnostics:

```bash
cargo run -- --config config/config.toml --verbose
```

List active policies:

```bash
cargo run -- --config config/config.toml --list-policies
```

## Feedback Loop Workflow

Use the built-in helper script for iterative apply/review/restore runs:

```bash
scripts/feedback.sh run --root ../HazardSystem --processes 4 --jobs 8
```

Restore the latest backup batch:

```bash
scripts/feedback.sh restore-latest
```

Restore only a targeted file from the latest backup batch:

```bash
scripts/feedback.sh restore-latest --only BitmaskTable.hpp
```

Show latest backup/report status:

```bash
scripts/feedback.sh status
```

## Current Policies

Implemented natively in Rust:
- `trim_trailing_whitespace`
- `dash_comment_normalizer`
- `section_title_normalizer`
- `lua_macro_spacing`
- `pragma_once_spacing`
- `include_guards`
- `align_assignments`
- `operator_overload_spacing`
- `clang_format`
- `naming_conventions` (hybrid semantic rename, strict mode, project-wide reference propagation)

Unported policies execute as no-op stubs with warnings.

## Configuration

Runtime config:
- `config/config.toml`
- project graph lifecycle keys:
  - `project_graph_prune_enabled`
  - `project_graph_retention_days`
  - `project_graph_max_nodes`
  - `project_graph_max_edges`
  - `project_graph_tombstone_enabled`
  - `project_graph_tombstone_retention_days`
  - `project_graph_tombstone_decay_days`

Style and policy TOML:
- `styles/<style>/format/*.toml`
- `styles/<style>/enable/enable.toml`

## Design Notes

- Safe Rust (no `unsafe` blocks in project code)
- Compile-time dispatch for policy execution (enum-based, no trait-object virtual dispatch in pipeline)
- FIFO thread pool built on `crossbeam-channel`
- Atomic file writes and backup support
- Project-graph persistence performs retention/cap compaction and reports per-run before/after counts

## Architecture

- `PolicyPipeline` is stage-based (`initialize -> parse -> prepare -> execute -> coordinate -> commit`) with a shared run-state object.
- `ParserManager` is an orchestration facade over focused parser modules:
  - `clang_service` (singleton worker service)
  - `arg_resolver` (clang args + probe cache)
  - `compdb_index` (exact + consensus compile_commands resolution)
  - `consensus` (header/source consensus arbitration)
  - `semantic_extractor` (libclang semantic extraction)
- Semantic integrity checks are decomposed into focused stages:
  - `semantic_contract/{readiness,snapshot,context,transition}`
  - `post_check/{baseline,delta,verdict}`
- Policy metadata is centralized in a singleton `PolicyCatalog`.
- Internal policy identifiers/trace fields use typed enums/newtypes (`PolicyName`, parse mode, retry scope, risk tier, zone, candidate outcome); JSON report artifacts remain string-compatible at boundaries.
- Project-graph learning and telemetry snapshots carry typed policy identifiers (`PolicyName`) internally; persisted keys and reports remain string-compatible at boundaries.
