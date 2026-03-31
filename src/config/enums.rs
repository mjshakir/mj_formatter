use serde::{Deserialize, Serialize};

// ── Enforcement ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Deserialize, Serialize)]
pub enum Enforcement {
    Must,
    Hard,
    Soft,
    Advisory,
}

impl Enforcement {
    pub fn from_value(raw: &str) -> Self {
        match raw.trim().to_lowercase().as_str() {
            "must" | "required" | "force" => Self::Must,
            "soft" | "normal" | "default" => Self::Soft,
            "advisory" | "relaxed" => Self::Advisory,
            _ => Self::Hard,
        }
    }
}

// ── BackupMode ────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackupMode {
    Suffix,
    Mirror,
}

impl BackupMode {
    pub fn from_value(raw: &str) -> Self {
        match raw.trim().to_lowercase().as_str() {
            "mirror" => Self::Mirror,
            _ => Self::Suffix,
        }
    }
}

// ── ClangArgsMode ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ClangArgsMode {
    Merge,
    CompdbOnly,
    ArgsOnly,
    CompdbThenArgs,
}

impl ClangArgsMode {
    pub fn from_value(raw: &str) -> Self {
        match raw.trim().to_lowercase().as_str() {
            "compdb_only" => Self::CompdbOnly,
            "args_only" => Self::ArgsOnly,
            "compdb_then_args" => Self::CompdbThenArgs,
            _ => Self::Merge,
        }
    }
}
