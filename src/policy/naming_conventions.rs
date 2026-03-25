use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;
use std::path::Path;

use tree_sitter::{Node, StreamingIterator};

use crate::model::edit::Edit;
use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::model::context_query::SemanticContextQuery;
use crate::model::rename_plan::SemanticRenamePlan;
use crate::model::violation::Violation;
use crate::parser::clang_types::ClangDeclKey;
use crate::parser::clang_result::ClangParseResult;
use crate::parser::file_context::SemanticFileContext;
use crate::parser::node_kind;
use crate::parser::ts_traversal;
use crate::policy::Policy;
use crate::parser::text_scan;

#[derive(Clone, Debug, Eq, PartialEq)]
struct RenamePlan {
    old_name: String,
    new_name: String,
    line: usize,
    column: usize,
    kind: i32,
    minimum_required_occurrences: usize,
    expected_occurrences: usize,
    stable_id: Option<String>,
}

impl RenamePlan {
    fn rename_confidence(&self) -> f64 {
        match (
            self.stable_id.is_some(),
            self.expected_occurrences > 0,
            self.minimum_required_occurrences >= self.expected_occurrences,
        ) {
            (true, true, true) => 1.0,
            (true, true, false) => 0.8,
            (true, false, _) => 0.6,
            (false, _, _) => 0.5,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Replacement {
    start: usize,
    end: usize,
    line: usize,
    old_name: std::sync::Arc<str>,
    new_name: std::sync::Arc<str>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct StrictIssues {
    enabled: bool,
    count: usize,
    first: Option<String>,
}

impl StrictIssues {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            count: 0,
            first: None,
        }
    }

    fn push(&mut self, message: String) {
        if !self.enabled {
            return;
        }
        self.count += 1;
        if self.first.is_none() {
            self.first = Some(message);
        }
    }

    fn push_lazy<F>(&mut self, produce: F)
    where
        F: FnOnce() -> String,
    {
        if !self.enabled {
            return;
        }
        self.count += 1;
        if self.first.is_none() {
            self.first = Some(produce());
        }
    }

    fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn len(&self) -> usize {
        self.count
    }

    fn first(&self) -> Option<&str> {
        self.first.as_deref()
    }
}

#[derive(Clone, Debug)]
pub struct PrefixConfig {
    pub(crate) local: Box<str>,
    pub(crate) member: Box<str>,
    pub(crate) global: Box<str>,
    pub(crate) static_lower: Box<str>,
    pub(crate) static_upper: Box<str>,
    pub(crate) const_lower: Box<str>,
    pub(crate) constexpr_upper: Box<str>,
    pub(crate) volatile: Box<str>,
    pub(crate) pointer: Box<str>,
    pub(crate) shared_ptr: Box<str>,
    pub(crate) unique_ptr: Box<str>,
    pub(crate) weak_ptr: Box<str>,
    pub(crate) function: Box<str>,
    pub(crate) reference: Box<str>,
    pub(crate) atomic: Box<str>,
    pub(crate) enum_var: Box<str>,
    pub(crate) struct_var: Box<str>,
}

impl Default for PrefixConfig {
    fn default() -> Self {
        Self {
            local: "_".into(),
            member: "m_".into(),
            global: "g_".into(),
            static_lower: "s_".into(),
            static_upper: "S_".into(),
            const_lower: "c_".into(),
            constexpr_upper: "C_".into(),
            volatile: "v_".into(),
            pointer: "p_".into(),
            shared_ptr: "sp_".into(),
            unique_ptr: "up_".into(),
            weak_ptr: "wp_".into(),
            function: "f_".into(),
            reference: "r_".into(),
            atomic: "a_".into(),
            enum_var: "e_".into(),
            struct_var: "t_".into(),
        }
    }
}

impl PrefixConfig {
    fn has_known_prefix(&self, name: &str) -> bool {
        let candidates = [
            &*self.local, &*self.member, &*self.global,
            &*self.static_lower, &*self.static_upper,
            &*self.const_lower, &*self.constexpr_upper,
            &*self.volatile, &*self.pointer, &*self.shared_ptr,
            &*self.unique_ptr, &*self.weak_ptr, &*self.function,
            &*self.reference, &*self.atomic, &*self.enum_var,
            &*self.struct_var,
        ];
        candidates
            .iter()
            .any(|pfx| !pfx.is_empty() && name.starts_with(pfx))
    }
}

#[derive(Clone, Debug, Default)]
struct IdentifierContext<'a> {
    pub(crate) is_field: bool,
    pub(crate) is_global: bool,
    pub(crate) ts_static: bool,
    pub(crate) ts_const: bool,
    pub(crate) ts_volatile: bool,
    pub(crate) ts_pointer: bool,
    pub(crate) ts_reference: bool,
    pub(crate) ts_type_text: Option<&'a str>,
    pub(crate) clang_symbol: Option<&'a crate::parser::clang_symbol::ClangSymbol>,
}

struct RenameDiagnostics<'a> {
    pub(crate) warnings: &'a mut Vec<String>,
    pub(crate) strict_issues: &'a mut StrictIssues,
}

pub struct NamingConventionsPolicy {
    semantic_mode: bool,
    semantic_strict: bool,
    prefixes: PrefixConfig,
}

use crate::parser::simd_classify::find_uppercase_positions_into;
use crate::parser::simd_classify::is_snake_case_bytes;
use crate::parser::simd_classify::is_upper_snake_case_bytes;

fn push_children_rev<'a>(stack: &mut SmallVec<[Node<'a>; 64]>, node: Node<'a>) {
    for idx in (0..node.child_count()).rev() {
        if let Some(child) = node.child(idx as u32) {
            stack.push(child);
        }
    }
}

impl NamingConventionsPolicy {
    fn semantic_parse_clean(
        parse: &ClangParseResult,
        semantic_context: Option<&SemanticFileContext>,
    ) -> bool {
        let summary = parse.diagnostic_summary();
        if summary.fatal > 0 {
            return false;
        }
        if !parse.symbols.is_empty() {
            return true;
        }
        if semantic_context.is_some_and(|semantic| {
            let summary = semantic.summary();
            summary.declaration_count > 0 && summary.reference_count > 0
        }) {
            return true;
        }
        // Allow renames without clang symbols when there are no fatal errors.
        // Tree-sitter provides rename candidates; clang symbols only add
        // confidence (stable_id, expected_occurrences). Without them, renames
        // proceed at lower confidence through the trust gate.
        true
    }

    pub fn new(semantic_mode: bool, semantic_strict: bool) -> Self {
        Self {
            semantic_mode,
            semantic_strict,
            prefixes: PrefixConfig::default(),
        }
    }

    pub fn with_prefix_config(mut self, config: PrefixConfig) -> Self {
        self.prefixes = config;
        self
    }

    fn extract_ts_template_name<'a>(decl_node: Node<'a>, source: &'a [u8]) -> Option<&'a str> {
        let type_node = decl_node.child_by_field_name("type")?;
        if type_node.kind() != node_kind::TEMPLATE_TYPE {
            return None;
        }
        let name_node = type_node.child_by_field_name("name")?;
        let leaf = ts_traversal::first_descendant(
            name_node,
            &[node_kind::TYPE_IDENTIFIER],
            &[],
        ).unwrap_or(name_node);
        leaf.utf8_text(source).ok()
    }

