use std::hash::{Hash, Hasher};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum SemanticRegionKind {
    File,
    Preprocessor,
    Namespace,
    Type,
    Function,
    Declaration,
    Reference,
    Diagnostic,
    Template,
    Attribute,
}

impl SemanticRegionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Preprocessor => "preprocessor",
            Self::Namespace => "namespace",
            Self::Type => "type",
            Self::Function => "function",
            Self::Declaration => "declaration",
            Self::Reference => "reference",
            Self::Diagnostic => "diagnostic",
            Self::Template => "template",
            Self::Attribute => "attribute",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticRegion {
    pub id: u64,
    pub kind: SemanticRegionKind,
    pub start_line: usize,
    pub end_line: usize,
    pub start_offset: usize,
    pub end_offset: usize,
    pub stable_id: Option<String>,
    pub has_diagnostic_error: bool,
}

impl SemanticRegion {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        canonical_path: &str,
        kind: SemanticRegionKind,
        start_line: usize,
        end_line: usize,
        start_offset: usize,
        end_offset: usize,
        stable_id: Option<String>,
        has_diagnostic_error: bool,
    ) -> Self {
        let id = Self::stable_region_id(
            canonical_path,
            kind,
            start_line,
            end_line,
            start_offset,
            end_offset,
            stable_id.as_deref(),
            has_diagnostic_error,
        );
        Self {
            id,
            kind,
            start_line,
            end_line,
            start_offset,
            end_offset,
            stable_id,
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
        kind: SemanticRegionKind,
        start_line: usize,
        end_line: usize,
        start_offset: usize,
        end_offset: usize,
        stable_id: Option<&str>,
        has_diagnostic_error: bool,
    ) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        canonical_path.hash(&mut hasher);
        kind.hash(&mut hasher);
        start_line.hash(&mut hasher);
        end_line.hash(&mut hasher);
        start_offset.hash(&mut hasher);
        end_offset.hash(&mut hasher);
        stable_id.unwrap_or_default().hash(&mut hasher);
        has_diagnostic_error.hash(&mut hasher);
        hasher.finish()
    }
}
