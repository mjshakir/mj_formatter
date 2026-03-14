use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::Path;

use tree_sitter::{Node, StreamingIterator, Tree};

use crate::parser::clang_types::ClangDeclKey;
use crate::parser::clang_result::ClangDiagnosticSeverity;
use crate::parser::clang_result::{
    ClangDiagnosticEntry, ClangDiagnosticSummary, ClangParseResult,
};
use crate::parser::clang_symbol::ClangSymbol;
use crate::parser::clang_types::ClangSymbolKind;
use crate::parser::semantic_region::{SemanticRegion, SemanticRegionKind};
use crate::text_scan;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticIdProvenance {
    Usr,
    SourceLocation,
    DeclLocation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticScopeKind {
    Namespace,
    Type,
    Function,
    Preprocessor,
    Template,
    Attribute,
}

#[derive(Clone, Debug)]
pub struct SemanticDeclaration {
    pub stable_id: String,
    pub provenance: SemanticIdProvenance,
    pub name: String,
    pub kind: ClangSymbolKind,
    pub line: usize,
    pub column: usize,
    pub usr: Option<String>,
    pub scope_usr: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SemanticReference {
    pub stable_id: String,
    pub provenance: SemanticIdProvenance,
    pub decl_path: String,
    pub decl_kind: ClangSymbolKind,
    pub offset: usize,
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Debug)]
pub struct SemanticScope {
    pub kind: SemanticScopeKind,
    pub node_kind: &'static str,
    pub start_offset: usize,
    pub end_offset: usize,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct SourceLocation {
    pub line: usize,
    pub column: usize,
}

impl SourceLocation {
    pub fn new(line: usize, column: usize) -> Self {
        Self {
            line: line.max(1),
            column: column.max(1),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SemanticFileContext {
    pub canonical_path: String,
    pub clang_success: bool,
    pub tree_has_error: bool,
    pub diagnostic_summary: ClangDiagnosticSummary,
    pub diagnostic_entries: Vec<ClangDiagnosticEntry>,
    pub declarations: Vec<SemanticDeclaration>,
    pub references: Vec<SemanticReference>,
    pub scopes: Vec<SemanticScope>,
    pub regions: Vec<SemanticRegion>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SemanticSummary {
    pub declaration_count: usize,
    pub reference_count: usize,
    pub scope_count: usize,
    pub region_count: usize,
    pub preprocessor_scope_count: usize,
    pub usr_backed_declaration_count: usize,
    pub diagnostics_count: usize,
    pub diagnostic_error_count: usize,
    pub semantic_signature: u64,
}

impl SemanticFileContext {
    pub fn from_parses(
        text: &str,
        path: &Path,
        tree: Option<&Tree>,
        clang: Option<&ClangParseResult>,
    ) -> Self {
        Self::from_parses_with_cache(text, path, tree, clang, None)
    }

    pub fn from_parses_with_cache(
        text: &str,
        path: &Path,
        tree: Option<&Tree>,
        clang: Option<&ClangParseResult>,
        query_cache: Option<&crate::parser::query_cache::TsQueryCache>,
    ) -> Self {
        let canonical_path = Self::normalize_path(path);
        let mut context = Self {
            canonical_path: canonical_path.clone(),
            tree_has_error: tree
                .map(|value| value.root_node().has_error())
                .unwrap_or(false),
            ..Self::default()
        };

        if let Some(tree) = tree {
            context.scopes = Self::collect_scopes(tree, query_cache);
        }

        let Some(parse) = clang else {
            context.regions = Self::build_regions(text, &context);
            return context;
        };
        context.clang_success = parse.success;
        context.diagnostic_summary = parse.diagnostic_summary();
        context.diagnostic_entries = parse.diagnostic_entries().to_vec();

        let line_starts = Self::line_starts(text);
        let mut declaration_ids = HashMap::<ClangDeclKey, (String, SemanticIdProvenance)>::new();
        for symbol in &parse.symbols {
            let (stable_id, provenance) = Self::stable_id_for_symbol(path, symbol);
            context.declarations.push(SemanticDeclaration {
                stable_id: stable_id.clone(),
                provenance,
                name: symbol.name.clone(),
                kind: symbol.kind,
                line: symbol.line,
                column: symbol.column,
                usr: symbol.usr.clone(),
                scope_usr: symbol.scope_usr.clone(),
            });
            let key = ClangDeclKey::new(
                canonical_path.clone(),
                symbol.line,
                symbol.column,
                symbol.kind,
            );
            declaration_ids.insert(key, (stable_id, provenance));
        }
        if parse.symbols.is_empty() {
            for decl_key in parse.reference_offsets_map().keys() {
                if decl_key.path != canonical_path
                    || declaration_ids.contains_key(decl_key)
                    || decl_key.line == 0
                    || decl_key.column == 0
                {
                    continue;
                }
                let name = Self::identifier_at_location(
                    text,
                    line_starts.as_slice(),
                    decl_key.line,
                    decl_key.column,
                )
                .unwrap_or_else(|| format!("decl_{}_{}", decl_key.line, decl_key.column));
                let (stable_id, provenance) = Self::stable_id_for_decl_key(decl_key, &name);
                context.declarations.push(SemanticDeclaration {
                    stable_id: stable_id.clone(),
                    provenance,
                    name,
                    kind: decl_key.kind,
                    line: decl_key.line,
                    column: decl_key.column,
                    usr: None,
                    scope_usr: None,
                });
                declaration_ids.insert(decl_key.clone(), (stable_id, provenance));
            }
        }
        context.declarations.sort_by(|left, right| {
            left.line
                .cmp(&right.line)
                .then(left.column.cmp(&right.column))
                .then(left.name.cmp(&right.name))
        });

        for (decl_key, offsets) in parse.reference_offsets_map() {
            let (stable_id, provenance) =
                if let Some((stable_id, provenance)) = declaration_ids.get(decl_key) {
                    (stable_id.clone(), *provenance)
                } else {
                    let ref_name = Self::identifier_at_location(
                        text,
                        line_starts.as_slice(),
                        decl_key.line,
                        decl_key.column,
                    )
                    .unwrap_or_else(|| format!("ref_{}_{}", decl_key.line, decl_key.column));
                    Self::stable_id_for_decl_key(decl_key, &ref_name)
                };
            for offset in offsets {
                let (line, column) = Self::line_and_column_for_offset(&line_starts, *offset);
                context.references.push(SemanticReference {
                    stable_id: stable_id.clone(),
                    provenance,
                    decl_path: decl_key.path.clone(),
                    decl_kind: decl_key.kind,
                    offset: *offset,
                    line,
                    column,
                });
            }
        }
        context.references.sort_by(|left, right| {
            left.offset
                .cmp(&right.offset)
                .then(left.stable_id.cmp(&right.stable_id))
        });
        context.references.dedup_by(|left, right| {
            left.offset == right.offset && left.stable_id == right.stable_id
        });
        context.regions = Self::build_regions(text, &context);

        context
    }

    pub fn declaration_at_location(
        &self,
        location: SourceLocation,
        allowed_kinds: &[ClangSymbolKind],
    ) -> Option<&SemanticDeclaration> {
        let allow_kind =
            |kind: ClangSymbolKind| allowed_kinds.is_empty() || allowed_kinds.contains(&kind);
        if let Some(exact) = self.declarations.iter().find(|declaration| {
            declaration.line == location.line
                && declaration.column == location.column
                && allow_kind(declaration.kind)
        }) {
            return Some(exact);
        }
        self.declarations
            .iter()
            .filter(|declaration| declaration.line == location.line && allow_kind(declaration.kind))
            .min_by_key(|declaration| declaration.column.abs_diff(location.column))
    }

    pub fn references_for_stable_id(&self, stable_id: &str) -> Vec<&SemanticReference> {
        self.references
            .iter()
            .filter(|reference| reference.stable_id == stable_id)
            .collect()
    }

    pub fn scope_at_location(&self, location: SourceLocation) -> Option<&SemanticScope> {
        self.scopes
            .iter()
            .filter(|scope| location.line >= scope.start_line && location.line <= scope.end_line)
            .min_by_key(|scope| {
                (
                    scope.end_line.saturating_sub(scope.start_line),
                    usize::MAX.saturating_sub(scope.start_offset),
                )
            })
    }

    pub fn region_at_location(&self, location: SourceLocation) -> Option<&SemanticRegion> {
        self.regions
            .iter()
            .filter(|region| region.contains_line(location.line))
            .min_by_key(|region| {
                (
                    region.width_lines(),
                    region.end_offset.saturating_sub(region.start_offset),
                    region.start_offset,
                )
            })
    }

    pub fn is_macro_region(&self, location: SourceLocation) -> bool {
        self.scopes.iter().any(|scope| {
            scope.kind == SemanticScopeKind::Preprocessor
                && location.line >= scope.start_line
                && location.line <= scope.end_line
        })
    }

    pub fn summary(&self) -> SemanticSummary {
        let mut signature = Self::hash64(self.canonical_path.as_str());
        signature ^= self.clang_success as u64;
        signature ^= (self.tree_has_error as u64) << 1;

        let mut preprocessor_scope_count = 0usize;
        for scope in &self.scopes {
            if scope.kind == SemanticScopeKind::Preprocessor {
                preprocessor_scope_count = preprocessor_scope_count.saturating_add(1);
            }
            signature ^= Self::hash64(
                format!(
                    "{}:{}:{}:{}:{}",
                    scope.node_kind,
                    scope.start_offset,
                    scope.end_offset,
                    scope.start_line,
                    scope.end_line
                )
                .as_str(),
            );
        }

        let mut usr_backed_declaration_count = 0usize;
        let mut stable_declaration_count = 0usize;
        for declaration in &self.declarations {
            if declaration.kind == ClangSymbolKind::FunctionTemplate {
                continue;
            }
            stable_declaration_count = stable_declaration_count.saturating_add(1);
            if declaration.usr.is_some() || declaration.provenance == SemanticIdProvenance::Usr {
                usr_backed_declaration_count = usr_backed_declaration_count.saturating_add(1);
            }
            signature ^= Self::hash64(
                format!(
                    "{}:{:?}:{}:{}:{}:{:?}:{:?}",
                    declaration.stable_id,
                    declaration.kind,
                    declaration.name,
                    declaration.line,
                    declaration.column,
                    declaration.usr,
                    declaration.scope_usr
                )
                .as_str(),
            );
        }

        for reference in &self.references {
            signature ^= Self::hash64(
                format!(
                    "{}:{:?}:{}:{:?}:{}:{}:{}",
                    reference.stable_id,
                    reference.provenance,
                    reference.decl_path,
                    reference.decl_kind,
                    reference.offset,
                    reference.line,
                    reference.column
                )
                .as_str(),
            );
        }

        for diagnostic in &self.diagnostic_entries {
            signature ^= Self::hash64(
                format!(
                    "{}:{}:{:?}",
                    diagnostic.line, diagnostic.column, diagnostic.severity
                )
                .as_str(),
            );
        }
        for region in &self.regions {
            signature ^= Self::hash64(
                format!(
                    "{}:{}:{}:{}:{}:{:?}:{}",
                    region.id,
                    region.kind.as_str(),
                    region.start_line,
                    region.end_line,
                    region.start_offset,
                    region.stable_id,
                    region.has_diagnostic_error
                )
                .as_str(),
            );
        }

        SemanticSummary {
            declaration_count: stable_declaration_count,
            reference_count: self.references.len(),
            scope_count: self.scopes.len(),
            region_count: self.regions.len(),
            preprocessor_scope_count,
            usr_backed_declaration_count,
            diagnostics_count: self.diagnostic_entries.len(),
            diagnostic_error_count: self.diagnostic_summary.error_total(),
            semantic_signature: signature,
        }
    }

    const SCOPE_QUERY: &str = r#"[
        (namespace_definition) @namespace
        (class_specifier) @type
        (struct_specifier) @type
        (union_specifier) @type
        (enum_specifier) @type
        (function_definition) @function
        (function_declarator) @function
        (lambda_expression) @function
        (preproc_if) @preproc
        (preproc_ifdef) @preproc
        (preproc_elif) @preproc
        (preproc_else) @preproc
        (preproc_def) @preproc
        (preproc_function_def) @preproc
        (template_declaration) @template
        (attribute_declaration) @attribute
    ]"#;

    fn collect_scopes(
        tree: &Tree,
        query_cache: Option<&crate::parser::query_cache::TsQueryCache>,
    ) -> Vec<SemanticScope> {
        let mut scopes = Vec::<SemanticScope>::new();

        if let Some(query) = query_cache
            .and_then(|qc| qc.get_or_compile(Self::SCOPE_QUERY).ok())
        {
            let namespace_idx = query.capture_index_for_name("namespace");
            let type_idx = query.capture_index_for_name("type");
            let function_idx = query.capture_index_for_name("function");
            let preproc_idx = query.capture_index_for_name("preproc");
            let template_idx = query.capture_index_for_name("template");
            let attribute_idx = query.capture_index_for_name("attribute");

            let mut cursor = tree_sitter::QueryCursor::new();
            let mut matches = cursor.matches(&query, tree.root_node(), "".as_bytes());
            while let Some(m) = {
                matches.advance();
                matches.get()
            } {
                for capture in m.captures {
                    let kind = if Some(capture.index) == namespace_idx {
                        SemanticScopeKind::Namespace
                    } else if Some(capture.index) == type_idx {
                        SemanticScopeKind::Type
                    } else if Some(capture.index) == function_idx {
                        SemanticScopeKind::Function
                    } else if Some(capture.index) == preproc_idx {
                        SemanticScopeKind::Preprocessor
                    } else if Some(capture.index) == template_idx {
                        SemanticScopeKind::Template
                    } else if Some(capture.index) == attribute_idx {
                        SemanticScopeKind::Attribute
                    } else {
                        continue;
                    };
                    scopes.push(SemanticScope {
                        kind,
                        node_kind: capture.node.kind(),
                        start_offset: capture.node.start_byte(),
                        end_offset: capture.node.end_byte(),
                        start_line: capture.node.start_position().row + 1,
                        end_line: capture.node.end_position().row + 1,
                    });
                }
            }
        } else {
            let mut stack = vec![tree.root_node()];
            while let Some(node) = stack.pop() {
                if let Some(kind) = Self::scope_kind_for_node(node) {
                    scopes.push(SemanticScope {
                        kind,
                        node_kind: node.kind(),
                        start_offset: node.start_byte(),
                        end_offset: node.end_byte(),
                        start_line: node.start_position().row + 1,
                        end_line: node.end_position().row + 1,
                    });
                }
                for idx in (0..node.child_count()).rev() {
                    if let Some(child) = node.child(idx as u32) {
                        stack.push(child);
                    }
                }
            }
        }

        scopes.sort_by(|left, right| {
            left.start_offset
                .cmp(&right.start_offset)
                .then(left.end_offset.cmp(&right.end_offset))
                .then(left.node_kind.cmp(right.node_kind))
        });
        scopes
    }

    fn build_regions(text: &str, context: &SemanticFileContext) -> Vec<SemanticRegion> {
        let mut regions = Vec::<SemanticRegion>::new();
        let line_starts = Self::line_starts(text);
        let total_lines = Self::line_count(text).max(1);
        let text_len = text.len();
        regions.push(SemanticRegion::new(
            context.canonical_path.as_str(),
            SemanticRegionKind::File,
            1,
            total_lines,
            0,
            text_len,
            None,
            false,
        ));

        for scope in &context.scopes {
            regions.push(SemanticRegion::new(
                context.canonical_path.as_str(),
                Self::region_kind_for_scope(scope.kind),
                scope.start_line,
                scope.end_line.max(scope.start_line),
                scope.start_offset,
                scope.end_offset.max(scope.start_offset),
                None,
                false,
            ));
        }

        for declaration in &context.declarations {
            let (start_offset, end_offset) =
                Self::line_bounds_for_line(line_starts.as_slice(), text_len, declaration.line);
            regions.push(SemanticRegion::new(
                context.canonical_path.as_str(),
                SemanticRegionKind::Declaration,
                declaration.line.max(1),
                declaration.line.max(1),
                start_offset,
                end_offset,
                Some(declaration.stable_id.clone()),
                false,
            ));
        }

        for reference in &context.references {
            let (start_offset, end_offset) =
                Self::line_bounds_for_line(line_starts.as_slice(), text_len, reference.line);
            regions.push(SemanticRegion::new(
                context.canonical_path.as_str(),
                SemanticRegionKind::Reference,
                reference.line.max(1),
                reference.line.max(1),
                start_offset,
                end_offset,
                Some(reference.stable_id.clone()),
                false,
            ));
        }

        for diagnostic in &context.diagnostic_entries {
            if diagnostic.line == 0 {
                continue;
            }
            let (start_offset, end_offset) =
                Self::line_bounds_for_line(line_starts.as_slice(), text_len, diagnostic.line);
            let has_error = matches!(
                diagnostic.severity,
                ClangDiagnosticSeverity::Error | ClangDiagnosticSeverity::Fatal
            );
            regions.push(SemanticRegion::new(
                context.canonical_path.as_str(),
                SemanticRegionKind::Diagnostic,
                diagnostic.line,
                diagnostic.line,
                start_offset,
                end_offset,
                None,
                has_error,
            ));
        }

        Self::dedup_and_sort_regions(&mut regions);
        regions
    }

    fn region_kind_for_scope(scope_kind: SemanticScopeKind) -> SemanticRegionKind {
        match scope_kind {
            SemanticScopeKind::Preprocessor => SemanticRegionKind::Preprocessor,
            SemanticScopeKind::Namespace => SemanticRegionKind::Namespace,
            SemanticScopeKind::Type => SemanticRegionKind::Type,
            SemanticScopeKind::Function => SemanticRegionKind::Function,
            SemanticScopeKind::Template => SemanticRegionKind::Template,
            SemanticScopeKind::Attribute => SemanticRegionKind::Attribute,
        }
    }

    fn dedup_and_sort_regions(regions: &mut Vec<SemanticRegion>) {
        regions.sort_by(|left, right| {
            left.start_offset
                .cmp(&right.start_offset)
                .then(left.end_offset.cmp(&right.end_offset))
                .then(left.kind.cmp(&right.kind))
                .then(left.stable_id.cmp(&right.stable_id))
                .then(left.id.cmp(&right.id))
        });
        regions.dedup_by(|left, right| {
            left.kind == right.kind
                && left.start_line == right.start_line
                && left.end_line == right.end_line
                && left.start_offset == right.start_offset
                && left.end_offset == right.end_offset
                && left.stable_id == right.stable_id
                && left.has_diagnostic_error == right.has_diagnostic_error
        });
    }

    fn scope_kind_for_node(node: Node<'_>) -> Option<SemanticScopeKind> {
        let kind = node.kind();
        if kind.starts_with("preproc_") {
            return Some(SemanticScopeKind::Preprocessor);
        }
        match kind {
            "namespace_definition" => Some(SemanticScopeKind::Namespace),
            "class_specifier" | "struct_specifier" | "union_specifier" | "enum_specifier" => {
                Some(SemanticScopeKind::Type)
            }
            "function_definition" | "function_declarator" | "lambda_expression" => {
                Some(SemanticScopeKind::Function)
            }
            "template_declaration" => Some(SemanticScopeKind::Template),
            "attribute_declaration" => Some(SemanticScopeKind::Attribute),
            _ => None,
        }
    }

    fn line_starts(text: &str) -> Vec<usize> {
        text_scan::line_starts(text, false)
    }

    fn line_count(text: &str) -> usize {
        if text.is_empty() {
            return 1;
        }
        text_scan::count_byte(text.as_bytes(), b'\n').saturating_add(1)
    }

    fn line_bounds_for_line(line_starts: &[usize], text_len: usize, line: usize) -> (usize, usize) {
        if line == 0 || line_starts.is_empty() {
            return (0, text_len);
        }
        let index = line
            .saturating_sub(1)
            .min(line_starts.len().saturating_sub(1));
        let start = line_starts[index].min(text_len);
        let end = if index + 1 < line_starts.len() {
            line_starts[index + 1].saturating_sub(1).min(text_len)
        } else {
            text_len
        };
        (start, end.max(start))
    }

    fn line_and_column_for_offset(line_starts: &[usize], offset: usize) -> (usize, usize) {
        let line_index = match line_starts.binary_search(&offset) {
            Ok(index) => index,
            Err(0) => 0,
            Err(index) => index.saturating_sub(1),
        };
        let line = line_index + 1;
        let line_start = line_starts.get(line_index).copied().unwrap_or(0);
        let column = offset.saturating_sub(line_start) + 1;
        (line, column)
    }

    fn identifier_at_location(
        text: &str,
        line_starts: &[usize],
        line: usize,
        column: usize,
    ) -> Option<String> {
        if line == 0 || column == 0 || line > line_starts.len() {
            return None;
        }
        let line_start = line_starts[line - 1];
        let line_end = if line < line_starts.len() {
            line_starts[line].saturating_sub(1)
        } else {
            text.len()
        }
        .min(text.len());
        if line_start >= line_end {
            return None;
        }
        let mut cursor = line_start.saturating_add(column.saturating_sub(1));
        while cursor < line_end && !text.is_char_boundary(cursor) {
            cursor = cursor.saturating_add(1);
        }
        let bytes = text.as_bytes();
        while cursor < line_end {
            let ch = bytes[cursor];
            if ch == b'_' || ch.is_ascii_alphabetic() {
                break;
            }
            cursor = cursor.saturating_add(1);
        }
        if cursor >= line_end {
            return None;
        }
        let start = cursor;
        cursor = cursor.saturating_add(1);
        while cursor < line_end {
            let ch = bytes[cursor];
            if ch == b'_' || ch.is_ascii_alphanumeric() {
                cursor = cursor.saturating_add(1);
            } else {
                break;
            }
        }
        text.get(start..cursor).map(str::to_string)
    }

    fn stable_id_for_symbol(path: &Path, symbol: &ClangSymbol) -> (String, SemanticIdProvenance) {
        if let Some(usr) = symbol
            .usr
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return (format!("usr:{usr}"), SemanticIdProvenance::Usr);
        }
        let scope = symbol.scope_usr.as_deref().unwrap_or("-");
        let payload = format!(
            "{}|{:?}|{}|{}|{}",
            Self::normalize_path(path),
            symbol.kind,
            scope,
            symbol.line,
            symbol.name
        );
        (
            format!("loc:{:016x}", Self::hash64(payload.as_str())),
            SemanticIdProvenance::SourceLocation,
        )
    }

    fn stable_id_for_decl_key(decl_key: &ClangDeclKey, name: &str) -> (String, SemanticIdProvenance) {
        let payload = format!(
            "{}|{:?}|{}|{}",
            decl_key.path, decl_key.kind, decl_key.line, name
        );
        (
            format!("decl:{:016x}", Self::hash64(payload.as_str())),
            SemanticIdProvenance::DeclLocation,
        )
    }

    fn normalize_path(path: &Path) -> String {
        fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .to_string()
    }

    fn hash64(value: &str) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use tree_sitter::Parser;

    use crate::parser::clang_types::ClangDeclKey;
    use crate::parser::clang_result::{ClangDiagnosticSummary, ClangParseResult};
    use crate::parser::clang_symbol::ClangSymbol;
    use crate::parser::clang_types::ClangSymbolKind;
    use crate::parser::file_context::{
        SemanticFileContext, SemanticIdProvenance, SemanticScopeKind, SourceLocation,
    };
    use crate::parser::semantic_region::{SemanticRegion, SemanticRegionKind};
    use crate::parser::node_kind;

    #[test]
    fn stable_id_prefers_usr() {
        let path = PathBuf::from("semantic_usr.cpp");
        let parse = ClangParseResult::new(
            true,
            Vec::new(),
            vec![ClangSymbol {
                name: "Foo".to_string(),
                kind: ClangSymbolKind::Function,
                line: 1,
                column: 5,
                usr: Some("c:@F@Foo#".to_string()),
                scope_usr: None,
            }],
            ClangDiagnosticSummary::default(),
            Vec::new(),
        );
        let context = SemanticFileContext::from_parses("int Foo();\n", &path, None, Some(&parse));
        assert_eq!(context.declarations.len(), 1);
        assert!(context.declarations[0].stable_id.starts_with("usr:"));
        assert_eq!(
            context.declarations[0].provenance,
            SemanticIdProvenance::Usr
        );
    }

    #[test]
    fn references_resolve_to_declaration_identity() {
        let path = PathBuf::from("semantic_refs.cpp");
        let canonical_path = std::fs::canonicalize(&path)
            .unwrap_or_else(|_| path.clone())
            .to_string_lossy()
            .to_string();
        let key = ClangDeclKey::new(canonical_path, 1, 5, ClangSymbolKind::Variable);
        let mut reference_map = HashMap::<ClangDeclKey, Vec<usize>>::new();
        reference_map.insert(key, vec![4, 13]);
        let parse = ClangParseResult::with_semantic_offsets(
            true,
            Vec::new(),
            vec![ClangSymbol {
                name: "Foo".to_string(),
                kind: ClangSymbolKind::Variable,
                line: 1,
                column: 5,
                usr: None,
                scope_usr: None,
            }],
            HashMap::new(),
            reference_map,
            ClangDiagnosticSummary::default(),
            Vec::new(),
        );
        let text = "int Foo = 0;\nFoo++;\n";
        let context = SemanticFileContext::from_parses(text, &path, None, Some(&parse));
        assert_eq!(context.declarations.len(), 1);
        assert!(!context.references.is_empty());
        assert!(context
            .references
            .iter()
            .all(|reference| reference.stable_id == context.declarations[0].stable_id));
    }

    #[test]
    fn synthesizes_declaration_from_reference_map_when_symbols_missing() {
        let path = PathBuf::from("semantic_refs_fallback.cpp");
        let canonical_path = std::fs::canonicalize(&path)
            .unwrap_or_else(|_| path.clone())
            .to_string_lossy()
            .to_string();
        let key = ClangDeclKey::new(canonical_path, 1, 5, ClangSymbolKind::Variable);
        let mut reference_map = HashMap::<ClangDeclKey, Vec<usize>>::new();
        reference_map.insert(key, vec![4, 13]);
        let parse = ClangParseResult::with_semantic_offsets(
            true,
            Vec::new(),
            Vec::new(),
            HashMap::new(),
            reference_map,
            ClangDiagnosticSummary::default(),
            Vec::new(),
        );
        let text = "int Foo = 0;\nFoo++;\n";
        let context = SemanticFileContext::from_parses(text, &path, None, Some(&parse));
        assert_eq!(context.declarations.len(), 1);
        assert_eq!(context.declarations[0].line, 1);
        assert_eq!(context.declarations[0].column, 5);
        assert_eq!(context.declarations[0].name, "Foo");
        assert_eq!(context.references.len(), 2);
        assert!(context
            .references
            .iter()
            .all(|reference| reference.stable_id == context.declarations[0].stable_id));
    }

    #[test]
    fn collects_preprocessor_scopes_from_tree_sitter() {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("cpp language");
        let text = "#if 1\nint x;\n#endif\n";
        let tree = parser.parse(text, None).expect("parse tree");
        let context =
            SemanticFileContext::from_parses(text, Path::new("scope.cpp"), Some(&tree), None);
        assert!(context
            .scopes
            .iter()
            .any(|scope| scope.kind == SemanticScopeKind::Preprocessor));
    }

    #[test]
    fn source_location_queries_work() {
        let context = SemanticFileContext {
            declarations: vec![crate::parser::file_context::SemanticDeclaration {
                stable_id: "usr:c:@F@foo#".to_string(),
                provenance: SemanticIdProvenance::Usr,
                name: "foo".to_string(),
                kind: ClangSymbolKind::Function,
                line: 3,
                column: 5,
                usr: Some("c:@F@foo#".to_string()),
                scope_usr: None,
            }],
            references: vec![crate::parser::file_context::SemanticReference {
                stable_id: "usr:c:@F@foo#".to_string(),
                provenance: SemanticIdProvenance::Usr,
                decl_path: "query.cpp".to_string(),
                decl_kind: ClangSymbolKind::Function,
                offset: 12,
                line: 6,
                column: 9,
            }],
            scopes: vec![crate::parser::file_context::SemanticScope {
                kind: SemanticScopeKind::Preprocessor,
                node_kind: node_kind::PREPROC_IF,
                start_offset: 0,
                end_offset: 20,
                start_line: 1,
                end_line: 2,
            }],
            ..SemanticFileContext::default()
        };
        let declaration = context
            .declaration_at_location(SourceLocation::new(3, 5), &[ClangSymbolKind::Function])
            .expect("declaration by location");
        assert_eq!(declaration.name, "foo");
        assert_eq!(context.references_for_stable_id("usr:c:@F@foo#").len(), 1);
        assert!(context
            .scope_at_location(SourceLocation::new(1, 1))
            .is_some());
        assert!(context.is_macro_region(SourceLocation::new(2, 1)));
        assert!(!context.is_macro_region(SourceLocation::new(10, 1)));
    }

    #[test]
    fn builds_deterministic_regions_for_file_context() {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("cpp language");
        let text = "#if 1\nint value = 0;\n#endif\n";
        let tree = parser.parse(text, None).expect("parse tree");
        let path = Path::new("regions.cpp");
        let first = SemanticFileContext::from_parses(text, path, Some(&tree), None);
        let second = SemanticFileContext::from_parses(text, path, Some(&tree), None);
        assert!(!first.regions.is_empty());
        assert_eq!(first.regions, second.regions);
        assert_eq!(
            first.summary().semantic_signature,
            second.summary().semantic_signature
        );
    }

    #[test]
    fn region_lookup_returns_smallest_covering_region() {
        let context = SemanticFileContext {
            regions: vec![
                SemanticRegion::new(
                    "demo.cpp",
                    SemanticRegionKind::File,
                    1,
                    20,
                    0,
                    200,
                    None,
                    false,
                ),
                SemanticRegion::new(
                    "demo.cpp",
                    SemanticRegionKind::Function,
                    5,
                    10,
                    40,
                    120,
                    None,
                    false,
                ),
                SemanticRegion::new(
                    "demo.cpp",
                    SemanticRegionKind::Declaration,
                    7,
                    7,
                    70,
                    90,
                    Some("usr:c:@F@demo#".to_string()),
                    false,
                ),
            ],
            ..SemanticFileContext::default()
        };
        let region = context
            .region_at_location(SourceLocation::new(7, 1))
            .expect("region");
        assert_eq!(region.kind, SemanticRegionKind::Declaration);
    }

    #[test]
    fn stable_id_for_symbol_is_column_resilient() {
        let path = PathBuf::from("column_resilience.cpp");
        let symbol_col5 = ClangSymbol {
            name: "value".to_string(),
            kind: ClangSymbolKind::Variable,
            line: 10,
            column: 5,
            usr: None,
            scope_usr: None,
        };
        let symbol_col8 = ClangSymbol {
            name: "value".to_string(),
            kind: ClangSymbolKind::Variable,
            line: 10,
            column: 8,
            usr: None,
            scope_usr: None,
        };
        let (id_a, _) = SemanticFileContext::stable_id_for_symbol(&path, &symbol_col5);
        let (id_b, _) = SemanticFileContext::stable_id_for_symbol(&path, &symbol_col8);
        assert_eq!(id_a, id_b, "same name+line+kind at different columns must produce same stable_id");
    }

    #[test]
    fn stable_id_for_symbol_differs_by_name() {
        let path = PathBuf::from("name_diff.cpp");
        let sym_a = ClangSymbol {
            name: "alpha".to_string(),
            kind: ClangSymbolKind::Variable,
            line: 10,
            column: 5,
            usr: None,
            scope_usr: None,
        };
        let sym_b = ClangSymbol {
            name: "beta".to_string(),
            kind: ClangSymbolKind::Variable,
            line: 10,
            column: 5,
            usr: None,
            scope_usr: None,
        };
        let (id_a, _) = SemanticFileContext::stable_id_for_symbol(&path, &sym_a);
        let (id_b, _) = SemanticFileContext::stable_id_for_symbol(&path, &sym_b);
        assert_ne!(id_a, id_b, "different names must produce different stable_ids");
    }
}
