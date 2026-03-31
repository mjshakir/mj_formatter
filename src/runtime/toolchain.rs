use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

use crate::parser::manager::ParserManager;

pub struct ToolchainRequirements;

impl ToolchainRequirements {
    pub fn verify(clang_binary: &str, clang_format_binary: &str) -> Result<()> {
        Self::verify_tree_sitter()?;
        Self::verify_command(clang_binary, &["--version"])?;
        Self::verify_clang_format(clang_format_binary)?;
        Self::verify_libclang(clang_binary)?;
        Ok(())
    }

    fn verify_tree_sitter() -> Result<()> {
        if !ParserManager::new().tree_sitter_available() {
            bail!("tree-sitter language initialization failed")
        }
        Ok(())
    }

    fn verify_clang_format(clang_format_binary: &str) -> Result<()> {
        Self::verify_command(clang_format_binary, &["--version"]).with_context(|| {
            format!("required clang-format binary is unavailable: {clang_format_binary}")
        })
    }

    fn verify_command(command: &str, args: &[&str]) -> Result<()> {
        let output = Command::new(command)
            .args(args)
            .output()
            .with_context(|| format!("failed launching required tool: {command}"))?;
        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(anyhow!(
            "required tool '{command}' failed: {}",
            stderr.trim()
        ))
    }

    fn verify_libclang(clang_binary: &str) -> Result<()> {
        if Self::libclang_in_env_path() {
            return Ok(());
        }
        if let Some(path) = Self::libclang_in_venv_paths() {
            env::set_var("LIBCLANG_PATH", &path);
            return Ok(());
        }
        if Self::libclang_via_clang_driver(clang_binary)? {
            return Ok(());
        }
        if Self::libclang_via_llvm_config()? {
            return Ok(());
        }
        if Self::libclang_via_ldconfig()? {
            return Ok(());
        }
        if Self::libclang_in_common_dirs() {
            return Ok(());
        }
        bail!(
            "libclang not found (checked LIBCLANG_PATH, clang -print-file-name, llvm-config, ldconfig, and common library directories)"
        )
    }

    fn libclang_in_env_path() -> bool {
        let Some(value) = env::var_os("LIBCLANG_PATH") else {
            return false;
        };
        let root = PathBuf::from(value);
        if root.is_file() {
            return root
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(Self::is_libclang_file_name);
        }
        Self::directory_has_libclang(root.as_path())
    }

    fn libclang_via_clang_driver(clang_binary: &str) -> Result<bool> {
        let names = [
            "libclang.so",
            "libclang.so.1",
            "libclang.dylib",
            "libclang.dll",
        ];
        for name in names {
            let output = Command::new(clang_binary)
                .arg(format!("-print-file-name={name}"))
                .output()
                .with_context(|| "failed running clang -print-file-name")?;
            if !output.status.success() {
                continue;
            }
            let candidate = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if candidate.is_empty() || candidate == name {
                continue;
            }
            let candidate_path = PathBuf::from(candidate);
            if candidate_path.exists() {
                if let Some(parent) = candidate_path.parent() {
                    env::set_var("LIBCLANG_PATH", parent);
                }
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn libclang_in_venv_paths() -> Option<PathBuf> {
        let mut roots = Vec::new();
        if let Some(virtual_env) = env::var_os("VIRTUAL_ENV") {
            roots.push(PathBuf::from(virtual_env));
        }
        if let Ok(cwd) = env::current_dir() {
            roots.push(cwd.join("venv"));
            roots.push(cwd.join(".venv"));
        }
        if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
            roots.push(home.join("venv"));
            roots.push(home.join(".venv"));
        }

        for root in roots {
            if let Some(path) = Self::libclang_in_venv_root(&root) {
                return Some(path);
            }
        }
        None
    }

    fn libclang_in_venv_root(root: &Path) -> Option<PathBuf> {
        let unix_lib_root = root.join("lib");
        if let Ok(entries) = unix_lib_root.read_dir() {
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                    continue;
                };
                if !name.starts_with("python") {
                    continue;
                }
                let candidate = path.join("site-packages").join("clang").join("native");
                if Self::directory_has_libclang(&candidate) {
                    return Some(candidate);
                }
            }
        }

        let windows_candidate = root
            .join("Lib")
            .join("site-packages")
            .join("clang")
            .join("native");
        if Self::directory_has_libclang(&windows_candidate) {
            return Some(windows_candidate);
        }

        None
    }

