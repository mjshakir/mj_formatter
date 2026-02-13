# MJ Formatter

Policy-driven C/C++ formatter with hybrid parsing support (clang + tree-sitter + Lua policies), backups, cache, and profiling.

## Platform Setup (Python + Pip)

This section installs Python from the command line on each OS, then installs `mj_formatter` and runs it.

### Ubuntu (CLI)

1. Install Python, `venv`, and pip:

```bash
sudo apt update
sudo apt install -y python3 python3-venv python3-pip
```

2. Verify Python + pip:

```bash
python3 --version
python3 -m pip --version
```

### macOS (Homebrew)

1. Install Python with Homebrew:

```bash
brew update
brew install python@3.12
```

2. Verify Python + pip:

```bash
python3 --version
python3 -m pip --version
```

### Windows (vcpkg / "vpkg", PowerShell)

1. Install vcpkg (if not already installed):

```powershell
git clone https://github.com/microsoft/vcpkg.git
cd vcpkg
.\bootstrap-vcpkg.bat
```

2. Install Python from vcpkg:

```powershell
.\vcpkg install python3:x64-windows
```

3. Find the installed Python executable:

```powershell
Get-ChildItem .\installed\x64-windows\tools\python3 -Recurse -Filter python.exe
```

Use the returned `python.exe` path in the project setup commands below.

## Project Setup (All Platforms)

1. From the repo root, create and activate virtual environment:

Linux/macOS:

```bash
python3 -m venv .venv
source .venv/bin/activate
```

Windows PowerShell:

```powershell
<path-to-python.exe> -m venv .venv
.\.venv\Scripts\Activate.ps1
```

2. Upgrade packaging tools:

```bash
python -m pip install --upgrade pip setuptools wheel
```

3. Install formatter:

```bash
pip install -e .
```

4. Optional extras:

```bash
pip install -e .[lua]
```

5. Or install from requirements directly:

```bash
pip install -r requirements.txt
```

6. Verify install:

```bash
python -m mj_formatter.main --list-policies --config config/config.toml
```

## Run

### Quick Run Checklist

1. Validate policy registry:

```bash
python -m mj_formatter.main --config config/config.toml --validate-registry
```

2. Show active policies:

```bash
python -m mj_formatter.main --config config/config.toml --list-policies
```

3. Dry run (check mode, no writes):

```bash
python -m mj_formatter.main --config config/config.toml --root . --check --verbose
```

4. Apply changes:

```bash
python -m mj_formatter.main --config config/config.toml --root . --verbose
```

5. Undo from backup if needed:

```bash
python -m mj_formatter.main --config config/config.toml --undo
```

If editable install added the CLI entrypoint:

```bash
mj_formatter --config config/config.toml --root . --check
```

Module form (always works from repo root):

```bash
python -m mj_formatter.main --config config/config.toml --root . --check
```

Apply changes:

```bash
python -m mj_formatter.main --config config/config.toml --root .
```

Run on `HazardSystem`:

```bash
python -m mj_formatter.main --config config/config.toml --root ../HazardSystem
```

Run on `HazardSystem` with safety + diagnostics:

```bash
python -m mj_formatter.main \
  --config config/config.toml \
  --root ../HazardSystem \
  --check \
  --verbose \
  --profile
```

## CLI Options

| Option | Description |
| --- | --- |
| `--config PATH` | Config TOML path |
| `--style NAME` | Style folder under `styles/` |
| `--root PATH` | Project root for file discovery |
| `--include GLOB` | Include glob (repeatable) |
| `--exclude GLOB` | Exclude glob (repeatable) |
| `--enable LIST` | Enable policies (CSV or repeated) |
| `--disable LIST` | Disable policies (CSV or repeated) |
| `--jobs N` | Worker processes (`0` = auto) |
| `--check` | Check only; no writes |
| `--verbose` | Print per-file violations/warnings |
| `--profile` | Aggregate per-policy timing (ms) in summary |
| `--parser-strategy {policy,hybrid,tree_only,clang_only}` | Override parser strategy for the run |
| `--parse-pool-workers N` | Per-process parse thread workers |
| `--post-edit-check / --no-post-edit-check` | Enable/disable post-edit validation |
| `--batch-autotune / --no-batch-autotune` | Enable/disable worker batch autotuning |
| `--report PATH` | JSONL report output |
| `--log-level LEVEL` | Log level |
| `--log-file PATH` | Log file output |
| `--backup` / `--no-backup` | Enable/disable backups |
| `--cache` / `--no-cache` | Enable/disable cache |
| `--list-styles` | List styles and exit |
| `--list-policies` | List policies and status table |
| `--validate-registry` | Validate policy registry |
| `--undo` | Restore latest backup and delete backup files |
| `--undo-no-delete` | Restore latest backup and keep backup files |

