use rustc_hash::FxHashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum FileUnitKind {
    Paired,
    HeaderOnly,
    ImplementationOnly,
}

impl FileUnitKind {
    pub(crate) fn classify_on_disk(path: &Path) -> Self {
        let role = FileRole::for_path(path);
        match role {
            Some(FileRole::Header) => {
                if !Self::paired_companion_paths_on_disk(path).is_empty() {
                    Self::Paired
                } else {
                    Self::HeaderOnly
                }
            }
            Some(FileRole::Implementation) => {
                if !Self::paired_companion_paths_on_disk(path).is_empty() {
                    Self::Paired
                } else {
                    Self::ImplementationOnly
                }
            }
            None => Self::ImplementationOnly,
        }
    }

    pub(crate) fn paired_companion_paths_on_disk(path: &Path) -> Vec<PathBuf> {
        let role = FileRole::for_path(path);
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            return Vec::new();
        };
        let companion_exts = match role {
            Some(FileRole::Header) => IMPLEMENTATION_EXTENSIONS.as_slice(),
            Some(FileRole::Implementation) => HEADER_EXTENSIONS.as_slice(),
            None => return Vec::new(),
        };
        let Some(parent) = path.parent() else {
            return Vec::new();
        };
        let mut results = Vec::new();
        for parent_dir in Self::candidate_parent_dirs(parent) {
            for ext in companion_exts {
                let candidate = parent_dir.join(format!("{stem}.{ext}"));
                if fs::metadata(candidate.as_path())
                    .map(|meta| meta.is_file())
                    .unwrap_or(false)
                    && !results.contains(&candidate)
                {
                    results.push(candidate);
                }
            }
        }
        results
    }

    fn candidate_parent_dirs(parent: &Path) -> Vec<PathBuf> {
        let parent_key = FileUnitLayout::normalize_path(parent);
        if parent_key.is_empty() {
            return vec![parent.to_path_buf()];
        }
        let mut candidates = vec![parent.to_path_buf()];
        let segments = parent_key.split('/').collect::<Vec<_>>();
        for index in 0..segments.len() {
            let lower = segments[index].to_ascii_lowercase();
            let replacements: &[&str] = if HEADER_DIR_NAMES.iter().any(|d| d.eq_ignore_ascii_case(&lower)) {
                &SOURCE_DIR_NAMES
            } else if SOURCE_DIR_NAMES.iter().any(|d| d.eq_ignore_ascii_case(&lower)) {
                &HEADER_DIR_NAMES
            } else {
                continue;
            };
            for replacement in replacements {
                let mut candidate_segments = segments.clone();
                candidate_segments[index] = replacement;
                let candidate = PathBuf::from(candidate_segments.join("/"));
                if !candidates.contains(&candidate) {
                    candidates.push(candidate);
                }
            }
        }
        candidates
    }
}

#[derive(Clone, Debug)]
pub struct FileUnitLayout {
    descriptors_by_path: FxHashMap<String, FileUnitDescriptor>,
}

#[derive(Clone, Debug)]
struct FileUnitDescriptor {
    kind: FileUnitKind,
    group_key: String,
}

impl FileUnitLayout {
    pub fn from_paths(paths: &[PathBuf]) -> Self {
        let mut groups: FxHashMap<String, FileUnitAccumulator> = FxHashMap::default();
        for path in paths {
            let path_key = Self::normalize_path(path.as_path());
            let unit_key = Self::unit_key(path.as_path());
            let entry = groups.entry(unit_key).or_default();
            entry.paths.push(path_key.clone());
            match FileRole::for_path(path.as_path()) {
                Some(FileRole::Header) => entry.has_header = true,
                Some(FileRole::Implementation) => entry.has_implementation = true,
                None => entry.has_implementation = true,
            }
        }

        let mut descriptors_by_path: FxHashMap<String, FileUnitDescriptor> = FxHashMap::default();
        for (unit_key, group) in groups {
            let kind = if group.has_header && group.has_implementation {
                FileUnitKind::Paired
            } else if group.has_header {
                FileUnitKind::HeaderOnly
            } else {
                FileUnitKind::ImplementationOnly
            };
            for path_key in group.paths {
                let group_key = if kind == FileUnitKind::Paired {
                    unit_key.clone()
                } else {
                    path_key.clone()
                };
                descriptors_by_path.insert(path_key, FileUnitDescriptor { kind, group_key });
            }
        }

        Self {
            descriptors_by_path,
        }
    }

    pub fn kind_for_path(&self, path: &Path) -> FileUnitKind {
        let path_key = Self::normalize_path(path);
        self.descriptors_by_path
            .get(path_key.as_str())
            .map(|descriptor| descriptor.kind)
            .unwrap_or_else(|| FileUnitKind::classify_on_disk(path))
    }

    pub fn group_key_for_path(&self, path: &Path) -> String {
        let path_key = Self::normalize_path(path);
        self.descriptors_by_path
            .get(path_key.as_str())
            .map(|descriptor| descriptor.group_key.clone())
            .unwrap_or(path_key)
    }

