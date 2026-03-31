use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tracing::warn;

use crate::files::ecc_frame;
use crate::model::report_record::{PolicyOutcome, ReportRecord};

static DROP_COUNT: AtomicU64 = AtomicU64::new(0);

pub struct ReporterProcess {
    sender: Option<crossbeam_channel::Sender<ReportRecord>>,
    sender_handle: Option<JoinHandle<()>>,
    child: Option<Child>,
    stderr_handle: Option<JoinHandle<()>>,
}

impl ReporterProcess {
    pub fn spawn(report_path: PathBuf) -> Result<Self> {
        let exe = std::env::current_exe().context("failed resolving current executable")?;
        let mut child = Command::new(exe)
            .arg("--reporter")
            .arg("--reporter-path")
            .arg(report_path.as_os_str())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed spawning reporter process")?;

        let child_stdin = child.stdin.take().context("reporter stdin unavailable")?;
        let child_stderr = child.stderr.take();

        let stderr_handle = child_stderr.map(|stderr| {
            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines() {
                    match line {
                        Ok(line) => {
                            if !line.is_empty() {
                                warn!(source = "reporter", "{}", line);
                            }
                        }
                        Err(_) => break,
                    }
                }
            })
        });

        let (sender, receiver) = crossbeam_channel::bounded::<ReportRecord>(128);

        let sender_handle = thread::spawn(move || {
            let mut writer = BufWriter::with_capacity(65536, child_stdin);
            while let Ok(record) = receiver.recv() {
                let payload =
                    match postcard::to_allocvec(&record) {
                        Ok(bytes) => bytes,
                        Err(err) => {
                            warn!("reporter: failed serializing record: {}", err);
                            continue;
                        }
                    };
                if let Err(err) = ecc_frame::write_frame(&mut writer, payload.as_slice()) {
                    warn!("reporter: failed writing frame: {}", err);
                    break;
                }
            }
            let _ = writer.flush();
        });

        Ok(Self {
            sender: Some(sender),
            sender_handle: Some(sender_handle),
            child: Some(child),
            stderr_handle,
        })
    }

    pub fn try_send(&self, record: ReportRecord) {
        if let Some(sender) = &self.sender {
            if sender.try_send(record).is_err() {
                DROP_COUNT.fetch_add(1, Ordering::Relaxed);
            }
        } else {
            DROP_COUNT.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn finish(mut self) -> Result<()> {
        self.sender.take();
        if let Some(handle) = self.sender_handle.take() {
            let _ = handle.join();
        }
        if let Some(mut child) = self.child.take() {
            let status = child.wait().context("failed waiting for reporter process")?;
            if !status.success() {
                warn!("reporter process exited with status: {}", status);
            }
        }
        if let Some(handle) = self.stderr_handle.take() {
            let _ = handle.join();
        }
        Ok(())
    }
}

impl Drop for ReporterProcess {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(handle) = self.sender_handle.take() {
            let _ = handle.join();
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
        if let Some(handle) = self.stderr_handle.take() {
            let _ = handle.join();
        }
    }
}

pub fn run_reporter_entry(report_path: &Path) -> Result<()> {
    let stdin = std::io::stdin();
    let mut reader = BufReader::with_capacity(65536, stdin.lock());

    let parent = report_path
        .parent()
        .context("report path has no parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed creating report directory {}", parent.display()))?;

    let ndjson_path = report_path;
    let summary_path = report_path.with_extension("summary.json");
    let trace_path = report_path.with_extension("trace.ndjson");
    let txt_path = report_path.with_extension("txt");

    let mut ndjson_file = BufWriter::with_capacity(
        65536,
        std::fs::File::create(ndjson_path)
            .with_context(|| format!("failed creating {}", ndjson_path.display()))?,
    );
    let mut trace_file = BufWriter::with_capacity(
        65536,
        std::fs::File::create(&trace_path)
            .with_context(|| format!("failed creating {}", trace_path.display()))?,
    );
    let mut txt_file = BufWriter::with_capacity(
        65536,
        std::fs::File::create(&txt_path)
            .with_context(|| format!("failed creating {}", txt_path.display()))?,
    );

    let mut summary = ReportSummary::default();

    loop {
        let payload = match ecc_frame::read_frame(&mut reader) {
            Ok(Some(payload)) => payload,
            Ok(None) => break,
            Err(err) => {
                warn!("reporter: frame read error: {}", err);
                continue;
            }
        };

        let record: ReportRecord = match postcard::from_bytes(payload.as_slice()) {
            Ok(record) => record,
            Err(err) => {
                warn!("reporter: deserialization error: {}", err);
                continue;
            }
        };

        summary.update(&record);

        let ndjson_line = serde_json::to_string(&record).unwrap_or_default();
        let _ = writeln!(ndjson_file, "{}", ndjson_line);

        let trace_line = serde_json::to_string(&json!({
            "path": record.path,
            "decision_trace": {
                "policies": record.policies,
            },
        }))
        .unwrap_or_default();
        let _ = writeln!(trace_file, "{}", trace_line);

        write_human_readable_record(&mut txt_file, &record);
    }

    write_human_readable_summary(&mut txt_file, &summary);
    let _ = txt_file.flush();
    let _ = ndjson_file.flush();
    let _ = trace_file.flush();

    let summary_json = summary.to_json();
    let mut summary_text = serde_json::to_string_pretty(&summary_json)?;
    summary_text.push('\n');
    std::fs::write(&summary_path, summary_text.as_bytes())
        .with_context(|| format!("failed writing {}", summary_path.display()))?;

    #[cfg(unix)]
    {
        if let (Some(report_root), Some(folder_name)) = (parent.parent(), parent.file_name()) {
            let latest = report_root.join("latest");
            let _ = std::fs::remove_file(&latest);
            let _ = std::os::unix::fs::symlink(folder_name, &latest);
        }
    }

    Ok(())
}

