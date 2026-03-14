use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

pub struct AtomicWriter;

impl AtomicWriter {
    pub fn write_text(path: &Path, content: &str) -> Result<()> {
        Self::write_bytes(path, content.as_bytes())
    }

    pub fn write_bytes(path: &Path, content: &[u8]) -> Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("target path has no parent: {}", path.display()))?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temp_name = format!(
            ".fmt_tmp_{}_{}_{}",
            std::process::id(),
            stamp,
            std::thread::current().name().unwrap_or("t")
        );
        let temp_path = parent.join(temp_name);

        fs::write(&temp_path, content)
            .with_context(|| format!("failed to write temp file {}", temp_path.display()))?;
        fs::rename(&temp_path, path).with_context(|| {
            format!(
                "failed to atomically replace {} with {}",
                path.display(),
                temp_path.display()
            )
        })?;

        Ok(())
    }
}
