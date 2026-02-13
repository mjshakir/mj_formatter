from __future__ import annotations

import logging
from pathlib import Path

from mj_formatter.core.runtime.run_journal import RunJournal
from mj_formatter.core.types import AppConfig


def test_run_journal_start_finish(tmp_path: Path) -> None:
    journal_dir = tmp_path / "runs"
    journal = RunJournal(str(journal_dir), logging.getLogger("mj_formatter_test"))
    config = AppConfig(root=str(tmp_path), run_journal_dir=str(journal_dir))

    run_id = journal.start(config)
    journal.finish(status="COMPLETED", exit_code=0, files=3, changed=1, errors=0)

    path = journal_dir / f"{run_id}.toml"
    assert path.exists()
    text = path.read_text(encoding="utf-8")
    assert 'status = "COMPLETED"' in text
    assert "files = 3" in text