#[derive(Default)]
struct ReportSummary {
    files: usize,
    changed: usize,
    errors: usize,
    warnings: usize,
    violations: usize,
    edits: usize,
    blocked_policies: usize,
    total_engine_ms: f64,
    max_engine_ms: f64,
    slowest_file: String,
    total_boot_parse_ms: f64,
    max_boot_parse_ms: f64,
    policy_counts: BTreeMap<String, usize>,
    policy_timing: BTreeMap<String, PolicyTimingAgg>,
}

#[derive(Default)]
struct PolicyTimingAgg {
    total_ms: f64,
    max_ms: f64,
    count: usize,
    parse_total_ms: f64,
    execute_total_ms: f64,
    checkpoint_total_ms: f64,
}

impl ReportSummary {
    fn update(&mut self, record: &ReportRecord) {
        self.files += 1;
        if record.changed {
            self.changed += 1;
        }
        if record.error.is_some() {
            self.errors += 1;
        }
        self.warnings += record.warnings.len();

        if record.elapsed_engine_ms > self.max_engine_ms {
            self.max_engine_ms = record.elapsed_engine_ms;
            self.slowest_file = record.path.display().to_string();
        }
        self.total_engine_ms += record.elapsed_engine_ms;
        self.total_boot_parse_ms += record.boot_parse_ms;
        if record.boot_parse_ms > self.max_boot_parse_ms {
            self.max_boot_parse_ms = record.boot_parse_ms;
        }

        for policy in &record.policies {
            let edit_count = policy.edits.len();
            self.edits += edit_count;
            if edit_count > 0 {
                *self.policy_counts.entry(policy.policy.clone()).or_insert(0) += edit_count;
            }
            if policy.outcome == PolicyOutcome::Blocked {
                self.blocked_policies += 1;
                self.violations += policy.blocked_lines.len();
            }
            let agg = self
                .policy_timing
                .entry(policy.policy.clone())
                .or_default();
            agg.total_ms += policy.elapsed_ms;
            agg.count += 1;
            if policy.elapsed_ms > agg.max_ms {
                agg.max_ms = policy.elapsed_ms;
            }
            agg.parse_total_ms += policy.parse_ms;
            agg.execute_total_ms += policy.execute_ms;
            agg.checkpoint_total_ms += policy.checkpoint_ms;
        }
    }

