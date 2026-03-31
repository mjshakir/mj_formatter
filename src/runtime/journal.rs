use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use serde::Serialize;

use crate::config::app_config::AppConfig;
use crate::files::atomic_writer::AtomicWriter;
use crate::model::run_summary::RunSummary;

#[derive(Clone, Debug, Serialize)]
struct RunJournalPayload {
    run: RunRecord,
}

#[derive(Clone, Debug, Serialize)]
struct RunRecord {
    run_id: String,
    status: String,
    started_at_unix_ms: u64,
    finished_at_unix_ms: Option<u64>,
    pid: u32,
    root: String,
    check: bool,
    jobs: usize,
    processes: usize,
    files: usize,
    changed: usize,
    errors: usize,
    warnings: usize,
    violations: usize,
    report_path: String,
    exit_code: i32,
}

pub struct RunJournal {
    path: PathBuf,
    payload: RunJournalPayload,
    finished: bool,
}

impl RunJournal {
    pub fn start(config: &AppConfig) -> Option<Self> {
        let dir = config.run_journal_dir.clone();
        if std::fs::create_dir_all(dir.as_path()).is_err() {
            return None;
        }
        let run_id = format!("{}_{}", current_unix_ms(), std::process::id());
        let path = dir.join(format!("{run_id}.bin"));
        let payload = RunJournalPayload {
            run: RunRecord {
                run_id,
                status: "RUNNING".to_string(),
                started_at_unix_ms: current_unix_ms(),
                finished_at_unix_ms: None,
                pid: std::process::id(),
                root: normalize_path(config.root.as_path()),
                check: config.check,
                jobs: config.jobs,
                processes: config.processes,
                files: 0,
                changed: 0,
                errors: 0,
                warnings: 0,
                violations: 0,
                report_path: normalize_path(config.report_path.as_path()),
                exit_code: 0,
            },
        };
        let journal = Self {
            path,
            payload,
            finished: false,
        };
        if journal.persist().is_err() {
            return None;
        }
        Some(journal)
    }

    pub fn finish_success(&mut self, summary: &RunSummary) {
        self.payload.run.status = "SUCCESS".to_string();
        self.payload.run.finished_at_unix_ms = Some(current_unix_ms());
        self.payload.run.files = summary.files_processed;
        self.payload.run.changed = summary.files_changed;
        self.payload.run.errors = summary.errors;
        self.payload.run.warnings = summary.warnings;
        self.payload.run.violations = summary.violations;
        self.payload.run.exit_code = 0;
        self.finished = true;
        let _ = self.persist();
    }

    fn finish_failure_if_needed(&mut self) {
        if self.finished {
            return;
        }
        self.payload.run.status = "FAILED".to_string();
        self.payload.run.finished_at_unix_ms = Some(current_unix_ms());
        self.payload.run.exit_code = 1;
        self.finished = true;
        let _ = self.persist();
    }

    fn persist(&self) -> Result<()> {
        let content = postcard::to_allocvec(&self.payload)
            .map_err(|err| anyhow!("failed serializing run journal: {err}"))?;
        AtomicWriter::write_bytes(self.path.as_path(), content.as_slice())?;
        Ok(())
    }
}

impl Drop for RunJournal {
    fn drop(&mut self) {
        self.finish_failure_if_needed();
    }
}

fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or(0)
}

fn normalize_path(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string()
}
