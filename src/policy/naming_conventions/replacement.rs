use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;
use std::path::Path;

use crate::model::context_query::SemanticContextQuery;
use crate::model::edit::Edit;
use crate::parser::file_context::SemanticFileContext;
use crate::parser::text_scan;
use crate::policy::Policy;

use super::{NamingConventionsPolicy, RenameDiagnostics, RenamePlan, Replacement, StrictIssues};

impl RenamePlan {
    pub(super) fn rename_confidence(&self) -> f64 {
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

impl StrictIssues {
    pub(super) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            count: 0,
            first: None,
        }
    }

    pub(super) fn push(&mut self, message: String) {
        if !self.enabled {
            return;
        }
        self.count += 1;
        if self.first.is_none() {
            self.first = Some(message);
        }
    }

    pub(super) fn push_lazy<F>(&mut self, produce: F)
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

    pub(super) fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub(super) fn len(&self) -> usize {
        self.count
    }

    pub(super) fn first(&self) -> Option<&str> {
        self.first.as_deref()
    }
}

impl NamingConventionsPolicy {
    pub(super) fn has_identifier_boundaries(text: &str, start: usize, end: usize) -> bool {
        let bytes = text.as_bytes();
        if start > 0 && text_scan::is_identifier_byte(bytes[start - 1]) {
            return false;
        }
        if end < bytes.len() && text_scan::is_identifier_byte(bytes[end]) {
            return false;
        }
        true
    }

    pub(super) fn line_and_column_for_offset(line_starts: &[usize], offset: usize) -> (usize, usize) {
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

    pub(super) fn offset_for_line_column(line_starts: &[usize], line: usize, column: usize) -> Option<usize> {
        if line == 0 || column == 0 {
            return None;
        }
        let line_start = line_starts.get(line.saturating_sub(1)).copied()?;
        Some(line_start.saturating_add(column.saturating_sub(1)))
    }

    pub(super) fn find_uncovered_field_initializer_label_offset(
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

    #[allow(clippy::too_many_arguments)]
    pub(super) fn resolve_rename_plan(
        semantic_query: &SemanticContextQuery<'_>,
        semantic: &SemanticFileContext,
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
            if declaration.is_anonymous {
                return false;
            }
            stable_id = Some(declaration.stable_id.clone());
            let references = semantic_query.references_of(declaration.stable_id.as_str());
            let safe_reference_count = references
                .iter()
                .filter(|reference| semantic_query.is_safe_edit(reference.line, reference.column))
                .count();
            expected_occurrences = references.len().max(1);
            minimum_required_occurrences = safe_reference_count.max(1);
        }
        if semantic.has_name_elsewhere(&suggested, line) {
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

    pub(super) fn normalize_decl_path(path: &Path) -> String {
        path.to_string_lossy()
            .to_string()
    }

    pub(super) fn build_replacements(
        &self,
        text: &str,
        semantic: &SemanticFileContext,
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
                let clang_offsets = semantic.rename_offsets(
                    &plan.old_name,
                    plan.line,
                    std::slice::from_ref(&plan.kind),
                );
                if clang_offsets.is_empty() {
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

    pub(super) fn apply_semantic_renames(
        &self,
        text: &str,
        semantic: &SemanticFileContext,
        plans: &[RenamePlan],
        semantic_query: &SemanticContextQuery<'_>,
        non_code_ranges: &[(usize, usize)],
        diag: &mut RenameDiagnostics<'_>,
    ) -> (String, Vec<Edit>) {
        let mut replacements = self.build_replacements(
            text,
            semantic,
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
