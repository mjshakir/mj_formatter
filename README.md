# formatter

Adaptive C/C++ code formatter with a semantic policy pipeline, built in Rust.

## Key Features

**Semantic awareness**
- Tree-sitter incremental parsing for structural analysis of C and C++
- libclang semantic extraction for type-aware policies like `naming_conventions`
- Dual-parser architecture: tree-sitter provides the hard correctness gate, libclang provides semantic depth

**Adaptive trust model**
- 5-dimensional Kalman filter with Sage-Husa adaptive Q/R tracking structural, semantic, coverage, richness, and edit-success confidence per policy
- All decision derivations (penalties, bonuses, radii, tolerances) computed directly from Kalman estimates and covariance -- no fixed thresholds anywhere
- The system learns what to trust from observation and run history

**Pipeline architecture**
- 6-stage policy pipeline: initialize, parse, prepare, execute, coordinate, commit
- Compile-time enum dispatch for all policies (no trait-object vtables in the hot path)
- Per-policy checkpoint via tree-sitter re-parse -- the only hard gate in the system

**Concurrency**
- FIFO thread pool on `crossbeam-channel` with multi-process worker mode
- Adaptive dispatch scheduling with history-based cost estimation
- No GIL, no garbage collector pauses

**Safety and recoverability**
- Atomic file writes via tempfile + rename
- ECC backup with Reed-Solomon erasure coding
- `--undo` / `--undo-run` to restore any previous formatting run
- Post-edit semantic verification catches regressions before they hit disk

**Project-graph learning**
- Cross-run convergence tracking persisted to disk
- Configurable retention, pruning, and tombstone decay
- Semantic relationship graph informs future formatting decisions

## Why Rust, Why Not Python

This project started as a Python prototype. Python was great for rapid policy iteration but could not scale to production C++ codebases:

- **The GIL blocks parallelism.** Python's Global Interpreter Lock made true parallel file processing impossible. On large codebases with thousands of files, wall-clock time was unacceptable.
- **Runtime errors, not compile-time safety.** A typo in a policy could silently corrupt source files. Rust's type system and ownership model catch entire classes of bugs -- data races, null dereferences, type mismatches -- at compile time.
- **Fragile FFI.** Python bindings to tree-sitter and libclang (via ctypes/cffi) were brittle and hard to debug. Rust's `tree-sitter` and `clang-sys` crates provide first-class, type-safe FFI with zero overhead.
- **Deployment complexity.** Python required a virtualenv, pip dependencies, and a compatible interpreter on every machine. Rust produces a single statically-linked binary.

The Rust rewrite preserved every policy's behavior while eliminating an entire class of runtime bugs and delivering significantly higher throughput on multi-core machines.

## Getting Started

### Step 1: Install Rust

If you have never used Rust before, install it with `rustup`:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustc --version   # verify installation
```

Follow the on-screen prompts and select the default installation. This installs `rustc` (the compiler), `cargo` (the build tool), and `rustup` (the toolchain manager).

### Step 2: Install System Dependencies

The formatter uses libclang at runtime for semantic extraction and needs a C compiler for tree-sitter's generated parser code.

**Debian / Ubuntu:**
```bash
sudo apt-get update
sudo apt-get install -y libclang-dev clang build-essential
```

**Fedora:**
```bash
sudo dnf install clang-devel clang gcc
```

**macOS:**
```bash
xcode-select --install
```
libclang ships with Xcode Command Line Tools.

### Step 3: Clone and Build

```bash
git clone <repo-url>
cd mj_formatter
cargo build --release
```

The release binary lands at `target/release/formatter`. The release profile uses thin LTO, single codegen unit, and `-O3` for a compact, optimized binary.

### Step 4: Configure

Create or edit `config/config.toml` with the minimum required settings:

```toml
[formatter]
root = "/path/to/your/cpp/project"
include = ["**/*.hpp", "**/*.cpp", "**/*.h", "**/*.c"]
```

See the [Configuration](#configuration) section for the full set of options.

### Step 5: Run

```bash
# Dry run -- report what would change without modifying files
cargo run --release -- --config config/config.toml --check

# Apply formatting
cargo run --release -- --config config/config.toml

# Apply with verbose diagnostics
cargo run --release -- --config config/config.toml --verbose

# Undo the last run (restore from backup)
cargo run --release -- --config config/config.toml --undo

