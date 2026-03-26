use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use serde::Deserialize;
use tracing::{error, warn};

#[derive(Debug, Deserialize)]
struct ReportRecord {
    path: PathBuf,
    changed: bool,
    error: Option<String>,
    warnings: Vec<String>,
    elapsed_engine_ms: f64,
    elapsed_total_ms: f64,
    policies: Vec<PolicyReport>,
}

#[derive(Debug, Deserialize)]
struct PolicyReport {
    policy: String,
    outcome: String,
    reason: Option<String>,
    elapsed_ms: f64,
    edits: Vec<EditReport>,
    blocked_lines: Vec<BlockedLineReport>,
    confidence_score: Option<f64>,
    confidence_threshold: Option<f64>,
    parse_mode: String,
    candidate_count: usize,
    dropped_count: usize,
    semantic_impact_radius: usize,
}

#[derive(Debug, Deserialize)]
struct EditReport {
    line: usize,
    before: String,
    after: String,
}

#[derive(Debug, Deserialize)]
struct BlockedLineReport {
    line: usize,
    reason: String,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: mj-report <report.ndjson> [--detail] [--filter=<blocked|slow|changed|errors>] [--trace]");
        std::process::exit(1);
    }

    tracing_subscriber::fmt::init();

    let path_arg = PathBuf::from(&args[1]);
    let path = if path_arg.is_dir() {
        path_arg.join("report.ndjson")
    } else {
        path_arg
    };
    let detail = args.iter().any(|a| a == "--detail");
    let trace = args.iter().any(|a| a == "--trace");
    let filter = args.iter().find_map(|a| a.strip_prefix("--filter="));

    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(err) => {
            error!("mj-report: failed opening {}: {}", path.display(), err);
            std::process::exit(1);
        }
    };

    let reader = BufReader::new(file);
    let mut records: Vec<ReportRecord> = Vec::new();

    for (line_num, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(err) => {
                warn!("mj-report: read error at line {}: {}", line_num + 1, err);
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<ReportRecord>(&line) {
            Ok(record) => records.push(record),
            Err(err) => {
                warn!(
                    "mj-report: parse error at line {}: {}",
                    line_num + 1,
                    err
                );
            }
        }
    }

    let records = apply_filter(records, filter);

    if trace {
        print_trace(&records, detail);
    } else {
        print_report(&records, detail);
    }
}