## Style Packs

Style configuration lives under:

```text
styles/<style_name>/
  enable/enable.toml
  format/*.toml
```

Per-policy safety contract (optional in each `format/*.toml`):

- `touch_contract = "any"`
- `touch_contract = "code_only"`
- `touch_contract = "preprocessor_only"`
- `touch_contract = "whitespace_only"`

Project-level runtime config:

```text
config/config.toml
```

## Parsing and Policy Backends

- `parse_mode = "text"`: text-based policies.
- `parse_mode = "tree_sitter"`: syntax-tree policies.
- `parse_mode = "clang"`: semantic/clang-backed policies.
- `type = "lua"`: Lua policy scripts.

Default parser strategy is hybrid and uses policy needs to decide parse work.
For parser-required policies (`tree_sitter` / `clang`), backend fallback-to-text is disabled; if the backend is unavailable the policy is skipped with a warning.

Relevant runtime controls in `config/config.toml`:

- `post_edit_check_enabled = true`: re-parse before/after and block unsafe output.
- `post_edit_retry_enabled = true`: retry failed post-edit checks from original text.
- `post_edit_retry_max_attempts = 6`
- `post_edit_retry_confidence_step = 0.05`
- `post_edit_retry_confidence_max = 1.00`
- `confidence_blocking_enabled = true`
- `confidence_blocking_min = 0.70`
- `confidence_blocking_policies = ["naming_conventions", "snake_case"]`
- `run_journal_dir = "scripts/mj_formatter/runs"`: per-run state journal (`RUNNING`/`COMPLETED`/`FAILED`).

Durability/fail-safe notes:

- cache/report/metrics/manifest writes are atomic (`temp -> fsync -> replace`)
- parser workers use thread-local parser instances for safer multi-threaded parse execution
- async metrics/log queues track and warn on dropped events under backpressure

## Core Package Layout

`core/` has been split by concern:

- `core/config/`: config and `.editorconfig` resolution
- `core/processing/`: formatter engine and per-file processor
- `core/parsing/`: clang/tree-sitter parse utilities
- `core/policy/`: policy selection/cache/conflict detection
- `core/files/`: I/O, cache file, backup, undo, report writing
- `core/reporting/`: metrics process/client and table output
- `core/runtime/`: process orchestration and run lifecycle
- `core/engine/context/`: context engine (code context builder, edit guard, post-edit checker)
- `core/types/`, `core/utilities/`, `core/logging/`: shared support modules

## Performance and Profiling

Profile run:

```bash
python -m mj_formatter.main --config config/config.toml --root ../HazardSystem --check --no-cache --profile
```

Profile matrix run (reads `[profiling]` and `[[profiling.matrix]]` from `config/config.toml`):

```bash
python scripts/profile_matrix.py --config config/config.toml
```

Artifacts:

- CSV: `scripts/mj_formatter/profile_matrix/profile_matrix.csv`
- Markdown: `scripts/mj_formatter/profile_matrix/profile_matrix.md`
- Raw `.log`/`.json` artifacts are kept temporary and cleaned after each run.

Summary includes:

- `elapsed`, `throughput`, cache hits
- top policy counts
- top policy times (`top policy times (ms): ...`)

Recent optimization included:

- naming-conventions semantic rename now precomputes identifier occurrence counts once per file instead of rescanning text per symbol.

## Cache, Reports, and Backups

- Policy cache path: `styles/cache` (configurable)
- Report JSONL: `scripts/mj_formatter/reports/format_report.jsonl`
- Summary JSON: `*.summary.json`
- Backups: suffix or mirror-directory mode (configurable)

## Undo

```bash
python -m mj_formatter.main --config config/config.toml --undo
python -m mj_formatter.main --config config/config.toml --undo-no-delete
```

## Tests

Install test dependencies:

```bash
pip install -r requirements-dev.txt
```

Run all tests:

```bash
python -m pytest -q
```

Run behavior test only:

```bash
python -m pytest -q tests/test_behavior_end_to_end.py::test_behavior_end_to_end
```

Run parser/context critical tests:

```bash
python -m pytest -q \
  tests/test_naming_semantic.py \
  tests/test_edit_guard_and_parse_control.py \
  tests/test_post_edit_checker.py \
  tests/test_worker_runner_batching.py
```

Run formatter against a real target and then restore:

```bash
python -m mj_formatter.main --config config/config.toml --root ../HazardSystem --verbose
python -m mj_formatter.main --config config/config.toml --undo
```
