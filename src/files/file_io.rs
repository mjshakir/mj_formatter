use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::app_config::AppConfig;
use crate::config::enums::BackupMode;
use crate::files::atomic_writer::AtomicWriter;

#[derive(Clone, Debug)]
pub struct FileIo {
    root: PathBuf,
    backup: bool,
    backup_mode: BackupMode,
    backup_suffix: String,
    backup_root: PathBuf,
}

impl FileIo {
    pub fn new(config: &AppConfig) -> Self {
        let backup_run = env::var("FORMATTER_BACKUP_RUN")
            .ok()
            .filter(|value| !value.trim().is_empty());
        let backup_root = if let Some(run_id) = backup_run {
            config.backup_dir.join(run_id)
        } else {
            config.backup_dir.clone()
        };
        Self {
            root: config.root.clone(),
            backup: config.backup,
            backup_mode: config.backup_mode.clone(),
            backup_suffix: config.backup_suffix.clone(),
            backup_root,
        }
    }

    pub fn read_text(&self, path: &Path) -> Result<String> {
        fs::read_to_string(path).with_context(|| format!("failed reading {}", path.display()))
    }

    pub fn write_text(&self, path: &Path, text: &str) -> Result<Option<PathBuf>> {
        let backup = if self.backup {
            Some(self.make_backup(path)?)
        } else {
            None
        };
        AtomicWriter::write_text(path, text)
            .with_context(|| format!("failed writing {}", path.display()))?;
        Ok(backup)
    }

    fn make_backup(&self, path: &Path) -> Result<PathBuf> {
        let rel = path
            .strip_prefix(&self.root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| path.to_path_buf());

        let mut target = match self.backup_mode {
            BackupMode::Mirror => self.backup_root.join(&rel),
            BackupMode::Suffix => {
                let file_name = rel
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("backup");
                let renamed = format!("{file_name}{}", self.backup_suffix);
                self.backup_root.join(rel.with_file_name(renamed))
            }
        };

        if target.exists() {
            target = Self::unique_backup_name(&target);
        }

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed creating backup dir {}", parent.display()))?;
        }
        fs::copy(path, &target).with_context(|| {
            format!(
                "failed creating backup {} from {}",
                target.display(),
                path.display()
            )
        })?;
        Ok(target)
    }

    fn unique_backup_name(path: &Path) -> PathBuf {
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("backup");
        let ext = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        let mut counter = 1usize;
        loop {
            let file_name = if ext.is_empty() {
                format!("{stem}.{counter}")
            } else {
                format!("{stem}.{counter}.{ext}")
            };
            let candidate = path.with_file_name(file_name);
            if !candidate.exists() {
                return candidate;
            }
            counter += 1;
        }
    }
}