    fn to_json(&self) -> Value {
        let avg_engine_ms = if self.files > 0 {
            self.total_engine_ms / self.files as f64
        } else {
            0.0
        };

        let per_policy: BTreeMap<String, Value> = self
            .policy_timing
            .iter()
            .map(|(name, agg)| {
                let avg = if agg.count > 0 {
                    agg.total_ms / agg.count as f64
                } else {
                    0.0
                };
                (
                    name.clone(),
                    json!({
                        "total_ms": format!("{:.1}", agg.total_ms),
                        "avg_ms": format!("{:.1}", avg),
                        "max_ms": format!("{:.1}", agg.max_ms),
                        "count": agg.count,
                        "parse_total_ms": format!("{:.1}", agg.parse_total_ms),
                        "execute_total_ms": format!("{:.1}", agg.execute_total_ms),
                        "checkpoint_total_ms": format!("{:.1}", agg.checkpoint_total_ms),
                    }),
                )
            })
            .collect();

        json!({
            "files": self.files,
            "changed": self.changed,
            "errors": self.errors,
            "warnings": self.warnings,
            "violations": self.violations,
            "edits": self.edits,
            "blocked_policies": self.blocked_policies,
            "policies": self.policy_counts,
            "timing": {
                "total_engine_ms": format!("{:.1}", self.total_engine_ms),
                "avg_engine_ms": format!("{:.1}", avg_engine_ms),
                "max_engine_ms": format!("{:.1}", self.max_engine_ms),
                "slowest_file": self.slowest_file,
                "total_boot_parse_ms": format!("{:.1}", self.total_boot_parse_ms),
                "avg_boot_parse_ms": format!("{:.1}", if self.files > 0 { self.total_boot_parse_ms / self.files as f64 } else { 0.0 }),
                "max_boot_parse_ms": format!("{:.1}", self.max_boot_parse_ms),
                "per_policy": per_policy,
            },
            "dropped_records": DROP_COUNT.load(Ordering::Relaxed),
        })
    }
}

fn write_human_readable_record(writer: &mut impl Write, record: &ReportRecord) {
    let status = if record.changed { "CHANGED" } else { "UNCHANGED" };
    let _ = writeln!(
        writer,
        "\n── {} ── {} (engine: {:.1}ms, total: {:.1}ms) ──",
        record.path.display(),
        status,
        record.elapsed_engine_ms,
        record.elapsed_total_ms,
    );

    for policy in &record.policies {
        let icon = match policy.outcome {
            PolicyOutcome::Applied => "  ✓",
            PolicyOutcome::PartiallyApplied => "  ~",
            PolicyOutcome::Blocked => "  ✗",
            PolicyOutcome::NoChange => "  -",
        };
        let detail = match policy.outcome {
            PolicyOutcome::Applied => format!("{} edits ({:.1}ms)", policy.edits.len(), policy.elapsed_ms),
            PolicyOutcome::PartiallyApplied => format!(
                "{}/{} edits ({} dropped) ({:.1}ms)",
                policy.edits.len(),
                policy.candidate_count,
                policy.dropped_count,
                policy.elapsed_ms,
            ),
            PolicyOutcome::Blocked => {
                let reason = policy.reason.as_deref().unwrap_or("blocked");
                format!("BLOCKED ({}) ({:.1}ms)", reason, policy.elapsed_ms)
            }
            PolicyOutcome::NoChange => format!("no change ({:.1}ms)", policy.elapsed_ms),
        };
        let _ = writeln!(writer, "{} {:30} {}", icon, policy.policy, detail);

        for edit in &policy.edits {
            let _ = writeln!(writer, "    L{}: '{}' → '{}'", edit.line, edit.before, edit.after);
        }
    }

    if let Some(err) = &record.error {
        let _ = writeln!(writer, "  ERROR: {}", err);
    }
}

fn write_human_readable_summary(writer: &mut impl Write, summary: &ReportSummary) {
    let _ = writeln!(
        writer,
        "\n{} files | {} changed | {} edits | {} blocked | {} errors",
        summary.files,
        summary.changed,
        summary.edits,
        summary.blocked_policies,
        summary.errors,
    );
    if summary.files > 0 {
        let _ = writeln!(
            writer,
            "Timing: total {:.1}ms, avg {:.1}ms/file, slowest: {} ({:.1}ms)",
            summary.total_engine_ms,
            summary.total_engine_ms / summary.files as f64,
            summary.slowest_file,
            summary.max_engine_ms,
        );
    }
}