    pub fn normalize_path(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "/")
    }

    fn unit_key(path: &Path) -> String {
        let parent = path
            .parent()
            .map(Self::canonical_parent_key)
            .unwrap_or_default();
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if stem.is_empty() {
            return Self::normalize_path(path);
        }
        if parent.is_empty() {
            stem
        } else {
            format!("{parent}/{stem}")
        }
    }

    fn canonical_parent_key(path: &Path) -> String {
        let normalized = Self::normalize_path(path);
        if normalized.is_empty() {
            return normalized;
        }
        let mut segments = normalized.split('/').collect::<Vec<_>>();
        for segment in &mut segments {
            match segment.to_ascii_lowercase().as_str() {
                "include" | "inc" | "src" | "source" => {
                    *segment = "__unit__";
                    break;
                }
                _ => {}
            }
        }
        segments.join("/")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FileRole {
    Header,
    Implementation,
}

impl FileRole {
    fn for_path(path: &Path) -> Option<Self> {
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase());
        let ext = extension.as_deref()?;
        if HEADER_EXTENSIONS.contains(&ext) {
            Some(Self::Header)
        } else if IMPLEMENTATION_EXTENSIONS.contains(&ext) {
            Some(Self::Implementation)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, Default)]
struct FileUnitAccumulator {
    paths: Vec<String>,
    has_header: bool,
    has_implementation: bool,
}

pub(crate) const HEADER_EXTENSIONS: [&str; 6] = ["h", "hh", "hpp", "hxx", "ipp", "inl"];
pub(crate) const IMPLEMENTATION_EXTENSIONS: [&str; 4] = ["c", "cc", "cpp", "cxx"];
pub(crate) const HEADER_DIR_NAMES: [&str; 2] = ["include", "inc"];
pub(crate) const SOURCE_DIR_NAMES: [&str; 2] = ["src", "source"];

pub(crate) fn is_header_extension(ext: &str) -> bool {
    HEADER_EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(ext))
}

pub(crate) fn is_implementation_extension(ext: &str) -> bool {
    IMPLEMENTATION_EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(ext))
}

#[cfg(test)]
mod tests {
    use super::{FileUnitKind, FileUnitLayout};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("fmt_{name}_{stamp}"));
        fs::create_dir_all(dir.as_path()).expect("create temp dir");
        dir
    }

    #[test]
    fn layout_classifies_pairs() {
        let root = temp_dir("file_unit_layout");
        let include_dir = root.join("include");
        let src_dir = root.join("src");
        fs::create_dir_all(include_dir.as_path()).expect("create include dir");
        fs::create_dir_all(src_dir.as_path()).expect("create src dir");
        let pair_header = include_dir.join("Widget.hpp");
        let pair_impl = src_dir.join("Widget.cpp");
        let header_only = root.join("Config.hpp");
        let impl_only = root.join("main.cpp");
        fs::write(pair_header.as_path(), b"// header").expect("write pair header");
        fs::write(pair_impl.as_path(), b"// impl").expect("write pair impl");
        fs::write(header_only.as_path(), b"// header only").expect("write header only");
        fs::write(impl_only.as_path(), b"// impl only").expect("write impl only");

        let layout = FileUnitLayout::from_paths(&[
            pair_header.clone(),
            pair_impl.clone(),
            header_only.clone(),
            impl_only.clone(),
        ]);
        assert_eq!(
            layout.kind_for_path(pair_header.as_path()),
            FileUnitKind::Paired
        );
        assert_eq!(
            layout.kind_for_path(pair_impl.as_path()),
            FileUnitKind::Paired
        );
        assert_eq!(
            layout.kind_for_path(header_only.as_path()),
            FileUnitKind::HeaderOnly
        );
        assert_eq!(
            layout.kind_for_path(impl_only.as_path()),
            FileUnitKind::ImplementationOnly
        );
        assert_eq!(
            layout.group_key_for_path(pair_header.as_path()),
            layout.group_key_for_path(pair_impl.as_path())
        );
        assert_ne!(
            layout.group_key_for_path(header_only.as_path()),
            layout.group_key_for_path(impl_only.as_path())
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn classify_paired_header() {
        let root = temp_dir("file_unit_disk");
        let include_dir = root.join("include");
        let src_dir = root.join("src");
        fs::create_dir_all(include_dir.as_path()).expect("create include dir");
        fs::create_dir_all(src_dir.as_path()).expect("create src dir");
        let pair_header = include_dir.join("Node.hpp");
        let pair_impl = src_dir.join("Node.cpp");
        let header_only = root.join("Traits.hpp");
        fs::write(pair_header.as_path(), b"// header").expect("write pair header");
        fs::write(pair_impl.as_path(), b"// impl").expect("write pair impl");
        fs::write(header_only.as_path(), b"// header only").expect("write header only");

        assert_eq!(
            FileUnitKind::classify_on_disk(pair_header.as_path()),
            FileUnitKind::Paired
        );
        assert_eq!(
            FileUnitKind::classify_on_disk(pair_impl.as_path()),
            FileUnitKind::Paired
        );
        assert_eq!(
            FileUnitKind::classify_on_disk(header_only.as_path()),
            FileUnitKind::HeaderOnly
        );

        let _ = fs::remove_dir_all(root);
    }
}
