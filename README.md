# MJ Formatter (Prototype)

Standalone, policy‑driven formatter prototype for C++ sources. It is **not** part of the HazardSystem project; it just lives here for local testing.

## Install

Editable install (adds `mj_formatter` CLI):

```bash
python -m venv .venv
source .venv/bin/activate
pip install -e .
```

Optional tree-sitter grammars:

```bash
pip install -e .[tree-sitter]
```

If you prefer requirements files:

```bash
pip install -r requirements.txt
```

Optional tree‑sitter grammars (only if your platform supports it):

```bash
pip install -r requirements-tree-sitter.txt
```

## Run

Dry run (no writes):

```bash
mj_formatter --config config/config.toml --root . --check
```

Apply formatting:

```bash
mj_formatter --config config/config.toml --root .
```

## CLI Options

| Option | Description | Notes |
| --- | --- | --- |
| `--config PATH` | Config TOML path | Defaults to `config/config.toml` if present |
| `--style NAME` | Style pack under `styles/` | Overrides `[policies].style` |
| `--root PATH` | Root directory for discovery | Defaults to `.` |
| `--include GLOB` | Include glob (repeatable) | Example: `--include "src/**/*.cpp"` |
| `--exclude GLOB` | Exclude glob (repeatable) | Excludes are applied early during discovery |
| `--enable LIST` | Enable policies (CSV) | Example: `--enable a,b,c` |
| `--disable LIST` | Disable policies (CSV) | Example: `--disable a,b,c` |
| `--jobs N` | Worker processes | `0` = auto (CPU affinity aware) |
| `--check` | Check only, no writes | Exit code `1` if violations |
| `--report PATH` | JSONL report path | Also writes `.summary.json` |
| `--log-level LEVEL` | Logging level | `DEBUG`, `INFO`, `WARNING`, `ERROR` |
| `--log-file PATH` | Log file path | Console logs are always enabled |
| `--backup/--no-backup` | Toggle backups | Uses config defaults if not set |
| `--cache/--no-cache` | Toggle cache | Uses config defaults if not set |
| `--list-styles` | List styles and exit | |
| `--list-policies` | List policies and exit | Shows enabled/disabled status |
| `--validate-registry` | Validate policy registry | |
| `--undo` | Restore backups and delete them | |
| `--undo-no-delete` | Restore backups and keep them | |

## Styles

Project configuration:
```
config/config.toml
```

Style pack layout:
```
styles/<style_name>/
  format/*.toml          # one policy per file
  enable/enable.toml     # enable/disable list
```

Policies not listed in `enable.toml` are **disabled by default** and produce a warning with an enable example.

## Performance Notes

- File discovery uses a single `os.walk` and prunes excluded directories early for large repos.
- Parallelism is process-based (not threads). On Linux it prefers `fork` for faster startup; elsewhere it uses `spawn`.
- `--jobs 0` picks the available CPU count (honors CPU affinity in containers/cgroups).

## Policy Control

You can enable/disable policies in multiple ways:

- `enable/enable.toml`
  - `[enable].enabled` / `[enable].disabled`
- `config.toml`
  - `[policies].enabled` / `[policies].disabled`
- CLI
  - `--enable policy_a,policy_b`
  - `--disable policy_c`
- Environment
  - `MJ_FORMATTER_ENABLE=policy_a,policy_b`
  - `MJ_FORMATTER_DISABLE=policy_c`

## Utilities

List styles:
```bash
mj_formatter --list-styles
```

List policies (table with status):
```bash
mj_formatter --list-policies --style default
```

Validate registry:
```bash
mj_formatter --validate-registry
```

## Undo

Restore from latest backups (deletes backups on success):

```bash
mj_formatter --undo --config config/config.toml
```

Restore without deleting backups:

```bash
mj_formatter --undo-no-delete --config config/config.toml
```

## Reports & Backups

- JSONL report: `scripts/mj_formatter/reports/format_report.jsonl`
- Summary report: `scripts/mj_formatter/reports/format_report.summary.json`
- Backups (configurable): `scripts/mj_formatter/backups/` or `*.bak`

## Hybrid Parsing

Policies can declare `parse_mode`:
- `text` (default): fast, line‑based.
- `tree_sitter`: uses tree‑sitter for structure.
- `clang`: reserved for future semantic parsing.

## Tests and Benchmarks

```bash
pip install -r requirements-dev.txt
python -m pytest -q
```

Benchmark (via pytest‑benchmark):

```bash
python -m pytest -q tests/bench_formatter_engine.py
```

## Env

Copy `scripts/mj_formatter/.env.example` to `.env` if you want to keep local overrides for `MJ_FORMATTER_ENABLE` / `MJ_FORMATTER_DISABLE`.