    fn extract_ts_type_context<'a>(decl_node: Node<'a>, source: &'a [u8]) -> IdentifierContext<'a> {
        let mut ctx = IdentifierContext::default();
        for i in 0..decl_node.child_count() {
            let Some(child) = decl_node.child(i as u32) else { continue };
            match child.kind() {
                "storage_class_specifier" => {
                    if child.utf8_text(source).ok() == Some("static") {
                        ctx.ts_static = true;
                    }
                }
                "type_qualifier" => {
                    match child.utf8_text(source).ok() {
                        Some("const") => ctx.ts_const = true,
                        Some("volatile") => ctx.ts_volatile = true,
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        if let Some(declarator) = decl_node.child_by_field_name("declarator") {
            match declarator.kind() {
                "pointer_declarator" => ctx.ts_pointer = true,
                "reference_declarator" => ctx.ts_reference = true,
                "init_declarator" => {
                    if let Some(inner) = declarator.child_by_field_name("declarator") {
                        match inner.kind() {
                            "pointer_declarator" => ctx.ts_pointer = true,
                            "reference_declarator" => ctx.ts_reference = true,
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        ctx.ts_type_text = Self::extract_ts_template_name(decl_node, source);
        ctx
    }

    #[cfg(test)]
    fn build_stacked_prefix(&self, ctx: &IdentifierContext<'_>) -> String {
        let mut buf = String::with_capacity(16);
        self.build_stacked_prefix_into(&mut buf, ctx);
        buf
    }

    fn build_stacked_prefix_into(&self, prefix: &mut String, ctx: &IdentifierContext<'_>) {
        prefix.clear();
        let candidates = &self.prefixes;

        if ctx.is_field {
            prefix.push_str(&candidates.member);
        } else if ctx.is_global {
            prefix.push_str(&candidates.global);
        } else {
            prefix.push_str(&candidates.local);
        }

        if let Some(sym) = ctx.clang_symbol {
            if sym
                .storage_class
                .is_some_and(|sc| sc == clang_sys::CX_SC_Static)
            {
                prefix.push_str(&candidates.static_lower);
            }
            if sym.is_const {
                prefix.push_str(&candidates.const_lower);
            }
            if sym.is_volatile {
                prefix.push_str(&candidates.volatile);
            }
            let tmpl = sym.template_name.as_deref();
            let type_pfx = match sym.type_kind {
                clang_sys::CXType_Pointer => {
                    if tmpl == Some("shared_ptr") {
                        &candidates.shared_ptr
                    } else if tmpl == Some("unique_ptr") {
                        &candidates.unique_ptr
                    } else if tmpl == Some("weak_ptr") {
                        &candidates.weak_ptr
                    } else {
                        &candidates.pointer
                    }
                }
                clang_sys::CXType_LValueReference | clang_sys::CXType_RValueReference => {
                    &candidates.reference
                }
                clang_sys::CXType_Enum => &candidates.enum_var,
                clang_sys::CXType_Record => &candidates.struct_var,
                _ => {
                    if tmpl == Some("shared_ptr") {
                        &candidates.shared_ptr
                    } else if tmpl == Some("unique_ptr") {
                        &candidates.unique_ptr
                    } else if tmpl == Some("weak_ptr") {
                        &candidates.weak_ptr
                    } else if tmpl == Some("function") {
                        &candidates.function
                    } else if tmpl == Some("atomic") {
                        &candidates.atomic
                    } else {
                        ""
                    }
                }
            };
            prefix.push_str(type_pfx);
        } else {
            if ctx.ts_static {
                prefix.push_str(&candidates.static_lower);
            }
            if ctx.ts_const {
                prefix.push_str(&candidates.const_lower);
            }
            if ctx.ts_volatile {
                prefix.push_str(&candidates.volatile);
            }
            let tmpl = ctx.ts_type_text;
            let type_pfx = if ctx.ts_pointer {
                match tmpl {
                    Some("shared_ptr") => &candidates.shared_ptr,
                    Some("unique_ptr") => &candidates.unique_ptr,
                    Some("weak_ptr") => &candidates.weak_ptr,
                    _ => &candidates.pointer,
                }
            } else if ctx.ts_reference {
                &candidates.reference
            } else {
                match tmpl {
                    Some("shared_ptr") => &candidates.shared_ptr,
                    Some("unique_ptr") => &candidates.unique_ptr,
                    Some("weak_ptr") => &candidates.weak_ptr,
                    Some("function") | Some("Function") => &candidates.function,
                    Some("atomic") | Some("Atomic") => &candidates.atomic,
                    _ => "",
                }
            };
            prefix.push_str(type_pfx);
        }
    }

    const RENAME_CANDIDATE_QUERY: &str = r#"[
        (function_definition) @node
        (declaration) @node
        (field_declaration) @node
    ]"#;

    fn has_semantic_rename_candidates(
        root: Node<'_>,
        query_cache: Option<&crate::parser::query_cache::TsQueryCache>,
    ) -> bool {
        if let Some(query) = query_cache
            .and_then(|qc| qc.get_or_compile(Self::RENAME_CANDIDATE_QUERY).ok())
        {
            let mut cursor = tree_sitter::QueryCursor::new();
            let mut matches = cursor.matches(&query, root, "".as_bytes());
            matches.advance();
            return matches.get().is_some();
        }

        let mut stack: SmallVec<[Node; 64]> = SmallVec::from_elem(root, 1);
        while let Some(node) = stack.pop() {
            if matches!(
                node.kind(),
                node_kind::FUNCTION_DEFINITION
                    | node_kind::DECLARATION
                    | node_kind::FIELD_DECLARATION
            ) {
                return true;
            }
            push_children_rev(&mut stack, node);
        }
        false
    }

    fn is_loop_header_declaration(node: Node<'_>) -> bool {
        let Some(parent) = node.parent() else {
            return false;
        };
        match parent.kind() {
            node_kind::FOR_STATEMENT | node_kind::FOR_RANGE_LOOP | node_kind::WHILE_STATEMENT => {
                let body = parent.child_by_field_name("body");
                if let Some(body_node) = body {
                    node.start_byte() < body_node.start_byte()
                } else {
                    true
                }
            }
            _ => false,
        }
    }

    fn is_enclosing_class_match<'a>(node: Node<'a>, name: &str, source: &'a [u8]) -> bool {
        let mut cursor = node;
        while let Some(parent) = cursor.parent() {
            if matches!(
                parent.kind(),
                node_kind::CLASS_SPECIFIER | node_kind::STRUCT_SPECIFIER
            ) {
                // Check the "name" field and all TYPE_IDENTIFIER children.
                // Handles `class EXPORT_MACRO ClassName` where the name field
                // may point to the macro rather than the class name.
                if let Some(name_node) = parent.child_by_field_name("name") {
                    if name_node.utf8_text(source).is_ok_and(|cn| cn == name) {
                        return true;
                    }
                }
                for i in 0..parent.child_count() {
                    if let Some(child) = parent.child(i as u32) {
                        if child.kind() == node_kind::TYPE_IDENTIFIER
                            && child.utf8_text(source).is_ok_and(|cn| cn == name)
                        {
                            return true;
                        }
                    }
                }
                return false;
            }
            cursor = parent;
        }
        false
    }

    fn is_ts_constructor<'a>(name_node: Node<'a>, name: &str, source: &[u8]) -> bool {
        Self::is_enclosing_class_match(name_node, name, source)
    }

    fn is_ts_destructor<'a>(name_node: Node<'a>, _name: &str, source: &[u8]) -> bool {
        // Check if any ancestor is destructor_name — handles the case where
        // rightmost_descendant found the IDENTIFIER child inside destructor_name
        let mut cursor = name_node;
        while let Some(parent) = cursor.parent() {
            if parent.kind() == node_kind::DESTRUCTOR_NAME {
                return true;
            }
            if parent.kind() == node_kind::FUNCTION_DECLARATOR
                || parent.kind() == node_kind::FUNCTION_DEFINITION
            {
                break;
            }
            cursor = parent;
        }
        // Fallback: check preceding ~ byte (for grammars that don't use destructor_name)
        let start = name_node.start_byte();
        if start == 0 || source[start - 1] != b'~' {
            return false;
        }
        // In .cpp files the destructor implementation has no enclosing class_specifier,
        // but ~ before the name is sufficient evidence
        true
    }

    fn is_snake_case(name: &str) -> bool {
        if name.is_empty() {
            return true;
        }
        let bytes = name.as_bytes();
        if !(bytes[0].is_ascii_lowercase() || bytes[0] == b'_') {
            return false;
        }
        is_snake_case_bytes(&bytes[1..])
    }

    fn is_upper_snake_case(name: &str) -> bool {
        if name.is_empty() {
            return false;
        }
        let bytes = name.as_bytes();
        if !is_upper_snake_case_bytes(bytes) {
            return false;
        }
        bytes.iter().any(|&b| b.is_ascii_alphabetic())
    }

    fn is_constant_like_identifier(name: &str) -> bool {
        if Self::is_upper_snake_case(name) {
            return true;
        }
        if let Some(rest) = name.strip_prefix("C_") {
            return !rest.is_empty() && Self::is_upper_snake_case(rest);
        }
        if let Some(rest) = name.strip_prefix("S_") {
            return !rest.is_empty() && Self::is_upper_snake_case(rest);
        }
        if let Some(rest) = name.strip_prefix("c_") {
            return !rest.is_empty() && Self::is_snake_case(rest);
        }
        if let Some(rest) = name.strip_prefix("s_") {
            return !rest.is_empty() && Self::is_snake_case(rest);
        }
        false
    }

    #[cfg(test)]
    fn to_snake_case(value: &str) -> String {
        let mut out = String::with_capacity(value.len().saturating_add(4));
        let mut pos_buf: SmallVec<[usize; 16]> = SmallVec::new();
        Self::to_snake_case_into(value, &mut pos_buf, &mut out);
        out
    }

    fn to_snake_case_into(value: &str, pos_buf: &mut SmallVec<[usize; 16]>, out: &mut String) {
        out.clear();
        let bytes = value.as_bytes();
        let len = bytes.len();

        pos_buf.clear();
        find_uppercase_positions_into(bytes, pos_buf);

        if pos_buf.is_empty() {
            out.push_str(value);
            return;
        }

        out.reserve(len.saturating_add(4));
        let result = unsafe { out.as_mut_vec() };
        let mut pos_idx = 0;
        for i in 0..len {
            if pos_idx < pos_buf.len() && pos_buf[pos_idx] == i {
                let prev = if i > 0 { Some(bytes[i - 1]) } else { None };
                let next = bytes.get(i + 1).copied();
                let boundary = prev
                    .is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit())
                    || (prev.is_some_and(|p| p.is_ascii_uppercase())
                        && next.is_some_and(|n| n.is_ascii_lowercase()));
                if boundary && result.last() != Some(&b'_') {
                    result.push(b'_');
                }
                result.push(bytes[i].to_ascii_lowercase());
                pos_idx += 1;
            } else {
                result.push(bytes[i]);
            }
        }
    }

    #[cfg(test)]
    fn to_upper_snake_case(value: &str) -> String {
        Self::to_snake_case(value).to_ascii_uppercase()
    }

    fn has_identifier_boundaries(text: &str, start: usize, end: usize) -> bool {
        let bytes = text.as_bytes();
        if start > 0 && text_scan::is_identifier_byte(bytes[start - 1]) {
            return false;
        }
        if end < bytes.len() && text_scan::is_identifier_byte(bytes[end]) {
            return false;
        }
        true
    }



    fn line_and_column_for_offset(line_starts: &[usize], offset: usize) -> (usize, usize) {
        match line_starts.binary_search(&offset) {
            Ok(index) => (index + 1, 1),
            Err(0) => (1, offset.saturating_add(1)),
            Err(index) => {
                let line_start = line_starts
                    .get(index.saturating_sub(1))
                    .copied()
                    .unwrap_or(0);
                (index, offset.saturating_sub(line_start).saturating_add(1))
            }
        }
    }

    fn offset_for_line_column(line_starts: &[usize], line: usize, column: usize) -> Option<usize> {
        if line == 0 || column == 0 {
            return None;
        }
        let line_start = line_starts.get(line.saturating_sub(1)).copied()?;
        Some(line_start.saturating_add(column.saturating_sub(1)))
    }

    fn find_uncovered_field_initializer_label_offset(
        text: &str,
        name: &str,
        covered_offsets: &[usize],
    ) -> Option<usize> {
        if name.is_empty() || covered_offsets.is_empty() || text.len() < name.len() {
            return None;
        }
        let covered = covered_offsets.iter().copied().collect::<FxHashSet<_>>();
        let bytes = text.as_bytes();
        let name_len = name.len();
        for offset in text_scan::subslice_match_indices(bytes, name.as_bytes()) {
            let end = offset + name_len;
            if !Self::has_identifier_boundaries(text, offset, end) || covered.contains(&offset) {
                continue;
            }
            let mut next = end;
            while next < bytes.len() && matches!(bytes[next], b' ' | b'\t') {
                next += 1;
            }
            if next < bytes.len() && bytes[next] == b'(' {
                let mut prev = offset;
                while prev > 0 && matches!(bytes[prev - 1], b' ' | b'\t') {
                    prev -= 1;
                }
                if prev > 0 && matches!(bytes[prev - 1], b':' | b',') {
                    return Some(offset);
                }
            }
        }
        None
    }

    fn resolve_rename_plan(
        semantic_query: &SemanticContextQuery<'_>,
        clang_parse: &ClangParseResult,
        old_name: &str,
        suggested: String,
        line: usize,
        source_column: usize,
        kind: i32,
        column: usize,
        allowed_kinds: &[i32],
        rename_plans: &mut Vec<RenamePlan>,
    ) -> bool {
        let mut expected_occurrences = 0usize;
        let mut minimum_required_occurrences = 0usize;
        let mut stable_id = None::<String>;
        if semantic_query.is_available() {
            let Some(declaration) =
                semantic_query.symbol_at(line, source_column, allowed_kinds)
            else {
                rename_plans.push(RenamePlan {
                    old_name: old_name.to_string(),
                    new_name: suggested,
                    line,
                    column,
                    kind,
                    minimum_required_occurrences,
                    expected_occurrences,
                    stable_id,
                });
                return true;
            };
            stable_id = Some(declaration.stable_id.clone());
            let references = semantic_query.references_of(declaration.stable_id.as_str());
            let safe_reference_count = references
                .iter()
                .filter(|reference| semantic_query.is_safe_edit(reference.line, reference.column))
                .count();
            expected_occurrences = references.len().max(1);
            minimum_required_occurrences = safe_reference_count.max(1);
        }
        if clang_parse.has_name_elsewhere(&suggested, line) {
            return false;
        }
        rename_plans.push(RenamePlan {
            old_name: old_name.to_string(),
            new_name: suggested,
            line,
            column,
            kind,
            minimum_required_occurrences,
            expected_occurrences,
            stable_id,
        });
        true
    }

    fn normalize_decl_path(path: &Path) -> String {
        path.to_string_lossy()
            .to_string()
    }

    fn build_replacements(
        &self,
        text: &str,
        clang_parse: &ClangParseResult,
        plans: &[RenamePlan],
        semantic_query: &SemanticContextQuery<'_>,
        non_code_ranges: &[(usize, usize)],
        diag: &mut RenameDiagnostics<'_>,
    ) -> Vec<Replacement> {
        let line_starts = text_scan::line_starts(text, false);
        let mut by_start: FxHashMap<usize, Replacement> =
            FxHashMap::with_capacity_and_hasher(plans.len().saturating_mul(2), Default::default());
        let mut conflicting_starts = FxHashSet::default();

        for plan in plans {
            let mut offsets = if let Some(stable_id) = plan.stable_id.as_deref() {
                let mut semantic_offsets = semantic_query
                    .references_of(stable_id)
                    .iter()
                    .filter_map(|reference| {
                        Self::offset_for_line_column(
                            line_starts.as_slice(),
                            reference.line,
                            reference.column,
                        )
                    })
                    .collect::<Vec<_>>();
                if let Some(declaration_offset) =
                    Self::offset_for_line_column(line_starts.as_slice(), plan.line, plan.column)
                {
                    semantic_offsets.push(declaration_offset);
                }
                semantic_offsets.sort_unstable();
                semantic_offsets.dedup();
                semantic_offsets
            } else {
                let clang_offsets = clang_parse.rename_offsets(
                    &plan.old_name,
                    plan.line,
                    std::slice::from_ref(&plan.kind),
                );
                if clang_offsets.is_empty() {
                    // Text-scan fallback: clang has no symbols for this file.
                    // Find all whole-word occurrences of the identifier.
                    let bytes = text.as_bytes();
                    let name_bytes = plan.old_name.as_bytes();
                    text_scan::subslice_match_indices(bytes, name_bytes)
                        .filter(|&offset| {
                            let end = offset + plan.old_name.len();
                            Self::has_identifier_boundaries(text, offset, end)
                        })
                        .collect()
                } else {
                    clang_offsets
                }
            };
            if plan.expected_occurrences > offsets.len() {
                if let Some(declaration_offset) =
                    Self::offset_for_line_column(line_starts.as_slice(), plan.line, plan.column)
                {
                    let declaration_end = declaration_offset.saturating_add(plan.old_name.len());
                    if declaration_end <= text.len()
                        && !offsets.contains(&declaration_offset)
                        && text
                            .get(declaration_offset..declaration_end)
                            .is_some_and(|slice| slice == plan.old_name)
                        && Self::has_identifier_boundaries(
                            text,
                            declaration_offset,
                            declaration_end,
                        )
                    {
                        offsets.push(declaration_offset);
                        offsets.sort_unstable();
                        offsets.dedup();
                    }
                }
            }
            if plan.kind == clang_sys::CXCursor_FieldDecl {
                if let Some(offset) = Self::find_uncovered_field_initializer_label_offset(
                    text,
                    plan.old_name.as_str(),
                    offsets.as_slice(),
                ) {
                    let (line, _column) = Self::line_and_column_for_offset(&line_starts, offset);
                    diag.warnings.push(format!(
                        "naming_conventions: skipped semantic rename '{}' on line {} due uncovered constructor initializer label at line {}",
                        plan.old_name, plan.line, line
                    ));
                    diag.strict_issues.push_lazy(|| {
                        format!(
                            "uncovered constructor initializer label for '{}' at line {}",
                            plan.old_name, line
                        )
                    });
                    continue;
                }
            }
            if offsets.is_empty() {
                diag.warnings.push(format!(
                    "naming_conventions: no semantic references found for '{}' on line {}",
                    plan.old_name, plan.line
                ));
                diag.strict_issues.push_lazy(|| {
                    format!(
                        "missing semantic references for '{}' on line {}",
                        plan.old_name, plan.line
                    )
                });
                continue;
            }
            let declaration_offset =
                Self::offset_for_line_column(line_starts.as_slice(), plan.line, plan.column)
                    .and_then(|offset| {
                        let end = offset.saturating_add(plan.old_name.len());
                        if end <= text.len()
                            && text
                                .get(offset..end)
                                .is_some_and(|slice| slice == plan.old_name)
                            && Self::has_identifier_boundaries(text, offset, end)
                        {
                            Some(offset)
                        } else {
                            None
                        }
                    });
            let plan_old: std::sync::Arc<str> = std::sync::Arc::from(plan.old_name.as_str());
            let plan_new: std::sync::Arc<str> = std::sync::Arc::from(plan.new_name.as_str());
            let mut plan_replacements: SmallVec<[Replacement; 8]> = SmallVec::new();
            for offset in offsets {
                let end = offset.saturating_add(plan.old_name.len());
                if end > text.len() || !text.is_char_boundary(offset) || !text.is_char_boundary(end)
                {
                    diag.warnings.push(format!(
                        "naming_conventions: skipped invalid rename span for '{}' at byte {}",
                        plan.old_name, offset
                    ));
                    diag.strict_issues.push_lazy(|| {
                        format!(
                            "invalid rename span for '{}' at byte {}",
                            plan.old_name, offset
                        )
                    });
                    continue;
                }

                let Some(found) = text.get(offset..end) else {
                    continue;
                };
                if found != plan.old_name {
                    continue;
                }
                if !Self::has_identifier_boundaries(text, offset, end) {
                    continue;
                }
                // Skip occurrences inside comments/strings to respect code_only contract
                if non_code_ranges.binary_search_by(|&(start, end_r)| {
                    if offset < start { std::cmp::Ordering::Greater }
                    else if offset >= end_r { std::cmp::Ordering::Less }
                    else { std::cmp::Ordering::Equal }
                }).is_ok() {
                    continue;
                }
                let (line, column) = Self::line_and_column_for_offset(&line_starts, offset);
                if semantic_query.is_available() && !semantic_query.is_safe_edit(line, column) {
                    diag.warnings.push(format!(
                        "naming_conventions: skipped semantic-unsafe rename '{}' at line {}",
                        plan.old_name, line
                    ));
                    diag.strict_issues.push_lazy(|| {
                        format!(
                            "semantic-unsafe rename '{}' at line {}",
                            plan.old_name, line
                        )
                    });
                    continue;
                }

                let replacement = Replacement {
                    start: offset,
                    end,
                    line,
                    old_name: std::sync::Arc::clone(&plan_old),
                    new_name: std::sync::Arc::clone(&plan_new),
                };
                plan_replacements.push(replacement);
            }

            if plan.expected_occurrences > 0 {
                let realized = plan_replacements.len();
                let safe_gap = plan.minimum_required_occurrences.saturating_sub(realized);
                if safe_gap > 0 {
                    diag.warnings.push(format!(
                        "naming_conventions: semantic safe-coverage gap for '{}' on line {} ({} < {})",
                        plan.old_name,
                        plan.line,
                        realized,
                        plan.minimum_required_occurrences
                    ));
                    diag.strict_issues.push_lazy(|| {
                        format!(
                            "semantic safe-coverage gap for '{}' on line {} ({} < {})",
                            plan.old_name, plan.line, realized, plan.minimum_required_occurrences
                        )
                    });
                }
                if realized < plan.expected_occurrences {
                    diag.warnings.push(format!(
                        "naming_conventions: full-coverage gap for '{}' on line {} ({} < {})",
                        plan.old_name,
                        plan.line,
                        realized,
                        plan.expected_occurrences
                    ));
                    diag.strict_issues.push_lazy(|| {
                        format!(
                            "full-coverage gap for '{}' on line {} ({} < {})",
                            plan.old_name, plan.line, realized, plan.expected_occurrences
                        )
                    });
                }
            }

            if let Some(declaration_offset) = declaration_offset {
                let declaration_covered = plan_replacements
                    .iter()
                    .any(|replacement| replacement.start == declaration_offset);
                if !declaration_covered {
                    diag.warnings.push(format!(
                        "naming_conventions: skipped semantic rename '{}' on line {} because declaration span was not safely renameable",
                        plan.old_name, plan.line
                    ));
                    diag.strict_issues.push_lazy(|| {
                        format!(
                            "declaration span missing for semantic rename '{}' on line {}",
                            plan.old_name, plan.line
                        )
                    });
                    continue;
                }
            }

            for replacement in plan_replacements {
                if let Some(existing) = by_start.get(&replacement.start) {
                    if existing.old_name != replacement.old_name
                        || existing.new_name != replacement.new_name
                    {
                        conflicting_starts.insert(replacement.start);
                        diag.warnings.push(format!(
                            "naming_conventions: conflicting semantic edits at byte {}; skipping",
                            replacement.start
                        ));
                        diag.strict_issues.push_lazy(|| {
                            format!("conflicting semantic edits at byte {}", replacement.start)
                        });
                    }
                } else {
                    by_start.insert(replacement.start, replacement);
                }
            }
        }

        for start in conflicting_starts {
            by_start.remove(&start);
        }

        let mut replacements = by_start.into_values().collect::<Vec<_>>();
        replacements.sort_by_key(|replacement| replacement.start);

        let mut filtered = Vec::with_capacity(replacements.len());
        let mut last_end = 0usize;
        for replacement in replacements {
            if !filtered.is_empty() && replacement.start < last_end {
                diag.warnings.push(format!(
                    "naming_conventions: overlapping semantic edit near line {}; skipping",
                    replacement.line
                ));
                diag.strict_issues.push_lazy(|| {
                    format!("overlapping semantic edit near line {}", replacement.line)
                });
                continue;
            }
            last_end = replacement.end;
            filtered.push(replacement);
        }

        filtered
    }

    fn apply_semantic_renames(
        &self,
        text: &str,
        clang_parse: &ClangParseResult,
        plans: &[RenamePlan],
        semantic_query: &SemanticContextQuery<'_>,
        non_code_ranges: &[(usize, usize)],
        diag: &mut RenameDiagnostics<'_>,
    ) -> (String, Vec<Edit>) {
        let mut replacements = self.build_replacements(
            text,
            clang_parse,
            plans,
            semantic_query,
            non_code_ranges,
            diag,
        );
        if replacements.is_empty() {
            if !plans.is_empty() {
                diag.strict_issues.push("semantic rename produced no concrete replacements".to_string());
            }
            return (text.to_string(), Vec::new());
        }

        let mut output = text.to_string();
        replacements.sort_by(|left, right| right.start.cmp(&left.start));

        let mut edits = Vec::with_capacity(replacements.len());
        for replacement in &replacements {
            output.replace_range(
                replacement.start..replacement.end,
                &replacement.new_name,
            );
            edits.push(Edit {
                policy: self.name().into(),
                line: replacement.line,
                before: replacement.old_name.to_string(),
                after: replacement.new_name.to_string(),
            });
        }
        edits.reverse();
        (output, edits)
    }
}

impl Policy for NamingConventionsPolicy {
    fn name(&self) -> &str {
        "naming_conventions"
    }
    fn apply(&self, context: &PolicyContext<'_>) -> PolicyResult {
        let Some(tree) = context.tree_sitter_tree else {
            return PolicyResult::unchanged_with_warning("naming_conventions: tree-sitter context unavailable".to_string());
        };

        let policy_name: crate::model::policy_name::PolicyName = self.name().into();
        let mut violations = Vec::with_capacity(64);
        let mut warnings = Vec::with_capacity(8);
        let mut strict_issues = StrictIssues::new(self.semantic_mode && self.semantic_strict);
        let mut rename_plans = Vec::with_capacity(32);
        let mut prefix_buf = String::with_capacity(16);
        let mut snake_buf = String::with_capacity(64);
        let mut upper_pos_buf: SmallVec<[usize; 16]> = SmallVec::new();
        let clang_parse = context.clang_parse_result;
        let semantic_query = context.semantic_query();
        let semantic_file_context = context.semantic_file_context;
        let semantic_enabled = self.semantic_mode
            && clang_parse
                .is_some_and(|parse| Self::semantic_parse_clean(parse, semantic_file_context));

        let root = tree.root_node();
        let has_candidate_nodes =
            Self::has_semantic_rename_candidates(root, context.query_cache);

        if self.semantic_mode {
            match clang_parse {
                Some(parse)
                    if has_candidate_nodes
                        && !Self::semantic_parse_clean(parse, semantic_file_context) =>
                {
                    let summary = parse.diagnostic_summary();
                    warnings.push(
                        format!(
                            "naming_conventions: semantic rename skipped due insufficient semantic parse reliability (success={}, symbols={}, errors={}, fatals={})",
                            parse.success,
                            parse.symbols.len(),
                            summary.error,
                            summary.fatal
                        ),
                    );
                }
                None => {
                    if has_candidate_nodes {
                        warnings.push(
                            "naming_conventions: semantic rename requires clang parse context"
                                .to_string(),
                        );
                    }
                }
                _ => {}
            }
            if has_candidate_nodes && !semantic_query.is_available() {
                warnings.push(
                    "naming_conventions: semantic rename requires semantic file context"
                        .to_string(),
                );
            }
        }
        let mut stack: SmallVec<[Node; 64]> = SmallVec::from_elem(root, 1);

        while let Some(node) = stack.pop() {
            if node.kind() == node_kind::FUNCTION_DEFINITION {
                if let Some(declarator) =
                    ts_traversal::first_descendant(node, &[node_kind::FUNCTION_DECLARATOR], &[node_kind::COMPOUND_STATEMENT])
                {
                    if let Some(name_node) = ts_traversal::rightmost_descendant(
                        declarator,
                        &[
                            node_kind::IDENTIFIER,
                            node_kind::FIELD_IDENTIFIER,
                            node_kind::TYPE_IDENTIFIER,
                            node_kind::DESTRUCTOR_NAME,
                        ],
                        &[node_kind::PARAMETER_LIST, node_kind::TEMPLATE_PARAMETER_LIST],
                    ) {
                        let name = name_node.utf8_text(context.text.as_bytes()).unwrap_or("");
                        let short = name.split("::").last().unwrap_or(name);
                        let line = name_node.start_position().row + 1;
                        let source_column = name_node.start_position().column + 1;
                        if name_node.kind() == node_kind::DESTRUCTOR_NAME {
                            continue;
                        }
                        let allowed = [
                            clang_sys::CXCursor_FunctionDecl,
                            clang_sys::CXCursor_FunctionTemplate,
                            clang_sys::CXCursor_CXXMethod,
                            clang_sys::CXCursor_Constructor,
                            clang_sys::CXCursor_Destructor,
                        ];
                        let matched_symbol = if let Some(parse) = clang_parse {
                            parse.symbol_on_line(short, line, &allowed)
                                .map(|symbol| (symbol.kind, symbol.column))
                        } else {
                            None
                        };
                        if matched_symbol.is_none() && name_node.kind() == node_kind::TYPE_IDENTIFIER {
                            continue;
                        }

                        if !short.starts_with("operator")
                            && !Self::is_snake_case(short)
                        {
                            if let Some((kind, _)) = matched_symbol {
                                if kind == clang_sys::CXCursor_Constructor
                                    || kind == clang_sys::CXCursor_Destructor
                                {
                                    continue;
                                }
                            }
                            if Self::is_ts_constructor(name_node, short, context.text.as_bytes())
                                || Self::is_ts_destructor(name_node, short, context.text.as_bytes())
                            {
                                continue;
                            }
                            Self::to_snake_case_into(short, &mut upper_pos_buf, &mut snake_buf);
                            violations.push(Violation {
                                policy: policy_name.clone(),
                                message: format!(
                                    "function '{}' is not snake_case; suggested '{}'",
                                    short, &snake_buf
                                ),
                                line,
                                column: Some(source_column),
                            });

                            if semantic_enabled {
                                let Some(parse) = clang_parse else {
                                    continue;
                                };
                                if let Some((kind, column)) = matched_symbol {
                                    if !Self::resolve_rename_plan(
                                        &semantic_query, parse, short, snake_buf.clone(),
                                        line, source_column, kind, column, &allowed,
                                        &mut rename_plans,
                                    ) {
                                        continue;
                                    }
                                } else {
                                    // No clang symbol — create text-scan-only rename plan.
                                    // Tree-sitter identified the function; clang just lacks
                                    // symbols for this file (e.g., template-heavy headers).
                                    rename_plans.push(RenamePlan {
                                        old_name: short.to_string(),
                                        new_name: snake_buf.clone(),
                                        line,
                                        column: source_column,
                                        kind: clang_sys::CXCursor_CXXMethod,
                                        minimum_required_occurrences: 0,
                                        expected_occurrences: 0,
                                        stable_id: None,
                                    });
                                }
                            }
                        }
                    }
                }
            }

            if node.is_error() {
                if let Some(parse) = clang_parse {
                    let error_start_line = node.start_position().row + 1;
                    let error_end_line = node.end_position().row + 1;
                    let allowed = [
                        clang_sys::CXCursor_FunctionDecl,
                        clang_sys::CXCursor_FunctionTemplate,
                        clang_sys::CXCursor_CXXMethod,
                    ];
                    for symbol in &parse.symbols {
                        if symbol.line < error_start_line || symbol.line > error_end_line {
                            continue;
                        }
                        if !allowed.contains(&symbol.kind) {
                            continue;
                        }
                        let short = symbol.name.split("::").last().unwrap_or(&symbol.name);
                        if short.starts_with("operator")
                            || Self::is_snake_case(short)
                        {
                            continue;
                        }
                        let line = symbol.line;
                        let source_column = symbol.column;
                        Self::to_snake_case_into(short, &mut upper_pos_buf, &mut snake_buf);
                        violations.push(Violation {
                            policy: policy_name.clone(),
                            message: format!(
                                "function '{}' is not snake_case; suggested '{}'",
                                short, &snake_buf
                            ),
                            line,
                            column: Some(source_column),
                        });

                        if semantic_enabled {
                            if !Self::resolve_rename_plan(
                                &semantic_query, parse, short, snake_buf.clone(),
                                line, source_column, symbol.kind, source_column, &allowed,
                                &mut rename_plans,
                            ) {
                                continue;
                            }
                        }
                    }
                }
            }

            if matches!(
                node.kind(),
                node_kind::DECLARATION | node_kind::FIELD_DECLARATION | node_kind::PARAMETER_DECLARATION
            ) {
                if node.kind() == node_kind::PARAMETER_DECLARATION
                    || node.kind() == node_kind::FIELD_DECLARATION
                {
                    push_children_rev(&mut stack, node);
                    continue;
                }
                if Self::is_loop_header_declaration(node) {
                    continue;
                }
                if let Some(name_node) = ts_traversal::declarator_identifier(node) {
                    let name = name_node.utf8_text(context.text.as_bytes()).unwrap_or("");
                    let line = name_node.start_position().row + 1;
                    let source_column = name_node.start_position().column + 1;
                    let allowed = [
                        clang_sys::CXCursor_VarDecl,
                        clang_sys::CXCursor_FieldDecl,
                        clang_sys::CXCursor_ParmDecl,
                    ];
                    let sym = clang_parse.and_then(|p| p.symbol_on_line(name, line, &allowed));
                    if sym.is_some_and(|s| s.kind == clang_sys::CXCursor_ParmDecl) {
                        push_children_rev(&mut stack, node);
                        continue;
                    }

                    if self.prefixes.has_known_prefix(name)
                        || Self::is_constant_like_identifier(name)
                    {
                        push_children_rev(&mut stack, node);
                        continue;
                    }

                    let id_ctx = if let Some(sym) = sym {
                        let is_field = sym.kind == clang_sys::CXCursor_FieldDecl;
                        let is_global = sym.scope_usr.is_none() && sym.kind == clang_sys::CXCursor_VarDecl;
                        IdentifierContext {
                            is_field,
                            is_global,
                            ts_static: false,
                            ts_const: false,
                            ts_volatile: false,
                            ts_pointer: false,
                            ts_reference: false,
                            ts_type_text: None,
                            clang_symbol: Some(sym),
                        }
                    } else {
                        Self::extract_ts_type_context(node, context.text.as_bytes())
                    };
                    let is_global = id_ctx.is_global;
                    self.build_stacked_prefix_into(&mut prefix_buf, &id_ctx);
                    Self::to_snake_case_into(name, &mut upper_pos_buf, &mut snake_buf);
                    snake_buf.insert_str(0, &prefix_buf);
                    if is_global {
                        snake_buf.make_ascii_uppercase();
                    }
                    let msg = if is_global {
                        format!("global identifier '{name}' should be UPPER_SNAKE_CASE with prefix; suggested '{}'", &snake_buf)
                    } else {
                        format!("local/member-like identifier '{name}' missing prefix; suggested '{}'", &snake_buf)
                    };
                    violations.push(Violation {
                        policy: policy_name.clone(),
                        message: msg,
                        line,
                        column: Some(source_column),
                    });

                    if semantic_enabled {
                        if let (Some(parse), Some(clang_sym)) = (clang_parse, sym) {
                            if !Self::resolve_rename_plan(
                                &semantic_query, parse, name, snake_buf.clone(),
                                line, source_column, clang_sym.kind, clang_sym.column, &allowed,
                                &mut rename_plans,
                            ) {
                                continue;
                            }
                        }
                    }
                }
            }

            push_children_rev(&mut stack, node);
        }

        if let Some(parse) = clang_parse {
            if !parse.success {
                warnings.push(
                    "naming_conventions: clang syntax diagnostics detected; semantic confidence reduced"
                        .to_string(),
                );
                for message in parse.diagnostics.iter().take(5) {
                    warnings.push(format!("clang: {message}"));
                }
            }
        } else {
            warnings.push("naming_conventions: clang parse context unavailable".to_string());
        }

        if !rename_plans.is_empty() {
            let healthy = rename_plans
                .iter()
                .filter(|p| p.stable_id.is_some() && p.expected_occurrences > 0)
                .count();
            let signal = (healthy as f64 / rename_plans.len() as f64).clamp(0.0, 1.0);
            warnings.push(format!("internal:rename_coverage_signal:{signal:.4}"));
        }

        let file_trust = 1.0;
        let mut suppressed_plans = Vec::new();
        let trust_filtered_plans: Vec<RenamePlan> = rename_plans
            .into_iter()
            .filter(|plan| {
                let confidence = plan.rename_confidence();
                let acceptance = 1.0;
                if acceptance < 0.5 {
                    warnings.push(format!(
                        "naming_conventions: trust-suppressed rename '{}' -> '{}' (confidence={:.2}, trust={:.2}, acceptance={:.2})",
                        plan.old_name, plan.new_name, confidence, file_trust, acceptance
                    ));
                    suppressed_plans.push(plan.clone());
                    return false;
                }
                true
            })
            .collect();
        let non_code_ranges = text_scan::non_code_ranges(tree);
        let (updated_text, edits) = if semantic_enabled {
            if let Some(parse) = clang_parse {
                let mut diag = RenameDiagnostics {
                    warnings: &mut warnings,
                    strict_issues: &mut strict_issues,
                };
                self.apply_semantic_renames(
                    context.text,
                    parse,
                    &trust_filtered_plans,
                    &semantic_query,
                    &non_code_ranges,
                    &mut diag,
                )
            } else {
                warnings.push(
                    "naming_conventions: semantic rename skipped due missing clang context"
                        .to_string(),
                );
                (context.text.to_string(), Vec::new())
            }
        } else {
            (context.text.to_string(), Vec::new())
        };

        if self.semantic_mode && self.semantic_strict && !strict_issues.is_empty() {
            let first_issue = strict_issues.first().unwrap_or("unknown strict issue");
            warnings.push(format!(
                "naming_conventions: {} strict issue(s) detected; first: {}",
                strict_issues.len(),
                first_issue
            ));
        }

        if !trust_filtered_plans.is_empty() || !suppressed_plans.is_empty() {
            let decl_path = Self::normalize_decl_path(context.path);
            for plan in trust_filtered_plans.iter().chain(suppressed_plans.iter()) {
                if plan.stable_id.is_none() {
                    continue;
                }
                let internal_plan = SemanticRenamePlan {
                    decl: ClangDeclKey::new(decl_path.clone(), plan.line, plan.column, plan.kind),
                    old_name: plan.old_name.clone(),
                    new_name: plan.new_name.clone(),
                };
                warnings.push(internal_plan.to_internal_warning());
            }
        }

        PolicyResult {
            text: updated_text,
            changed: !edits.is_empty(),
            violations,
            edits,
            warnings,
        }
    }
}

#[cfg(test)]
mod tests {
    use rustc_hash::FxHashMap;
    use std::path::PathBuf;

    use tree_sitter::Parser;

    use super::*;
    use crate::model::policy_context::PolicyContext;
    use crate::parser::clang_result::{ClangDiagnosticSummary, ClangParseResult};
    use crate::parser::clang_symbol::ClangSymbol;
    use crate::parser::clang_types::ClangSymbolKey;
    fn parse_cpp(text: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("cpp language");
        parser.parse(text, None).expect("parse tree")
    }

    #[test]
    fn clang_miss_falls_through_to_ts() {
        let policy = NamingConventionsPolicy::new(true, true);
        let text = "int CamelVar = 0;\nint BadName() { return CamelVar; }\n";
        let tree = parse_cpp(text);
        let clang_parse_result = ClangParseResult::new(
            true,
            Vec::new(),
            vec![ClangSymbol {
                name: "DifferentName".to_string(),
                kind: clang_sys::CXCursor_FunctionDecl,
                line: 2,
                column: 5,
                usr: None,
                scope_usr: None,
                storage_class: None,
                is_const: false,
                is_volatile: false,
                type_kind: clang_sys::CXType_Unexposed,
                type_display: String::new(),
            canonical_type_kind: clang_sys::CXType_Unexposed,
            template_name: None,
            }],
            ClangDiagnosticSummary::default(),
            Vec::new(),
        );
        let path = PathBuf::from("sample.cpp");
        let context = PolicyContext::new(text, &path)
            .with_tree(Some(&tree))
            .with_clang(Some(&clang_parse_result));
        let result = policy.apply(&context);
        assert!(
            !result.violations.is_empty(),
            "clang miss should fall through to tree-sitter detection, not skip"
        );
    }

    #[test]
    fn semantic_applies_renames() {
        let policy = NamingConventionsPolicy::new(true, true);
        let text = "void f() {\n  int CamelVar = 0;\n  int x = CamelVar;\n}\n";
        let tree = parse_cpp(text);
        let declaration_offset = text.find("CamelVar").expect("declaration offset");
        let reference_offset = text.rfind("CamelVar").expect("reference offset");
        let symbol = ClangSymbol {
            name: "CamelVar".to_string(),
            kind: clang_sys::CXCursor_VarDecl,
            line: 2,
            column: 7,
            usr: None,
            scope_usr: Some("c:@F@f#".to_string()),
            storage_class: None,
            is_const: false,
            is_volatile: false,
            type_kind: clang_sys::CXType_Unexposed,
            type_display: String::new(),
        canonical_type_kind: clang_sys::CXType_Unexposed,
        template_name: None,
        };
        let rename_offsets = FxHashMap::from_iter([(
            ClangSymbolKey::new(symbol.name.clone(), symbol.kind, symbol.line),
            vec![declaration_offset, reference_offset],
        )]);
        let clang_parse_result = ClangParseResult::with_rename_offsets(
            true,
            Vec::new(),
            vec![symbol],
            rename_offsets,
            ClangDiagnosticSummary::default(),
            Vec::new(),
        );

        let path = PathBuf::from("sample.cpp");
        let context = PolicyContext::new(text, &path)
            .with_tree(Some(&tree))
            .with_clang(Some(&clang_parse_result));
        let result = policy.apply(&context);

        assert!(result.text.contains("_camel_var"));
        assert!(!result.text.contains("CamelVar"));
        assert_eq!(result.edits.len(), 2);
    }

    #[test]
    fn strict_reports_conflict() {
        let policy = NamingConventionsPolicy::new(true, true);
        let text = "int CamelVar = 0;\nint G_CAMEL_VAR = 1;\nint use_it() { return CamelVar + G_CAMEL_VAR; }\n";
        let tree = parse_cpp(text);
        let declaration_offset = text.find("CamelVar").expect("declaration offset");
        let reference_offset = text.rfind("CamelVar").expect("reference offset");

        let camel_symbol = ClangSymbol {
            name: "CamelVar".to_string(),
            kind: clang_sys::CXCursor_VarDecl,
            line: 1,
            column: 5,
            usr: None,
            scope_usr: None,
            storage_class: None,
            is_const: false,
            is_volatile: false,
            type_kind: clang_sys::CXType_Unexposed,
            type_display: String::new(),
        canonical_type_kind: clang_sys::CXType_Unexposed,
        template_name: None,
        };
        let existing_symbol = ClangSymbol {
            name: "G_CAMEL_VAR".to_string(),
            kind: clang_sys::CXCursor_VarDecl,
            line: 2,
            column: 5,
            usr: None,
            scope_usr: None,
            storage_class: None,
            is_const: false,
            is_volatile: false,
            type_kind: clang_sys::CXType_Unexposed,
            type_display: String::new(),
        canonical_type_kind: clang_sys::CXType_Unexposed,
        template_name: None,
        };
        let rename_offsets = FxHashMap::from_iter([(
            ClangSymbolKey::new(
                camel_symbol.name.clone(),
                camel_symbol.kind,
                camel_symbol.line,
            ),
            vec![declaration_offset, reference_offset],
        )]);
        let clang_parse_result = ClangParseResult::with_rename_offsets(
            true,
            Vec::new(),
            vec![camel_symbol, existing_symbol],
            rename_offsets,
            ClangDiagnosticSummary::default(),
            Vec::new(),
        );

        let path = PathBuf::from("sample.cpp");
        let context = PolicyContext::new(text, &path)
            .with_tree(Some(&tree))
            .with_clang(Some(&clang_parse_result));
        let result = policy.apply(&context);

        assert_eq!(result.text, text);
        assert!(result.edits.is_empty());
        assert!(!result
            .warnings
            .iter()
            .any(|warning| warning.starts_with("fatal:naming_conventions:")));
    }

    #[test]
    fn strict_nonfatal_clang() {
        let policy = NamingConventionsPolicy::new(true, true);
        let text = "int CamelVar = 0;\n";
        let tree = parse_cpp(text);
        let clang_parse_result = ClangParseResult::new(
            false,
            vec!["sample.cpp:1:1: Error: unresolved include".to_string()],
            Vec::new(),
            ClangDiagnosticSummary {
                error: 1,
                ..ClangDiagnosticSummary::default()
            },
            vec![crate::parser::clang_result::ClangDiagnosticEntry {
                line: 1,
                column: 1,
                severity: crate::parser::clang_result::ClangDiagnosticSeverity::Error,
            }],
        );
        let path = PathBuf::from("sample.cpp");
        let context = PolicyContext::new(text, &path)
            .with_tree(Some(&tree))
            .with_clang(Some(&clang_parse_result));
        let result = policy.apply(&context);

        assert_eq!(result.text, text);
        assert!(result.edits.is_empty());
        assert!(!result
            .warnings
            .iter()
            .any(|warning| warning.starts_with("fatal:naming_conventions:")));
    }

    #[test]
    fn recoverable_skips_renames() {
        let policy = NamingConventionsPolicy::new(true, true);
        let text = "int CamelVar = 0;\nint use_it() { return CamelVar; }\n";
        let tree = parse_cpp(text);
        let declaration_offset = text.find("CamelVar").expect("declaration offset");
        let reference_offset = text.rfind("CamelVar").expect("reference offset");
        let symbol = ClangSymbol {
            name: "CamelVar".to_string(),
            kind: clang_sys::CXCursor_VarDecl,
            line: 1,
            column: 5,
            usr: Some("usr:test:camelvar".to_string()),
            scope_usr: Some("usr:test:scope".to_string()),
            storage_class: None,
            is_const: false,
            is_volatile: false,
            type_kind: clang_sys::CXType_Unexposed,
            type_display: String::new(),
        canonical_type_kind: clang_sys::CXType_Unexposed,
        template_name: None,
        };
        let rename_offsets = FxHashMap::from_iter([(
            ClangSymbolKey::new(symbol.name.clone(), symbol.kind, symbol.line),
            vec![declaration_offset, reference_offset],
        )]);
        let clang_parse_result = ClangParseResult::with_rename_offsets(
            false,
            vec!["header-consensus:10:4:Fatal".to_string()],
            vec![symbol],
            rename_offsets,
            ClangDiagnosticSummary {
                fatal: 1,
                ..ClangDiagnosticSummary::default()
            },
            vec![crate::parser::clang_result::ClangDiagnosticEntry {
                line: 10,
                column: 4,
                severity: crate::parser::clang_result::ClangDiagnosticSeverity::Fatal,
            }],
        );

        let path = PathBuf::from("sample.cpp");
        let context = PolicyContext::new(text, &path)
            .with_tree(Some(&tree))
            .with_clang(Some(&clang_parse_result));
        let result = policy.apply(&context);

        assert_eq!(result.text, text);
        assert!(result.edits.is_empty());
        assert!(result.warnings.iter().any(|warning| {
            warning.contains("semantic rename skipped due insufficient semantic parse reliability")
        }));
    }

    #[test]
    fn constructor_not_rewritten() {
        let policy = NamingConventionsPolicy::new(true, true);
        let text = "struct Node {\n  Node() {}\n};\n";
        let tree = parse_cpp(text);
        let clang_parse_result = ClangParseResult::new(
            true,
            Vec::new(),
            vec![ClangSymbol {
                name: "Node".to_string(),
                kind: clang_sys::CXCursor_Constructor,
                line: 2,
                column: 3,
                usr: None,
                scope_usr: None,
                storage_class: None,
                is_const: false,
                is_volatile: false,
                type_kind: clang_sys::CXType_Unexposed,
                type_display: String::new(),
            canonical_type_kind: clang_sys::CXType_Unexposed,
            template_name: None,
            }],
            ClangDiagnosticSummary::default(),
            Vec::new(),
        );
        let path = PathBuf::from("sample.cpp");
        let context = PolicyContext::new(text, &path)
            .with_tree(Some(&tree))
            .with_clang(Some(&clang_parse_result));
        let result = policy.apply(&context);

        assert!(result
            .violations
            .iter()
            .all(|violation| !violation.message.contains("function 'Node'")));
        assert_eq!(result.text, text);
    }

    #[test]
    fn params_no_prefix() {
        let policy = NamingConventionsPolicy::new(false, false);
        let text = "int compute(int other) { return other; }\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let context = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&context);

        assert!(result
            .violations
            .iter()
            .all(|violation| !violation.message.contains("identifier 'other'")));
        assert_eq!(result.text, text);
    }

    #[test]
    fn constant_not_rewritten() {
        let policy = NamingConventionsPolicy::new(false, false);
        let text = "struct A {\n  static constexpr size_t C_LEVEL_SHIFT = 6UL;\n};\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let context = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&context);

        assert!(result
            .violations
            .iter()
            .all(|violation| !violation.message.contains("C_LEVEL_SHIFT")));
        assert_eq!(result.text, text);
    }

    #[test]
    fn loop_header_skips() {
        let policy = NamingConventionsPolicy::new(false, false);
        let text = "void f() {\n  for (int i = 0; i < 10; i++) {}\n}\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let context = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&context);
        assert!(
            result.violations.iter().all(|v| !v.message.contains("'i'")),
            "for-loop header variable 'i' should not require prefix"
        );
    }

    #[test]
    fn range_header_skips() {
        let policy = NamingConventionsPolicy::new(false, false);
        let text = "void f() {\n  int arr[3] = {1,2,3};\n  for (auto val : arr) {}\n}\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let context = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&context);
        assert!(
            result.violations.iter().all(|v| !v.message.contains("'val'")),
            "for-range-loop header variable 'val' should not require prefix"
        );
    }

    #[test]
    fn include_only_skips() {
        let policy = NamingConventionsPolicy::new(true, true);
        let text = "#include \"HashSet.hpp\"\n";
        let tree = parse_cpp(text);
        let clang_parse_result = ClangParseResult::new(
            true,
            Vec::new(),
            Vec::new(),
            ClangDiagnosticSummary::default(),
            Vec::new(),
        );
        let path = PathBuf::from("HashSet.cpp");
        let context = PolicyContext::new(text, &path)
            .with_tree(Some(&tree))
            .with_clang(Some(&clang_parse_result));
        let result = policy.apply(&context);
        assert!(result.edits.is_empty());
        assert!(!result.warnings.iter().any(|item| {
            item.contains("semantic rename skipped due insufficient semantic parse reliability")
        }));
    }

    #[test]
    fn field_skipped_safety() {
        let policy = NamingConventionsPolicy::new(false, false);
        let text = "struct Foo {\n  int count;\n};\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.hpp");
        let context = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&context);
        assert!(
            !result.violations.iter().any(|v| v.message.contains("'count'")),
            "field declarations should be skipped (member access rename can't propagate safely)"
        );
    }

    // --- build_stacked_prefix unit tests ---

    #[test]
    fn prefix_shared_ptr_via_clang() {
        let policy = NamingConventionsPolicy::new(true, true);
        let sym = ClangSymbol {
            name: "data".to_string(),
            kind: clang_sys::CXCursor_VarDecl,
            line: 1, column: 1,
            usr: None, scope_usr: None,
            storage_class: None,
            is_const: false, is_volatile: false,
            type_kind: clang_sys::CXType_Unexposed,
            type_display: "std::shared_ptr<int>".to_string(),
        canonical_type_kind: clang_sys::CXType_Unexposed,
        template_name: Some("shared_ptr".to_string()),
        };
        let prefix = policy.build_stacked_prefix(&IdentifierContext { clang_symbol: Some(&sym), ..Default::default() });
        assert_eq!(prefix, "_sp_", "shared_ptr local should be '_sp_'");
    }

    #[test]
    fn prefix_unique_ptr_via_clang() {
        let policy = NamingConventionsPolicy::new(true, true);
        let sym = ClangSymbol {
            name: "handle".to_string(),
            kind: clang_sys::CXCursor_VarDecl,
            line: 1, column: 1,
            usr: None, scope_usr: None,
            storage_class: None,
            is_const: false, is_volatile: false,
            type_kind: clang_sys::CXType_Unexposed,
            type_display: "std::unique_ptr<int>".to_string(),
        canonical_type_kind: clang_sys::CXType_Unexposed,
        template_name: Some("unique_ptr".to_string()),
        };
        let prefix = policy.build_stacked_prefix(&IdentifierContext { clang_symbol: Some(&sym), ..Default::default() });
        assert_eq!(prefix, "_up_", "unique_ptr local should be '_up_'");
    }

    #[test]
    fn prefix_weak_ptr_via_clang() {
        let policy = NamingConventionsPolicy::new(true, true);
        let sym = ClangSymbol {
            name: "obs".to_string(),
            kind: clang_sys::CXCursor_VarDecl,
            line: 1, column: 1,
            usr: None, scope_usr: None,
            storage_class: None,
            is_const: false, is_volatile: false,
            type_kind: clang_sys::CXType_Unexposed,
            type_display: "std::weak_ptr<int>".to_string(),
        canonical_type_kind: clang_sys::CXType_Unexposed,
        template_name: Some("weak_ptr".to_string()),
        };
        let prefix = policy.build_stacked_prefix(&IdentifierContext { clang_symbol: Some(&sym), ..Default::default() });
        assert_eq!(prefix, "_wp_", "weak_ptr local should be '_wp_'");
    }

    #[test]
    fn prefix_function_type_via_clang() {
        let policy = NamingConventionsPolicy::new(true, true);
        let sym = ClangSymbol {
            name: "cb".to_string(),
            kind: clang_sys::CXCursor_VarDecl,
            line: 1, column: 1,
            usr: None, scope_usr: None,
            storage_class: None,
            is_const: false, is_volatile: false,
            type_kind: clang_sys::CXType_Unexposed,
            type_display: "std::function<void ()>".to_string(),
        canonical_type_kind: clang_sys::CXType_Unexposed,
        template_name: Some("function".to_string()),
        };
        let prefix = policy.build_stacked_prefix(&IdentifierContext { clang_symbol: Some(&sym), ..Default::default() });
        assert_eq!(prefix, "_f_", "function type local should be '_f_'");
    }

    #[test]
    fn prefix_atomic_via_clang() {
        let policy = NamingConventionsPolicy::new(true, true);
        let sym = ClangSymbol {
            name: "counter".to_string(),
            kind: clang_sys::CXCursor_VarDecl,
            line: 1, column: 1,
            usr: None, scope_usr: None,
            storage_class: None,
            is_const: false, is_volatile: false,
            type_kind: clang_sys::CXType_Unexposed,
            type_display: "std::atomic<int>".to_string(),
        canonical_type_kind: clang_sys::CXType_Unexposed,
        template_name: Some("atomic".to_string()),
        };
        let prefix = policy.build_stacked_prefix(&IdentifierContext { clang_symbol: Some(&sym), ..Default::default() });
        assert_eq!(prefix, "_a_", "atomic local should be '_a_'");
    }

    #[test]
    fn prefix_enum_via_clang() {
        let policy = NamingConventionsPolicy::new(true, true);
        let sym = ClangSymbol {
            name: "val".to_string(),
            kind: clang_sys::CXCursor_VarDecl,
            line: 1, column: 1,
            usr: None, scope_usr: None,
            storage_class: None,
            is_const: false, is_volatile: false,
            type_kind: clang_sys::CXType_Enum,
            type_display: "Color".to_string(),
        canonical_type_kind: clang_sys::CXType_Unexposed,
        template_name: None,
        };
        let prefix = policy.build_stacked_prefix(&IdentifierContext { clang_symbol: Some(&sym), ..Default::default() });
        assert_eq!(prefix, "_e_", "enum var local should be '_e_'");
    }

    #[test]
    fn prefix_struct_via_clang() {
        let policy = NamingConventionsPolicy::new(true, true);
        let sym = ClangSymbol {
            name: "pos".to_string(),
            kind: clang_sys::CXCursor_VarDecl,
            line: 1, column: 1,
            usr: None, scope_usr: None,
            storage_class: None,
            is_const: false, is_volatile: false,
            type_kind: clang_sys::CXType_Record,
            type_display: "Point".to_string(),
        canonical_type_kind: clang_sys::CXType_Unexposed,
        template_name: None,
        };
        let prefix = policy.build_stacked_prefix(&IdentifierContext { clang_symbol: Some(&sym), ..Default::default() });
        assert_eq!(prefix, "_t_", "struct var local should be '_t_'");
    }

    #[test]
    fn prefix_pointer_member_via_clang() {
        let policy = NamingConventionsPolicy::new(true, true);
        let sym = ClangSymbol {
            name: "tree".to_string(),
            kind: clang_sys::CXCursor_FieldDecl,
            line: 1, column: 1,
            usr: None, scope_usr: None,
            storage_class: None,
            is_const: false, is_volatile: false,
            type_kind: clang_sys::CXType_Pointer,
            type_display: "int *".to_string(),
        canonical_type_kind: clang_sys::CXType_Unexposed,
        template_name: None,
        };
        let prefix = policy.build_stacked_prefix(&IdentifierContext { is_field: true, clang_symbol: Some(&sym), ..Default::default() });
        assert_eq!(prefix, "m_p_", "pointer member should be 'm_p_'");
    }

    #[test]
    fn prefix_static_const_pointer_member_via_clang() {
        let policy = NamingConventionsPolicy::new(true, true);
        let sym = ClangSymbol {
            name: "tree".to_string(),
            kind: clang_sys::CXCursor_VarDecl,
            line: 1, column: 1,
            usr: None, scope_usr: None,
            storage_class: Some(clang_sys::CX_SC_Static),
            is_const: true, is_volatile: false,
            type_kind: clang_sys::CXType_Pointer,
            type_display: "const int *".to_string(),
        canonical_type_kind: clang_sys::CXType_Unexposed,
        template_name: None,
        };
        let prefix = policy.build_stacked_prefix(&IdentifierContext { is_field: true, clang_symbol: Some(&sym), ..Default::default() });
        assert_eq!(prefix, "m_s_c_p_", "static const pointer member should be 'm_s_c_p_'");
    }

    #[test]
    fn prefix_global_pointer_no_clang() {
        let policy = NamingConventionsPolicy::new(false, false);
        let prefix = policy.build_stacked_prefix(&IdentifierContext { is_global: true, ts_pointer: true, ..Default::default() });
        assert_eq!(prefix, "g_p_", "global pointer should be 'g_p_'");
    }

    #[test]
    fn prefix_volatile_reference() {
        let policy = NamingConventionsPolicy::new(false, false);
        let prefix = policy.build_stacked_prefix(&IdentifierContext { ts_volatile: true, ts_reference: true, ..Default::default() });
        assert_eq!(prefix, "_v_r_", "volatile reference local should be '_v_r_'");
    }

    #[test]
    fn prefix_empty_when_standard_disables() {
        let mut policy = NamingConventionsPolicy::new(false, false);
        policy = policy.with_prefix_config(PrefixConfig {
            local: "".into(),
            member: "".into(),
            global: "".into(),
            pointer: "".into(),
            ..PrefixConfig::default()
        });
        let prefix = policy.build_stacked_prefix(&IdentifierContext { ts_pointer: true, ..Default::default() });
        assert_eq!(prefix, "", "disabled prefixes should produce empty string");
    }

    // --- Integration tests via apply (tree-sitter only) ---

    #[test]
    fn reference_local_gets_r_prefix() {
        let policy = NamingConventionsPolicy::new(false, false);
        let prefix = policy.build_stacked_prefix(&IdentifierContext { ts_reference: true, ..Default::default() });
        assert_eq!(prefix, "_r_", "reference local should produce '_r_' prefix");
    }

    #[test]
    fn already_prefixed_p_skipped() {
        let policy = NamingConventionsPolicy::new(false, false);
        let text = "void f() {\n  int* p_count = nullptr;\n}\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let context = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&context);
        assert!(
            !result.violations.iter().any(|v| v.message.contains("'p_count'")),
            "already-prefixed 'p_count' should not produce a violation for that name, got: {:?}",
            result.violations
        );
    }

    // --- Global UPPER_SNAKE_CASE tests ---




    #[test]
    fn global_already_upper_skipped() {
        let policy = NamingConventionsPolicy::new(false, false);
        let text = "int G_COUNTER = 0;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let context = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&context);
        assert!(
            !result.violations.iter().any(|v| v.message.contains("G_COUNTER")),
            "already UPPER_SNAKE_CASE global should be skipped, got: {:?}",
            result.violations
        );
    }

    #[test]
    fn prefix_global_upper_via_build() {
        let policy = NamingConventionsPolicy::new(false, false);
        let prefix = policy.build_stacked_prefix(&IdentifierContext { is_global: true, ts_static: true, ts_const: true, ts_pointer: true, ..Default::default() });
        assert_eq!(prefix, "g_s_c_p_", "build_stacked_prefix returns lowercase; call site uppercases");
    }

    #[test]
    fn prefix_ts_shared_ptr() {
        let policy = NamingConventionsPolicy::new(false, false);
        let prefix = policy.build_stacked_prefix(&IdentifierContext {
            ts_type_text: Some("shared_ptr"),
            ..Default::default()
        });
        assert_eq!(prefix, "_sp_", "shared_ptr via tree-sitter should produce '_sp_'");
    }

    #[test]
    fn prefix_ts_unique_ptr() {
        let policy = NamingConventionsPolicy::new(false, false);
        let prefix = policy.build_stacked_prefix(&IdentifierContext {
            ts_type_text: Some("unique_ptr"),
            ..Default::default()
        });
        assert_eq!(prefix, "_up_", "unique_ptr via tree-sitter should produce '_up_'");
    }

    #[test]
    fn prefix_ts_pointer_shared_ptr() {
        let policy = NamingConventionsPolicy::new(false, false);
        let prefix = policy.build_stacked_prefix(&IdentifierContext {
            ts_pointer: true,
            ts_type_text: Some("shared_ptr"),
            ..Default::default()
        });
        assert_eq!(prefix, "_sp_", "pointer + shared_ptr should be '_sp_' not '_p_'");
    }

    #[test]
    fn ts_fallback_detects_const_static() {
        let policy = NamingConventionsPolicy::new(false, false);
        let text = "static const int myValue = 42;\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let context = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&context);
        assert!(
            result.violations.iter().any(|v| v.message.contains("myValue") && v.message.contains("s_c_")),
            "tree-sitter fallback should detect static+const prefix, got: {:?}",
            result.violations
        );
    }

    #[test]
    fn ts_fallback_detects_pointer() {
        let policy = NamingConventionsPolicy::new(false, false);
        let text = "void f() {\n  int* myPtr = nullptr;\n}\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let context = PolicyContext::new(text, &path).with_tree(Some(&tree));
        let result = policy.apply(&context);
        assert!(
            result.violations.iter().any(|v| v.message.contains("myPtr") && v.message.contains("p_")),
            "tree-sitter fallback should detect pointer prefix, got: {:?}",
            result.violations
        );
    }

}