    fn libclang_via_llvm_config() -> Result<bool> {
        let output = match Command::new("llvm-config").arg("--libdir").output() {
            Ok(output) => output,
            Err(_) => return Ok(false),
        };
        if !output.status.success() {
            return Ok(false);
        }
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if path.is_empty() {
            return Ok(false);
        }
        let dir = Path::new(path.as_str());
        if Self::directory_has_libclang(dir) {
            env::set_var("LIBCLANG_PATH", dir);
            return Ok(true);
        }
        Ok(false)
    }

    fn libclang_via_ldconfig() -> Result<bool> {
        let output = match Command::new("ldconfig").arg("-p").output() {
            Ok(output) => output,
            Err(_) => return Ok(false),
        };
        if !output.status.success() {
            return Ok(false);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            let trimmed = line.trim();
            if !(trimmed.contains("libclang.so")
                || trimmed.contains("libclang.dylib")
                || trimmed.contains("libclang.dll"))
            {
                continue;
            }
            let Some((_, path)) = trimmed.split_once("=>") else {
                continue;
            };
            let path = PathBuf::from(path.trim());
            if path.exists() {
                if let Some(parent) = path.parent() {
                    env::set_var("LIBCLANG_PATH", parent);
                }
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn libclang_in_common_dirs() -> bool {
        let static_dirs = [
            "/usr/lib",
            "/usr/lib64",
            "/usr/local/lib",
            "/lib",
            "/lib64",
            "/opt/local/lib",
            "/opt/homebrew/opt/llvm/lib",
        ];

        for dir in static_dirs {
            if Self::directory_has_libclang(Path::new(dir)) {
                env::set_var("LIBCLANG_PATH", dir);
                return true;
            }
        }

        if let Some(path) = Self::any_matching_subdir("/usr/lib", "llvm-", "lib") {
            env::set_var("LIBCLANG_PATH", path);
            return true;
        }
        if let Some(path) = Self::any_matching_subdir("/usr/local/opt", "llvm", "lib") {
            env::set_var("LIBCLANG_PATH", path);
            return true;
        }
        if let Some(path) = Self::any_matching_subdir("/opt/homebrew/Cellar/llvm", "", "lib") {
            env::set_var("LIBCLANG_PATH", path);
            return true;
        }
        false
    }

    fn any_matching_subdir(root: &str, prefix: &str, suffix: &str) -> Option<PathBuf> {
        let root_path = Path::new(root);
        let Ok(entries) = root_path.read_dir() else {
            return None;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            if !prefix.is_empty() && !name.starts_with(prefix) {
                continue;
            }
            let candidate = path.join(suffix);
            if Self::directory_has_libclang(candidate.as_path()) {
                return Some(candidate);
            }
        }
        None
    }

    fn directory_has_libclang(dir: &Path) -> bool {
        let Ok(entries) = dir.read_dir() else {
            return false;
        };
        entries.flatten().any(|entry| {
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy().to_lowercase();
            Self::is_libclang_file_name(file_name.as_str())
        })
    }

    fn is_libclang_file_name(file_name: &str) -> bool {
        if !file_name.starts_with("libclang") || file_name.starts_with("libclang-cpp") {
            return false;
        }
        file_name.contains(".so") || file_name.contains(".dylib") || file_name.ends_with(".dll")
    }
}