# List active policies and exit
cargo run --release -- --config config/config.toml --list-policies
```

Or run the binary directly:

```bash
./target/release/formatter --root /path/to/project --processes 4 --jobs 12 --verbose
```

## CLI Reference

| Flag | Description | Default |
|------|-------------|---------|
| `--config <PATH>` | Path to configuration TOML file | None (optional) |
| `--root <PATH>` | Project root directory to format | From config, or `.` |
| `--include <GLOB>` | File include glob pattern (repeatable) | From config |
| `--exclude <GLOB>` | File exclude glob pattern (repeatable) | From config |
| `--enable <POLICY>` | Enable a specific policy (repeatable) | All enabled |
| `--disable <POLICY>` | Disable a specific policy (repeatable) | None |
| `--jobs <N>` | Total number of parallel jobs | CPU cores x 4 |
| `--processes <N\|"max">` | Number of worker processes; `"max"` for all CPU cores | `1` |
| `--threads-per-process <N>` | Threads per worker process; overrides `--jobs` (total = processes x N) | Derived from `--jobs` |
| `--check` | Dry-run mode: report issues without modifying files | `false` |
| `--undo` | Restore files from the most recent backup run | `false` |
| `--undo-run <RUN_ID>` | Restore files from a specific backup run ID | None |
| `--verbose` | Enable verbose diagnostic output | `false` |
| `--list-policies` | List active policies with parse modes and exit | `false` |
| `--worker-timeout <SECS>` | Worker process timeout in seconds | `900` |

## Policies

| Policy | Category | Description |
|--------|----------|-------------|
| `dash_comment_normalizer` | Comments | Normalize dash-style comment separators |
| `section_title_normalizer` | Comments | Standardize section title comment blocks |
| `namespace_end_comments` | Comments | Add or normalize closing comments for namespaces, classes, and control blocks |
| `pragma_once_spacing` | Headers | Enforce blank-line spacing after `#pragma once` |
| `include_guards` | Headers | Enforce include guard or pragma-once conventions |
| `include_order` | Headers | Sort and group `#include` directives by category |
| `declaration_alignment` | Structure | Align assignment operators, `= delete`, and `= default` |
| `compact_declarations` | Structure | Group consecutive same-type variable declarations |
| `class_layout` | Structure | Organize class members by access specifier with section comments |
| `operator_overload_spacing` | Spacing | Normalize spacing in operator overload signatures |
| `lua_macro_spacing` | Spacing | Enforce spacing around Lua-binding and user-defined macro blocks |
| `numeric_literal_suffix` | Spacing | Normalize numeric literal suffixes (`U`, `L`, `ULL`, `f`, etc.) |
| `function_void_params` | Semantic | Normalize `void` in empty C/C++ parameter lists |
| `logical_keyword_operators` | Semantic | Replace `&&`/`||`/`!` with `and`/`or`/`not` (or vice versa) |
| `snake_case` | Semantic | Enforce snake_case naming for functions and variables |
| `naming_conventions` | Semantic | Semantic rename with prefix conventions and project-wide reference propagation |
| `clang_format` | Integration | Run clang-format as a pipeline stage with semantic safety gates |

Policies can be selectively enabled or disabled via `--enable` / `--disable` CLI flags or in the `config.toml` configuration file.

## Configuration

Runtime configuration lives in `config/config.toml`:

- `[formatter]` -- root path, include/exclude globs, parallelism, backup settings, clang binary paths, cache settings, worker timeout
- Style and policy TOML files under `styles/<style>/format/*.toml` and `styles/<style>/enable/enable.toml`

Key config-only settings (not exposed as CLI flags):

| Setting | Default | Description |
|---------|---------|-------------|
| `formatter.backup` | `true` | Enable file backups before formatting |
| `formatter.backup_dir` | `var/backups` | Backup storage directory |
| `formatter.report_path` | `var/reports/run.ndjson` | Run report output path |
| `formatter.clang_binary` | `clang` | Path to clang executable |
| `formatter.clang_format_binary` | `clang-format` | Path to clang-format executable |
| `formatter.cache.enabled` | `true` | Enable result caching between runs |
| `formatter.cache.path` | `var/cache/check_results.bin` | Cache file path |
| `formatter.cache.l1_size` | `2048` | L1 in-memory cache size |

Project-graph lifecycle settings:
- `project_graph_prune_enabled`, `project_graph_retention_days`, `project_graph_max_nodes`, `project_graph_max_edges`
- `project_graph_tombstone_enabled`, `project_graph_tombstone_retention_days`, `project_graph_tombstone_decay_days`

## Feedback Loop Workflow

Use the built-in helper script for iterative apply/review/restore runs:

```bash
# Run the formatter
scripts/feedback.sh run --root ../HazardSystem --processes 4 --jobs 8

# Restore the latest backup batch
scripts/feedback.sh restore-latest

# Restore a specific file from the latest batch
scripts/feedback.sh restore-latest --only BitmaskTable.hpp

# Show latest backup and report status
scripts/feedback.sh status
```

## Architecture

- `PolicyPipeline` is stage-based (`initialize -> parse -> prepare -> execute -> coordinate -> commit`) with a shared run-state object.
- `ParserManager` is an orchestration facade over focused parser modules:
  - `clang_service` (singleton worker service)
  - `arg_resolver` (clang args + probe cache)
  - `compdb_index` (exact + consensus compile_commands resolution)
  - `consensus` (header/source consensus arbitration)
  - `semantic_extractor` (libclang semantic extraction)
- Semantic integrity checks are decomposed into focused stages:
  - `semantic_contract/{readiness, snapshot, context, transition}`
  - `post_check/{baseline, delta, verdict}`
- Policy metadata is centralized in a singleton `PolicyCatalog`.
- Internal identifiers use typed enums/newtypes (`PolicyId`, parse mode, retry scope, risk tier, zone, candidate outcome); JSON report artifacts remain string-compatible at boundaries.
- Safe Rust throughout (no `unsafe` blocks in project code).
- Compile-time dispatch for policy execution (enum-based, no trait-object virtual dispatch in the pipeline).
- FIFO thread pool built on `crossbeam-channel`.
- Atomic file writes with ECC backup support.

## License

MIT