fn apply_filter(records: Vec<ReportRecord>, filter: Option<&str>) -> Vec<ReportRecord> {
    match filter {
        Some("blocked") => records
            .into_iter()
            .filter(|r| r.policies.iter().any(|p| p.outcome == "Blocked"))
            .collect(),
        Some("slow") => {
            let mut sorted = records;
            sorted.sort_by(|a, b| {
                b.elapsed_engine_ms
                    .partial_cmp(&a.elapsed_engine_ms)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            sorted
        }
        Some("changed") => records.into_iter().filter(|r| r.changed).collect(),
        Some("errors") => records.into_iter().filter(|r| r.error.is_some()).collect(),
        _ => records,
    }
}

fn print_report(records: &[ReportRecord], detail: bool) {
    for record in records {
        let status = if record.changed { "CHANGED" } else { "UNCHANGED" };
        println!(
            "\n── {} ── {} (engine: {:.1}ms, total: {:.1}ms) ──",
            record.path.display(),
            status,
            record.elapsed_engine_ms,
            record.elapsed_total_ms,
        );

        for policy in &record.policies {
            let icon = match policy.outcome.as_str() {
                "Applied" => "  ✓",
                "PartiallyApplied" => "  ~",
                "Blocked" => "  ✗",
                _ => "  -",
            };
            let detail_str = match policy.outcome.as_str() {
                "Applied" => {
                    format!("{} edits ({:.1}ms)", policy.edits.len(), policy.elapsed_ms)
                }
                "PartiallyApplied" => format!(
                    "{}/{} edits ({} dropped) ({:.1}ms)",
                    policy.edits.len(),
                    policy.candidate_count,
                    policy.dropped_count,
                    policy.elapsed_ms,
                ),
                "Blocked" => {
                    let reason = policy.reason.as_deref().unwrap_or("blocked");
                    format!("BLOCKED ({}) ({:.1}ms)", reason, policy.elapsed_ms)
                }
                _ => format!("no change ({:.1}ms)", policy.elapsed_ms),
            };
            println!("{} {:30} {}", icon, policy.policy, detail_str);

            for edit in &policy.edits {
                println!("    L{}: '{}' → '{}'", edit.line, edit.before, edit.after);
            }

            for blocked in &policy.blocked_lines {
                println!("    L{}: BLOCKED ({})", blocked.line, blocked.reason);
            }
        }

        if let Some(err) = &record.error {
            println!("  ERROR: {}", err);
        }
        for warning in &record.warnings {
            println!("  WARN: {}", warning);
        }

        if detail {
            for policy in &record.policies {
                print_policy_detail(policy);
            }
        }
    }

    print_summary(records);
}

fn print_policy_detail(policy: &PolicyReport) {
    println!(
        "    [{}] parse_mode={} candidates={} dropped={} impact_radius={}",
        policy.policy,
        policy.parse_mode,
        policy.candidate_count,
        policy.dropped_count,
        policy.semantic_impact_radius,
    );
    if let (Some(score), Some(threshold)) = (policy.confidence_score, policy.confidence_threshold) {
        println!(
            "      confidence: {:.3} (threshold: {:.3}, {})",
            score,
            threshold,
            if score >= threshold { "PASS" } else { "FAIL" },
        );
    }
}

fn print_trace(records: &[ReportRecord], detail: bool) {
    for record in records {
        println!("\n─── {} ───", record.path.display());
        for policy in &record.policies {
            println!(
                "  {} → {} ({:.1}ms)",
                policy.policy, policy.outcome, policy.elapsed_ms,
            );
            if let Some(reason) = &policy.reason {
                println!("    reason: {}", reason);
            }
            if let (Some(score), Some(threshold)) =
                (policy.confidence_score, policy.confidence_threshold)
            {
                println!("    confidence: {:.3} / {:.3}", score, threshold);
            }
            if detail {
                println!(
                    "    parse_mode={} candidates={} dropped={} impact_radius={}",
                    policy.parse_mode,
                    policy.candidate_count,
                    policy.dropped_count,
                    policy.semantic_impact_radius,
                );
            }
        }
    }
}

fn print_summary(records: &[ReportRecord]) {
    let files = records.len();
    let changed = records.iter().filter(|r| r.changed).count();
    let errors = records.iter().filter(|r| r.error.is_some()).count();
    let edits: usize = records
        .iter()
        .flat_map(|r| &r.policies)
        .map(|p| p.edits.len())
        .sum();
    let blocked: usize = records
        .iter()
        .flat_map(|r| &r.policies)
        .filter(|p| p.outcome == "Blocked")
        .count();

    let total_engine_ms: f64 = records.iter().map(|r| r.elapsed_engine_ms).sum();
    let max_engine_ms = records
        .iter()
        .map(|r| r.elapsed_engine_ms)
        .fold(0.0f64, f64::max);
    let slowest = records
        .iter()
        .max_by(|a, b| {
            a.elapsed_engine_ms
                .partial_cmp(&b.elapsed_engine_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|r| r.path.display().to_string())
        .unwrap_or_default();

    println!(
        "\n{} files | {} changed | {} edits | {} blocked | {} errors",
        files, changed, edits, blocked, errors,
    );
    if files > 0 {
        println!(
            "Timing: total {:.1}ms, avg {:.1}ms/file, slowest: {} ({:.1}ms)",
            total_engine_ms,
            total_engine_ms / files as f64,
            slowest,
            max_engine_ms,
        );
    }

    let mut policy_edits: BTreeMap<&str, usize> = BTreeMap::new();
    for record in records {
        for policy in &record.policies {
            if !policy.edits.is_empty() {
                *policy_edits.entry(&policy.policy).or_insert(0) += policy.edits.len();
            }
        }
    }
    if !policy_edits.is_empty() {
        println!("Edits by policy:");
        for (name, count) in &policy_edits {
            println!("  {:30} {}", name, count);
        }
    }
}
