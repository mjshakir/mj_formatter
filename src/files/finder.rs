use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use walkdir::WalkDir;

use crate::config::app_config::AppConfig;

pub struct FileFinder {
    root: PathBuf,
    include: GlobSet,
    exclude: GlobSet,
    has_include: bool,
}

impl FileFinder {
    pub fn new(config: &AppConfig) -> Result<Self> {
        let include = Self::compile(&config.include_patterns)?;
        let exclude = Self::compile(&config.exclude_patterns)?;
        Ok(Self {
            root: config.root.clone(),
            include,
            exclude,
            has_include: !config.include_patterns.is_empty(),
        })
    }

    pub fn collect(&self) -> Result<Vec<PathBuf>> {
        if !self.has_include {
            return Ok(Vec::new());
        }

        let mut result: Vec<PathBuf> = Vec::new();
        for entry in WalkDir::new(&self.root)
            .follow_links(false)
            .into_iter()
            .filter_map(|item| item.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let relative = match entry.path().strip_prefix(&self.root) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let norm = Self::normalize(relative);
            if !self.include.is_match(&norm) {
                continue;
            }
            if self.exclude.is_match(&norm) {
                continue;
            }
            result.push(entry.path().to_path_buf());
        }

        result.sort();
        Ok(result)
    }

    fn compile(patterns: &[String]) -> Result<GlobSet> {
        let mut builder = GlobSetBuilder::new();
        for pattern in patterns {
            let glob =
                Glob::new(pattern).with_context(|| format!("invalid glob pattern: {pattern}"))?;
            builder.add(glob);
        }
        builder.build().context("failed to compile glob set")
    }

    fn normalize(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "/")
    }
}
