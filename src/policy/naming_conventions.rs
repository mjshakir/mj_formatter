use std::collections::{HashMap, HashSet};
use std::fs;
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
use crate::parser::clang_types::ClangSymbolKind;
use crate::parser::file_context::SemanticFileContext;
use crate::parser::node_kind;
use crate::parser::ts_traversal;
use crate::policy::traits::Policy;
use crate::text_scan;

#[derive(Clone, Debug, Eq, PartialEq)]
struct RenamePlan {
    old_name: String,
    new_name: String,
    line: usize,
    column: usize,
    kind: ClangSymbolKind,
    minimum_required_occurrences: usize,
    expected_occurrences: usize,
    stable_id: Option<String>,
}

impl RenamePlan {
    fn rename_confidence(&self) -> f64 {
        let usr_backed = self.stable_id.is_some();
        let has_refs = self.expected_occurrences > 0;
        let all_safe = self.minimum_required_occurrences >= self.expected_occurrences;
        match (usr_backed, has_refs, all_safe) {
            (true, true, true) => 1.0,
            (true, true, false) => 0.8,
            (true, false, _) => 0.6,
            (false, _, _) => 0.3,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Replacement {
    start: usize,
    end: usize,
    line: usize,
    old_name: String,
    new_name: String,
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

pub struct NamingConventionsPolicy {
    semantic_mode: bool,
    semantic_strict: bool,
    local_prefix: String,
    member_prefix: String,
}

fn find_uppercase_positions(bytes: &[u8]) -> Vec<usize> {
    find_uppercase_positions_impl(bytes)
}

#[cfg(target_arch = "aarch64")]
fn find_uppercase_positions_impl(bytes: &[u8]) -> Vec<usize> {
    use core::arch::aarch64::*;
    let mut positions = Vec::new();
    let chunks = bytes.len() / 16;
    unsafe {
        let a_val = vdupq_n_u8(b'A');
        let range = vdupq_n_u8(25); // 'Z' - 'A' = 25
        for chunk in 0..chunks {
            let offset = chunk * 16;
            let v = vld1q_u8(bytes.as_ptr().add(offset));
            let shifted = vsubq_u8(v, a_val);
            let is_upper = vcleq_u8(shifted, range);
            let mask_lo = vgetq_lane_u64(vreinterpretq_u64_u8(is_upper), 0);
            let mask_hi = vgetq_lane_u64(vreinterpretq_u64_u8(is_upper), 1);
            for bit in 0..8 {
                if (mask_lo >> (bit * 8)) & 0xFF != 0 {
                    positions.push(offset + bit);
                }
            }
            for bit in 0..8 {
                if (mask_hi >> (bit * 8)) & 0xFF != 0 {
                    positions.push(offset + 8 + bit);
                }
            }
        }
    }
    for (i, &b) in bytes.iter().enumerate().skip(chunks * 16) {
        if b.is_ascii_uppercase() {
            positions.push(i);
        }
    }
    positions
}

#[cfg(not(target_arch = "aarch64"))]
fn find_uppercase_positions_impl(bytes: &[u8]) -> Vec<usize> {
    bytes
        .iter()
        .enumerate()
        .filter_map(|(i, &b)| b.is_ascii_uppercase().then_some(i))
        .collect()
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
        semantic_context.is_some_and(|semantic| {
            let summary = semantic.summary();
            summary.declaration_count > 0 && summary.reference_count > 0
        })
    }

    pub fn new(semantic_mode: bool, semantic_strict: bool) -> Self {
        Self {
            semantic_mode,
            semantic_strict,
            local_prefix: "_".to_string(),
            member_prefix: "m_".to_string(),
        }
    }

    pub fn with_prefixes(mut self, local_prefix: &str, member_prefix: &str) -> Self {
        self.local_prefix = local_prefix.to_string();
        self.member_prefix = member_prefix.to_string();
        self
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

        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if matches!(
                node.kind(),
                node_kind::FUNCTION_DEFINITION
                    | node_kind::DECLARATION
                    | node_kind::FIELD_DECLARATION
            ) {
                return true;
            }
            for idx in (0..node.child_count()).rev() {
                if let Some(child) = node.child(idx as u32) {
                    stack.push(child);
                }
            }
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

    fn is_snake_case(name: &str) -> bool {
        if name.is_empty() {
            return true;
        }
        let mut chars = name.chars();
        let Some(first) = chars.next() else {
            return true;
        };
        if !(first.is_ascii_lowercase() || first == '_') {
            return false;
        }
        chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
    }

    fn is_upper_snake_case(name: &str) -> bool {
        if name.is_empty() {
            return false;
        }
        let mut has_alpha = false;
        for ch in name.chars() {
            if ch.is_ascii_alphabetic() {
                has_alpha = true;
            }
            if !(ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_') {
                return false;
            }
        }
        has_alpha
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

    fn to_snake_case(value: &str) -> String {
        let bytes = value.as_bytes();
        let len = bytes.len();
        let mut result = Vec::with_capacity(len + 4);

        let upper_positions = find_uppercase_positions(bytes);

        if upper_positions.is_empty() {
            return value.to_string();
        }

        let mut pos_idx = 0;
        for i in 0..len {
            if pos_idx < upper_positions.len() && upper_positions[pos_idx] == i {
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
        unsafe { String::from_utf8_unchecked(result) }
    }

    fn is_keyword(name: &str) -> bool {
        matches!(
            name,
            "if" | "for" | "while" | "switch" | "catch" | "return" | "operator" | "constexpr"
        )
    }

    fn is_identifier_byte(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || byte == b'_'
    }

    fn has_identifier_boundaries(text: &str, start: usize, end: usize) -> bool {
        let bytes = text.as_bytes();
        if start > 0 && Self::is_identifier_byte(bytes[start - 1]) {
            return false;
        }
        if end < bytes.len() && Self::is_identifier_byte(bytes[end]) {
            return false;
        }
        true
    }

    fn line_starts(text: &str) -> Vec<usize> {
        text_scan::line_starts(text, false)
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
        let covered = covered_offsets.iter().copied().collect::<HashSet<_>>();
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

    fn normalize_decl_path(path: &Path) -> String {
        fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .to_string()
    }

    #[allow(clippy::too_many_arguments)]
    fn build_replacements(
        &self,
        text: &str,
        clang_parse: &ClangParseResult,
        plans: &[RenamePlan],
        semantic_query: &SemanticContextQuery<'_>,
        warnings: &mut Vec<String>,
        strict_issues: &mut StrictIssues,
    ) -> Vec<Replacement> {
        let line_starts = Self::line_starts(text);
        let mut by_start: HashMap<usize, Replacement> =
            HashMap::with_capacity(plans.len().saturating_mul(2));
        let mut conflicting_starts = HashSet::new();

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
                clang_parse.rename_offsets_on_line(
                    &plan.old_name,
                    plan.line,
                    std::slice::from_ref(&plan.kind),
                )
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
            if plan.kind == ClangSymbolKind::Field {
                if let Some(offset) = Self::find_uncovered_field_initializer_label_offset(
                    text,
                    plan.old_name.as_str(),
                    offsets.as_slice(),
                ) {
                    let (line, _column) = Self::line_and_column_for_offset(&line_starts, offset);
                    warnings.push(format!(
                        "naming_conventions: skipped semantic rename '{}' on line {} due uncovered constructor initializer label at line {}",
                        plan.old_name, plan.line, line
                    ));
                    strict_issues.push_lazy(|| {
                        format!(
                            "uncovered constructor initializer label for '{}' at line {}",
                            plan.old_name, line
                        )
                    });
                    continue;
                }
            }
            if offsets.is_empty() {
                warnings.push(format!(
                    "naming_conventions: no semantic references found for '{}' on line {}",
                    plan.old_name, plan.line
                ));
                strict_issues.push_lazy(|| {
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
            let mut plan_replacements = Vec::<Replacement>::new();
            for offset in offsets {
                let end = offset.saturating_add(plan.old_name.len());
                if end > text.len() || !text.is_char_boundary(offset) || !text.is_char_boundary(end)
                {
                    warnings.push(format!(
                        "naming_conventions: skipped invalid rename span for '{}' at byte {}",
                        plan.old_name, offset
                    ));
                    strict_issues.push_lazy(|| {
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
                let (line, column) = Self::line_and_column_for_offset(&line_starts, offset);
                if semantic_query.is_available() && !semantic_query.is_safe_edit(line, column) {
                    warnings.push(format!(
                        "naming_conventions: skipped semantic-unsafe rename '{}' at line {}",
                        plan.old_name, line
                    ));
                    strict_issues.push_lazy(|| {
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
                    old_name: plan.old_name.clone(),
                    new_name: plan.new_name.clone(),
                };
                plan_replacements.push(replacement);
            }

            if plan.expected_occurrences > 0 {
                let realized = plan_replacements.len();
                let safe_gap = plan.minimum_required_occurrences.saturating_sub(realized);
                if safe_gap > 0 {
                    warnings.push(format!(
                        "naming_conventions: skipped semantic rename '{}' on line {} due filtered semantic safe-coverage ({} < {})",
                        plan.old_name,
                        plan.line,
                        realized,
                        plan.minimum_required_occurrences
                    ));
                    strict_issues.push_lazy(|| {
                        format!(
                            "filtered semantic safe-coverage for '{}' on line {} ({} < {})",
                            plan.old_name, plan.line, realized, plan.minimum_required_occurrences
                        )
                    });
                    continue;
                }
                if realized < plan.expected_occurrences {
                    warnings.push(format!(
                        "naming_conventions: skipped semantic rename '{}' on line {} due filtered full-coverage ({} < {})",
                        plan.old_name,
                        plan.line,
                        realized,
                        plan.expected_occurrences
                    ));
                    strict_issues.push_lazy(|| {
                        format!(
                            "filtered full-coverage for '{}' on line {} ({} < {})",
                            plan.old_name, plan.line, realized, plan.expected_occurrences
                        )
                    });
                    continue;
                }
            }

            if let Some(declaration_offset) = declaration_offset {
                let declaration_covered = plan_replacements
                    .iter()
                    .any(|replacement| replacement.start == declaration_offset);
                if !declaration_covered {
                    warnings.push(format!(
                        "naming_conventions: skipped semantic rename '{}' on line {} because declaration span was not safely renameable",
                        plan.old_name, plan.line
                    ));
                    strict_issues.push_lazy(|| {
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
                        warnings.push(format!(
                            "naming_conventions: conflicting semantic edits at byte {}; skipping",
                            replacement.start
                        ));
                        strict_issues.push_lazy(|| {
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
                warnings.push(format!(
                    "naming_conventions: overlapping semantic edit near line {}; skipping",
                    replacement.line
                ));
                strict_issues.push_lazy(|| {
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
        warnings: &mut Vec<String>,
        strict_issues: &mut StrictIssues,
    ) -> (String, Vec<Edit>) {
        let mut replacements = self.build_replacements(
            text,
            clang_parse,
            plans,
            semantic_query,
            warnings,
            strict_issues,
        );
        if replacements.is_empty() {
            if !plans.is_empty() {
                strict_issues.push("semantic rename produced no concrete replacements".to_string());
            }
            return (text.to_string(), Vec::new());
        }

        let mut output = text.to_string();
        replacements.sort_by(|left, right| right.start.cmp(&left.start));

        let mut edits = Vec::with_capacity(replacements.len());
        for replacement in &replacements {
            output.replace_range(
                replacement.start..replacement.end,
                replacement.new_name.as_str(),
            );
            edits.push(Edit {
                policy: self.name().into(),
                line: replacement.line,
                before: replacement.old_name.clone(),
                after: replacement.new_name.clone(),
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
            return PolicyResult {
                text: context.text.to_string(),
                violations: Vec::new(),
                edits: Vec::new(),
                warnings: vec!["naming_conventions: tree-sitter context unavailable".to_string()],
            };
        };

        let mut violations = Vec::new();
        let mut warnings = Vec::new();
        let mut strict_issues = StrictIssues::new(self.semantic_mode && self.semantic_strict);
        let mut rename_plans = Vec::new();
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
        let mut stack = vec![root];

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
                            ClangSymbolKind::Function,
                            ClangSymbolKind::FunctionTemplate,
                            ClangSymbolKind::Method,
                            ClangSymbolKind::Constructor,
                            ClangSymbolKind::Destructor,
                        ];
                        let matched_symbol = if let Some(parse) = clang_parse {
                            let Some(symbol) = parse.symbol_on_line(short, line, &allowed) else {
                                continue;
                            };
                            Some((symbol.kind, symbol.column))
                        } else {
                            if name_node.kind() == node_kind::TYPE_IDENTIFIER {
                                continue;
                            }
                            None
                        };

                        if !short.starts_with("operator")
                            && !Self::is_keyword(short)
                            && !Self::is_snake_case(short)
                        {
                            if let Some((kind, _)) = matched_symbol {
                                if matches!(
                                    kind,
                                    ClangSymbolKind::Constructor | ClangSymbolKind::Destructor
                                ) {
                                    continue;
                                }
                            }
                            let suggested = Self::to_snake_case(short);
                            violations.push(Violation {
                                policy: self.name().into(),
                                message: format!(
                                    "function '{short}' is not snake_case; suggested '{suggested}'"
                                ),
                                line,
                                column: Some(source_column),
                            });

                            if semantic_enabled {
                                let Some(parse) = clang_parse else {
                                    continue;
                                };
                                let Some((kind, column)) = matched_symbol else {
                                    continue;
                                };
                                let mut expected_occurrences = 0usize;
                                let mut minimum_required_occurrences = 0usize;
                                let mut stable_id = None::<String>;
                                if semantic_query.is_available()
                                    && !semantic_query.is_safe_edit(line, source_column)
                                {
                                    warnings.push(format!(
                                        "naming_conventions: skipped semantic rename '{}' -> '{}' in semantic-unsafe region",
                                        short, suggested
                                    ));
                                    strict_issues.push_lazy(|| {
                                        format!(
                                            "semantic-unsafe rename '{}' -> '{}'",
                                            short, suggested
                                        )
                                    });
                                    continue;
                                }
                                if semantic_query.is_available()
                                    && semantic_query
                                        .symbol_at(line, source_column, &allowed)
                                        .is_none()
                                {
                                    warnings.push(format!(
                                        "naming_conventions: skipped semantic rename '{}' -> '{}' due symbol mismatch",
                                        short, suggested
                                    ));
                                    strict_issues.push_lazy(|| {
                                        format!(
                                            "symbol mismatch for semantic rename '{}' -> '{}'",
                                            short, suggested
                                        )
                                    });
                                    continue;
                                }
                                if semantic_query.is_available() {
                                    let Some(declaration) =
                                        semantic_query.symbol_at(line, source_column, &allowed)
                                    else {
                                        continue;
                                    };
                                    stable_id = Some(declaration.stable_id.clone());
                                    let references = semantic_query
                                        .references_of(declaration.stable_id.as_str());
                                    let safe_reference_count = references
                                        .iter()
                                        .filter(|reference| {
                                            semantic_query
                                                .is_safe_edit(reference.line, reference.column)
                                        })
                                        .count();
                                    expected_occurrences = references.len().max(1);
                                    minimum_required_occurrences = safe_reference_count.max(1);
                                    let parser_offset_count = parse
                                        .rename_offsets_on_line(
                                            short,
                                            line,
                                            std::slice::from_ref(&kind),
                                        )
                                        .len();
                                    let divergence_limit = expected_occurrences
                                        .saturating_mul(2)
                                        .max(expected_occurrences.saturating_add(3));
                                    if parser_offset_count >= 8
                                        && parser_offset_count > divergence_limit
                                    {
                                        warnings.push(format!(
                                            "naming_conventions: skipped semantic rename '{}' -> '{}' due semantic/clang offset divergence (semantic_refs={}, clang_offsets={})",
                                            short, suggested, expected_occurrences, parser_offset_count
                                        ));
                                        strict_issues.push_lazy(|| {
                                            format!(
                                                "semantic/clang offset divergence for '{}' -> '{}' (semantic_refs={}, clang_offsets={})",
                                                short, suggested, expected_occurrences, parser_offset_count
                                            )
                                        });
                                        continue;
                                    }
                                    if semantic_query
                                        .declaration_by_stable_id(declaration.stable_id.as_str())
                                        .is_none()
                                    {
                                        strict_issues.push_lazy(|| {
                                            format!(
                                                "missing declaration for semantic rename '{}' -> '{}'",
                                                short, suggested
                                            )
                                        });
                                        continue;
                                    }
                                    if references.is_empty() {
                                        warnings.push(format!(
                                            "naming_conventions: skipped semantic rename '{}' -> '{}' due empty semantic references",
                                            short, suggested
                                        ));
                                        strict_issues.push_lazy(|| {
                                            format!(
                                                "empty semantic references for '{}' -> '{}'",
                                                short, suggested
                                            )
                                        });
                                        continue;
                                    }
                                }
                                if parse.has_symbol_name_elsewhere(&suggested, line) {
                                    warnings.push(format!(
                                        "naming_conventions: skipped semantic rename '{}' -> '{}' due name conflict",
                                        short, suggested
                                    ));
                                    strict_issues.push_lazy(|| {
                                        format!(
                                            "name conflict for semantic rename '{}' -> '{}'",
                                            short, suggested
                                        )
                                    });
                                    continue;
                                }
                                rename_plans.push(RenamePlan {
                                    old_name: short.to_string(),
                                    new_name: suggested,
                                    line,
                                    column,
                                    kind,
                                    minimum_required_occurrences,
                                    expected_occurrences,
                                    stable_id,
                                });
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
                        ClangSymbolKind::Function,
                        ClangSymbolKind::FunctionTemplate,
                        ClangSymbolKind::Method,
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
                            || Self::is_keyword(short)
                            || Self::is_snake_case(short)
                        {
                            continue;
                        }
                        let line = symbol.line;
                        let source_column = symbol.column;
                        let suggested = Self::to_snake_case(short);
                        violations.push(Violation {
                            policy: self.name().into(),
                            message: format!(
                                "function '{short}' is not snake_case; suggested '{suggested}'"
                            ),
                            line,
                            column: Some(source_column),
                        });

                        if semantic_enabled {
                            let kind = symbol.kind;
                            let column = source_column;
                            let mut expected_occurrences = 0usize;
                            let mut minimum_required_occurrences = 0usize;
                            let mut stable_id = None::<String>;
                            if semantic_query.is_available()
                                && !semantic_query.is_safe_edit(line, source_column)
                            {
                                warnings.push(format!(
                                    "naming_conventions: skipped semantic rename '{}' -> '{}' in semantic-unsafe region",
                                    short, suggested
                                ));
                                continue;
                            }
                            if semantic_query.is_available()
                                && semantic_query
                                    .symbol_at(line, source_column, &allowed)
                                    .is_none()
                            {
                                warnings.push(format!(
                                    "naming_conventions: skipped semantic rename '{}' -> '{}' due symbol mismatch",
                                    short, suggested
                                ));
                                continue;
                            }
                            if semantic_query.is_available() {
                                let Some(declaration) =
                                    semantic_query.symbol_at(line, source_column, &allowed)
                                else {
                                    continue;
                                };
                                stable_id = Some(declaration.stable_id.clone());
                                let references = semantic_query
                                    .references_of(declaration.stable_id.as_str());
                                let safe_reference_count = references
                                    .iter()
                                    .filter(|reference| {
                                        semantic_query
                                            .is_safe_edit(reference.line, reference.column)
                                    })
                                    .count();
                                expected_occurrences = references.len().max(1);
                                minimum_required_occurrences = safe_reference_count.max(1);
                                let parser_offset_count = parse
                                    .rename_offsets_on_line(
                                        short,
                                        line,
                                        std::slice::from_ref(&kind),
                                    )
                                    .len();
                                let divergence_limit = expected_occurrences
                                    .saturating_mul(2)
                                    .max(expected_occurrences.saturating_add(3));
                                if parser_offset_count >= 8
                                    && parser_offset_count > divergence_limit
                                {
                                    warnings.push(format!(
                                        "naming_conventions: skipped semantic rename '{}' -> '{}' due semantic/clang offset divergence (semantic_refs={}, clang_offsets={})",
                                        short, suggested, expected_occurrences, parser_offset_count
                                    ));
                                    continue;
                                }
                                if semantic_query
                                    .declaration_by_stable_id(declaration.stable_id.as_str())
                                    .is_none()
                                {
                                    continue;
                                }
                                if references.is_empty() {
                                    warnings.push(format!(
                                        "naming_conventions: skipped semantic rename '{}' -> '{}' due empty semantic references",
                                        short, suggested
                                    ));
                                    continue;
                                }
                            }
                            if parse.has_symbol_name_elsewhere(&suggested, line) {
                                warnings.push(format!(
                                    "naming_conventions: skipped semantic rename '{}' -> '{}' due name conflict",
                                    short, suggested
                                ));
                                continue;
                            }
                            rename_plans.push(RenamePlan {
                                old_name: short.to_string(),
                                new_name: suggested,
                                line,
                                column,
                                kind,
                                minimum_required_occurrences,
                                expected_occurrences,
                                stable_id,
                            });
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
                    for idx in (0..node.child_count()).rev() {
                        if let Some(child) = node.child((idx) as u32) {
                            stack.push(child);
                        }
                    }
                    continue;
                }
                if Self::is_loop_header_declaration(node) {
                    continue;
                }
                if let Some(name_node) = ts_traversal::rightmost_descendant(
                    node,
                    &[node_kind::IDENTIFIER, node_kind::FIELD_IDENTIFIER],
                    &[node_kind::PARAMETER_LIST, node_kind::TEMPLATE_PARAMETER_LIST],
                ) {
                    let name = name_node.utf8_text(context.text.as_bytes()).unwrap_or("");
                    let line = name_node.start_position().row + 1;
                    let source_column = name_node.start_position().column + 1;
                    let allowed = [
                        ClangSymbolKind::Variable,
                        ClangSymbolKind::Field,
                        ClangSymbolKind::Parameter,
                    ];
                    let matched_symbol = if let Some(parse) = clang_parse {
                        let Some(symbol) = parse.symbol_on_line(name, line, &allowed) else {
                            continue;
                        };
                        Some((symbol.kind, symbol.column))
                    } else {
                        None
                    };

                    if name.starts_with("m_")
                        || name.starts_with("g_")
                        || name.starts_with("s_")
                        || name.starts_with("c_")
                        || name.starts_with("C_")
                        || name.starts_with("S_")
                        || Self::is_constant_like_identifier(name)
                        || Self::is_keyword(name)
                        || name.starts_with('_')
                    {
                        for idx in (0..node.child_count()).rev() {
                            if let Some(child) = node.child((idx) as u32) {
                                stack.push(child);
                            }
                        }
                        continue;
                    }

                    let is_field = node.kind() == node_kind::FIELD_DECLARATION
                        || matched_symbol.is_some_and(|(kind, _)| kind == ClangSymbolKind::Field);
                    let prefix = if is_field { &self.member_prefix } else { &self.local_prefix };
                    let suggested = format!("{}{}", prefix, Self::to_snake_case(name));
                    violations.push(Violation {
                        policy: self.name().into(),
                        message: format!(
                            "local/member-like identifier '{name}' missing prefix; suggested '{suggested}'"
                        ),
                        line,
                        column: Some(source_column),
                    });

                    if semantic_enabled {
                        let Some(parse) = clang_parse else {
                            continue;
                        };
                        let Some((kind, column)) = matched_symbol else {
                            continue;
                        };
                        let mut expected_occurrences = 0usize;
                        let mut minimum_required_occurrences = 0usize;
                        let mut stable_id = None::<String>;
                        if semantic_query.is_available()
                            && !semantic_query.is_safe_edit(line, source_column)
                        {
                            warnings.push(format!(
                                "naming_conventions: skipped semantic rename '{}' -> '{}' in semantic-unsafe region",
                                name, suggested
                            ));
                            strict_issues.push_lazy(|| {
                                format!("semantic-unsafe rename '{}' -> '{}'", name, suggested)
                            });
                            continue;
                        }
                        if semantic_query.is_available()
                            && semantic_query
                                .symbol_at(line, source_column, &allowed)
                                .is_none()
                        {
                            warnings.push(format!(
                                "naming_conventions: skipped semantic rename '{}' -> '{}' due symbol mismatch",
                                name, suggested
                            ));
                            strict_issues.push_lazy(|| {
                                format!(
                                    "symbol mismatch for semantic rename '{}' -> '{}'",
                                    name, suggested
                                )
                            });
                            continue;
                        }
                        if semantic_query.is_available() {
                            let Some(declaration) =
                                semantic_query.symbol_at(line, source_column, &allowed)
                            else {
                                continue;
                            };
                            stable_id = Some(declaration.stable_id.clone());
                            let references =
                                semantic_query.references_of(declaration.stable_id.as_str());
                            let safe_reference_count = references
                                .iter()
                                .filter(|reference| {
                                    semantic_query.is_safe_edit(reference.line, reference.column)
                                })
                                .count();
                            expected_occurrences = references.len().max(1);
                            minimum_required_occurrences = safe_reference_count.max(1);
                            let parser_offset_count = parse
                                .rename_offsets_on_line(name, line, std::slice::from_ref(&kind))
                                .len();
                            let divergence_limit = expected_occurrences
                                .saturating_mul(2)
                                .max(expected_occurrences.saturating_add(3));
                            if parser_offset_count >= 8 && parser_offset_count > divergence_limit {
                                warnings.push(format!(
                                    "naming_conventions: skipped semantic rename '{}' -> '{}' due semantic/clang offset divergence (semantic_refs={}, clang_offsets={})",
                                    name, suggested, expected_occurrences, parser_offset_count
                                ));
                                strict_issues.push_lazy(|| {
                                    format!(
                                        "semantic/clang offset divergence for '{}' -> '{}' (semantic_refs={}, clang_offsets={})",
                                        name, suggested, expected_occurrences, parser_offset_count
                                    )
                                });
                                continue;
                            }
                            let fanout = references.len();
                            if name.len() <= 1 && fanout > 8 {
                                warnings.push(format!(
                                    "naming_conventions: skipped semantic rename '{}' -> '{}' due high-fanout short identifier (refs={})",
                                    name, suggested, fanout
                                ));
                                strict_issues.push_lazy(|| {
                                    format!(
                                        "high-fanout short identifier for semantic rename '{}' -> '{}' (refs={})",
                                        name, suggested, fanout
                                    )
                                });
                                continue;
                            }
                            if fanout > 32 {
                                warnings.push(format!(
                                    "naming_conventions: skipped semantic rename '{}' -> '{}' due high semantic fanout (refs={})",
                                    name, suggested, fanout
                                ));
                                strict_issues.push_lazy(|| {
                                    format!(
                                        "high semantic fanout for semantic rename '{}' -> '{}' (refs={})",
                                        name, suggested, fanout
                                    )
                                });
                                continue;
                            }
                            if semantic_query
                                .declaration_by_stable_id(declaration.stable_id.as_str())
                                .is_none()
                            {
                                strict_issues.push_lazy(|| {
                                    format!(
                                        "missing declaration for semantic rename '{}' -> '{}'",
                                        name, suggested
                                    )
                                });
                                continue;
                            }
                            if references.is_empty() {
                                warnings.push(format!(
                                    "naming_conventions: skipped semantic rename '{}' -> '{}' due empty semantic references",
                                    name, suggested
                                ));
                                strict_issues.push_lazy(|| {
                                    format!(
                                        "empty semantic references for '{}' -> '{}'",
                                        name, suggested
                                    )
                                });
                                continue;
                            }
                        }
                        if parse.has_symbol_name_elsewhere(&suggested, line) {
                            warnings.push(format!(
                                "naming_conventions: skipped semantic rename '{}' -> '{}' due name conflict",
                                name, suggested
                            ));
                            strict_issues.push_lazy(|| {
                                format!(
                                    "name conflict for semantic rename '{}' -> '{}'",
                                    name, suggested
                                )
                            });
                        } else {
                            rename_plans.push(RenamePlan {
                                old_name: name.to_string(),
                                new_name: suggested,
                                line,
                                column,
                                kind,
                                minimum_required_occurrences,
                                expected_occurrences,
                                stable_id,
                            });
                        }
                    }
                }
            }

            for idx in (0..node.child_count()).rev() {
                if let Some(child) = node.child((idx) as u32) {
                    stack.push(child);
                }
            }
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

        let trust_willingness =
            context.parser_trust.scaled_edit_willingness();
        let edit_bar = 1.0 - trust_willingness;
        let project_query = context.project_query();
        let trust_filtered_plans: Vec<RenamePlan> = rename_plans
            .into_iter()
            .filter(|plan| {
                let confidence = plan.rename_confidence();
                if confidence < edit_bar {
                    warnings.push(format!(
                        "naming_conventions: trust-suppressed rename '{}' -> '{}' (confidence={:.2}, trust_willingness={:.2}, bar={:.3})",
                        plan.old_name, plan.new_name, confidence, trust_willingness, edit_bar
                    ));
                    return false;
                }
                if let Some(ref stable_id) = plan.stable_id {
                    if let Some(signal) = project_query.project_signal_for_stable_id(stable_id) {
                        if signal.file_count > 1 {
                            warnings.push(format!(
                                "naming_conventions: cross-file rename suppressed '{}' -> '{}' (symbol referenced in {} files)",
                                plan.old_name, plan.new_name, signal.file_count
                            ));
                            return false;
                        }
                    }
                }
                true
            })
            .collect();
        let (updated_text, edits) = if semantic_enabled {
            if let Some(parse) = clang_parse {
                self.apply_semantic_renames(
                    context.text,
                    parse,
                    &trust_filtered_plans,
                    &semantic_query,
                    &mut warnings,
                    &mut strict_issues,
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
                "naming_conventions: strict semantic gate blocked {} rename decision(s); continuing with safe subset; first: {}",
                strict_issues.len(),
                first_issue
            ));
            let issue_line = violations.first().map(|item| item.line).unwrap_or(1);
            violations.push(Violation {
                policy: self.name().into(),
                message: format!(
                    "strict semantic gate blocked {} rename decision(s); first: {}",
                    strict_issues.len(),
                    first_issue
                ),
                line: issue_line,
                column: Some(1),
            });
        }

        if !trust_filtered_plans.is_empty() {
            let decl_path = Self::normalize_decl_path(context.path);
            for plan in &trust_filtered_plans {
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
            violations,
            edits,
            warnings,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use tree_sitter::Parser;

    use super::*;
    use crate::model::policy_context::PolicyContext;
    use crate::parser::clang_result::{ClangDiagnosticSummary, ClangParseResult};
    use crate::parser::clang_symbol::ClangSymbol;
    use crate::parser::clang_types::ClangSymbolKey;
    use crate::parser::clang_types::ClangSymbolKind;

    fn parse_cpp(text: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("cpp language");
        parser.parse(text, None).expect("parse tree")
    }

    #[test]
    fn filters_candidates_using_clang_symbols() {
        let policy = NamingConventionsPolicy::new(true, true);
        let text = "int CamelVar = 0;\nint BadName() { return CamelVar; }\n";
        let tree = parse_cpp(text);
        let clang_parse_result = ClangParseResult::new(
            true,
            Vec::new(),
            vec![ClangSymbol {
                name: "DifferentName".to_string(),
                kind: ClangSymbolKind::Function,
                line: 2,
                column: 5,
                usr: None,
                scope_usr: None,
            }],
            ClangDiagnosticSummary::default(),
            Vec::new(),
        );
        let path = PathBuf::from("sample.cpp");
        let context = PolicyContext::new(text, &path)
            .with_tree_sitter_tree(Some(&tree))
            .with_clang_parse_result(Some(&clang_parse_result));
        let result = policy.apply(&context);
        assert!(result.violations.is_empty());
    }

    #[test]
    fn semantic_mode_applies_rename_edits() {
        let policy = NamingConventionsPolicy::new(true, true);
        let text = "int CamelVar = 0;\nint use_it() { return CamelVar; }\n";
        let tree = parse_cpp(text);
        let declaration_offset = text.find("CamelVar").expect("declaration offset");
        let reference_offset = text.rfind("CamelVar").expect("reference offset");
        let symbol = ClangSymbol {
            name: "CamelVar".to_string(),
            kind: ClangSymbolKind::Variable,
            line: 1,
            column: 5,
            usr: None,
            scope_usr: None,
        };
        let rename_offsets = HashMap::from([(
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
            .with_tree_sitter_tree(Some(&tree))
            .with_clang_parse_result(Some(&clang_parse_result));
        let result = policy.apply(&context);

        assert!(result.text.contains("_camel_var"));
        assert!(!result.text.contains("CamelVar"));
        assert_eq!(result.edits.len(), 2);
    }

    #[test]
    fn strict_semantic_mode_reports_conflict_without_fatal_abort() {
        let policy = NamingConventionsPolicy::new(true, true);
        let text = "int CamelVar = 0;\nint _camel_var = 1;\nint use_it() { return CamelVar + _camel_var; }\n";
        let tree = parse_cpp(text);
        let declaration_offset = text.find("CamelVar").expect("declaration offset");
        let reference_offset = text.rfind("CamelVar").expect("reference offset");

        let camel_symbol = ClangSymbol {
            name: "CamelVar".to_string(),
            kind: ClangSymbolKind::Variable,
            line: 1,
            column: 5,
            usr: None,
            scope_usr: None,
        };
        let existing_symbol = ClangSymbol {
            name: "_camel_var".to_string(),
            kind: ClangSymbolKind::Variable,
            line: 2,
            column: 5,
            usr: None,
            scope_usr: None,
        };
        let rename_offsets = HashMap::from([(
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
            .with_tree_sitter_tree(Some(&tree))
            .with_clang_parse_result(Some(&clang_parse_result));
        let result = policy.apply(&context);

        assert_eq!(result.text, text);
        assert!(result.edits.is_empty());
        assert!(result
            .warnings
            .iter()
            .any(|warning| warning.contains("strict semantic gate blocked")));
        assert!(!result
            .warnings
            .iter()
            .any(|warning| warning.starts_with("fatal:naming_conventions:")));
        assert!(result
            .violations
            .iter()
            .any(|violation| violation.message.contains("strict semantic gate blocked")));
    }

    #[test]
    fn strict_semantic_mode_does_not_fatal_on_clang_diagnostics_only() {
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
            .with_tree_sitter_tree(Some(&tree))
            .with_clang_parse_result(Some(&clang_parse_result));
        let result = policy.apply(&context);

        assert_eq!(result.text, text);
        assert!(result.edits.is_empty());
        assert!(!result
            .warnings
            .iter()
            .any(|warning| warning.starts_with("fatal:naming_conventions:")));
    }

    #[test]
    fn recoverable_clang_diagnostics_skip_semantic_renames() {
        let policy = NamingConventionsPolicy::new(true, true);
        let text = "int CamelVar = 0;\nint use_it() { return CamelVar; }\n";
        let tree = parse_cpp(text);
        let declaration_offset = text.find("CamelVar").expect("declaration offset");
        let reference_offset = text.rfind("CamelVar").expect("reference offset");
        let symbol = ClangSymbol {
            name: "CamelVar".to_string(),
            kind: ClangSymbolKind::Variable,
            line: 1,
            column: 5,
            usr: Some("usr:test:camelvar".to_string()),
            scope_usr: Some("usr:test:scope".to_string()),
        };
        let rename_offsets = HashMap::from([(
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
            .with_tree_sitter_tree(Some(&tree))
            .with_clang_parse_result(Some(&clang_parse_result));
        let result = policy.apply(&context);

        assert_eq!(result.text, text);
        assert!(result.edits.is_empty());
        assert!(result.warnings.iter().any(|warning| {
            warning.contains("semantic rename skipped due insufficient semantic parse reliability")
        }));
    }

    #[test]
    fn constructor_names_are_not_rewritten_to_snake_case() {
        let policy = NamingConventionsPolicy::new(true, true);
        let text = "struct Node {\n  Node() {}\n};\n";
        let tree = parse_cpp(text);
        let clang_parse_result = ClangParseResult::new(
            true,
            Vec::new(),
            vec![ClangSymbol {
                name: "Node".to_string(),
                kind: ClangSymbolKind::Constructor,
                line: 2,
                column: 3,
                usr: None,
                scope_usr: None,
            }],
            ClangDiagnosticSummary::default(),
            Vec::new(),
        );
        let path = PathBuf::from("sample.cpp");
        let context = PolicyContext::new(text, &path)
            .with_tree_sitter_tree(Some(&tree))
            .with_clang_parse_result(Some(&clang_parse_result));
        let result = policy.apply(&context);

        assert!(result
            .violations
            .iter()
            .all(|violation| !violation.message.contains("function 'Node'")));
        assert_eq!(result.text, text);
    }

    #[test]
    fn parameter_identifiers_are_not_forced_to_local_prefix() {
        let policy = NamingConventionsPolicy::new(false, false);
        let text = "int compute(int other) { return other; }\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let context = PolicyContext::new(text, &path).with_tree_sitter_tree(Some(&tree));
        let result = policy.apply(&context);

        assert!(result
            .violations
            .iter()
            .all(|violation| !violation.message.contains("identifier 'other'")));
        assert_eq!(result.text, text);
    }

    #[test]
    fn constant_style_identifiers_are_not_rewritten() {
        let policy = NamingConventionsPolicy::new(false, false);
        let text = "struct A {\n  static constexpr size_t C_LEVEL_SHIFT = 6UL;\n};\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let context = PolicyContext::new(text, &path).with_tree_sitter_tree(Some(&tree));
        let result = policy.apply(&context);

        assert!(result
            .violations
            .iter()
            .all(|violation| !violation.message.contains("C_LEVEL_SHIFT")));
        assert_eq!(result.text, text);
    }

    #[test]
    fn for_loop_header_variable_skips_prefix() {
        let policy = NamingConventionsPolicy::new(false, false);
        let text = "void f() {\n  for (int i = 0; i < 10; i++) {}\n}\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let context = PolicyContext::new(text, &path).with_tree_sitter_tree(Some(&tree));
        let result = policy.apply(&context);
        assert!(
            result.violations.iter().all(|v| !v.message.contains("'i'")),
            "for-loop header variable 'i' should not require prefix"
        );
    }

    #[test]
    fn for_range_loop_header_variable_skips_prefix() {
        let policy = NamingConventionsPolicy::new(false, false);
        let text = "void f() {\n  int arr[3] = {1,2,3};\n  for (auto val : arr) {}\n}\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let context = PolicyContext::new(text, &path).with_tree_sitter_tree(Some(&tree));
        let result = policy.apply(&context);
        assert!(
            result.violations.iter().all(|v| !v.message.contains("'val'")),
            "for-range-loop header variable 'val' should not require prefix"
        );
    }

    #[test]
    fn for_loop_body_variable_requires_prefix() {
        let policy = NamingConventionsPolicy::new(false, false);
        let text = "void f() {\n  for (int _i = 0; _i < 10; _i++) {\n    int count = 0;\n  }\n}\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let context = PolicyContext::new(text, &path).with_tree_sitter_tree(Some(&tree));
        let result = policy.apply(&context);
        assert!(
            result.violations.iter().any(|v| v.message.contains("'count'")),
            "variable declared inside for-loop body should require prefix"
        );
    }

    #[test]
    fn include_only_translation_unit_skips_reliability_warning() {
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
            .with_tree_sitter_tree(Some(&tree))
            .with_clang_parse_result(Some(&clang_parse_result));
        let result = policy.apply(&context);
        assert!(result.edits.is_empty());
        assert!(!result.warnings.iter().any(|item| {
            item.contains("semantic rename skipped due insufficient semantic parse reliability")
        }));
    }

    #[test]
    fn field_declaration_skipped_for_safety() {
        let policy = NamingConventionsPolicy::new(false, false);
        let text = "struct Foo {\n  int count;\n};\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.hpp");
        let context = PolicyContext::new(text, &path).with_tree_sitter_tree(Some(&tree));
        let result = policy.apply(&context);
        assert!(
            !result.violations.iter().any(|v| v.message.contains("'count'")),
            "field declarations should be skipped (member access rename can't propagate safely)"
        );
    }

    #[test]
    fn local_declaration_gets_local_prefix() {
        let policy = NamingConventionsPolicy::new(false, false);
        let text = "void f() {\n  int count = 0;\n}\n";
        let tree = parse_cpp(text);
        let path = PathBuf::from("sample.cpp");
        let context = PolicyContext::new(text, &path).with_tree_sitter_tree(Some(&tree));
        let result = policy.apply(&context);
        assert!(
            result.violations.iter().any(|v| v.message.contains("'_count'")),
            "local declaration should suggest local prefix '_', got: {:?}",
            result.violations
        );
    }
}
