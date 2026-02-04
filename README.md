# MJ Formatter (Prototype)

Standalone, policy‑driven formatter prototype for C++ sources. It is **not** part of the HazardSystem project; it just lives here for local testing.

## Install

```bash
python -m venv scripts/mj_formatter/.venv
source scripts/mj_formatter/.venv/bin/activate
pip install -r scripts/mj_formatter/requirements.txt
```

Optional tree‑sitter grammars (only if your platform supports it):

```bash
pip install -r scripts/mj_formatter/requirements-tree-sitter.txt
```

## Run

Dry run (no writes):

```bash
python scripts/mj_formatter/run.py --config scripts/mj_formatter/config/config.toml --root . --check
```

Apply formatting:

```bash
python scripts/mj_formatter/run.py --config scripts/mj_formatter/config/config.toml --root .
```

## Styles

Project configuration:
```
scripts/mj_formatter/config/config.toml
```

Style pack layout:
```
scripts/mj_formatter/styles/<style_name>/
  format/*.toml          # one policy per file
  enable/enable.toml     # enable/disable list
```

Policies not listed in `enable.toml` are **disabled by default** and produce a warning with an enable example.

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
python scripts/mj_formatter/run.py --list-styles
```

List policies (table with status):
```bash
python scripts/mj_formatter/run.py --list-policies --style default
```

Validate registry:
```bash
python scripts/mj_formatter/run.py --validate-registry
```

## Undo

Restore from latest backups (deletes backups on success):

```bash
python scripts/mj_formatter/run.py --undo --config scripts/mj_formatter/config/config.toml
```

Restore without deleting backups:

```bash
python scripts/mj_formatter/run.py --undo-no-delete --config scripts/mj_formatter/config/config.toml
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
pip install -r scripts/mj_formatter/requirements-dev.txt
python -m pytest -q scripts/mj_formatter
```

Benchmark (via pytest‑benchmark):

```bash
python -m pytest -q scripts/mj_formatter/tests/bench_formatter_engine.py
```

## Env

Copy `scripts/mj_formatter/.env.example` to `.env` if you want to keep local overrides for `MJ_FORMATTER_ENABLE` / `MJ_FORMATTER_DISABLE`.
