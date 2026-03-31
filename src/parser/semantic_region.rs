use std::hash::{Hash, Hasher};

pub const REGION_FILE: u16 = u16::MAX;
pub const REGION_DECLARATION: u16 = u16::MAX - 1;
pub const REGION_REFERENCE: u16 = u16::MAX - 2;
pub const REGION_DIAGNOSTIC: u16 = u16::MAX - 3;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticRegion {
    pub id: u64,
    pub kind_id: u16,
    pub start_line: usize,
    pub end_line: usize,
    pub start_offset: usize,
    pub end_offset: usize,
    pub has_diagnostic_error: bool,
}

impl SemanticRegion {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        canonical_path: &str,
        kind_id: u16,
        start_line: usize,
        end_line: usize,
        start_offset: usize,
        end_offset: usize,
        stable_id: Option<String>,
        has_diagnostic_error: bool,
    ) -> Self {
        let id = Self::stable_region_id(
            canonical_path,
            kind_id,
            start_line,
            end_line,
            start_offset,
            end_offset,
            stable_id.as_deref(),
            has_diagnostic_error,
        );
        Self {
            id,
            kind_id,
            start_line,
            end_line,
            start_offset,
            end_offset,
            has_diagnostic_error,
        }
    }

    pub fn contains_line(&self, line: usize) -> bool {
        line >= self.start_line && line <= self.end_line
    }

    pub fn width_lines(&self) -> usize {
        self.end_line
            .saturating_sub(self.start_line)
            .saturating_add(1)
    }

    #[allow(clippy::too_many_arguments)]
    fn stable_region_id(
        canonical_path: &str,
        kind_id: u16,
        start_line: usize,
        end_line: usize,
        start_offset: usize,
        end_offset: usize,
        stable_id: Option<&str>,
        has_diagnostic_error: bool,
    ) -> u64 {
        let mut hasher = rustc_hash::FxHasher::default();
        canonical_path.hash(&mut hasher);
        kind_id.hash(&mut hasher);
        start_line.hash(&mut hasher);
        end_line.hash(&mut hasher);
        start_offset.hash(&mut hasher);
        end_offset.hash(&mut hasher);
        stable_id.unwrap_or_default().hash(&mut hasher);
        has_diagnostic_error.hash(&mut hasher);
        hasher.finish()
    }
}
