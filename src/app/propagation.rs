use rustc_hash::FxHashMap;
use std::path::Path;
use std::path::PathBuf;

use std::sync::Arc;

use rayon::prelude::*;

use crate::app::runner::{App, SemanticPropagationOutcome};
use crate::files::file_io::FileIo;
use crate::model::edit::Edit;
use crate::model::file_result::FileResult;
use crate::parser::clang_types::ClangDeclKey;
use crate::parser::manager::{ParserManager, SemanticCompdbContextKind};
use crate::parser::text_scan;

impl App {
    pub(crate) fn apply_project_wide_semantic_renames(
        file_io: &FileIo,
        parser_manager: &ParserManager,
        results: &mut [FileResult],
        parallel_pool: Option<&rayon::ThreadPool>,
        population_edit_success: f64,
    ) {
        let mut rename_by_decl: FxHashMap<ClangDeclKey, (String, String)> = FxHashMap::default();
        let mut plan_conflict = None::<String>;
        for result in results.iter() {
            if result.error.is_some() {
                continue;
            }
            for plan in &result.outcome.rename_plans {
                let next = (plan.old_name.clone(), plan.new_name.clone());
                if let Some(existing) = rename_by_decl.get(&plan.decl) {
                    if existing != &next {
                        plan_conflict = Some(format!(
                            "conflicting project-wide semantic plans for {}:{}:{}",
                            plan.decl.path, plan.decl.line, plan.decl.column
                        ));
                        break;
                    }
                } else {
                    rename_by_decl.insert(plan.decl.clone(), next);
                }
            }
            if plan_conflict.is_some() {
                break;
            }
        }

        if let Some(conflict) = plan_conflict {
            for result in results.iter_mut() {
                if result.error.is_none() && !result.outcome.rename_plans.is_empty() {
                    result.error = Some(conflict.clone());
                    result.outcome.changed = false;
                    result.outcome.pending_text = None;
                }
            }
            return;
        }
        if rename_by_decl.is_empty() {
            return;
        }

        // Population-level Kalman filtering: compute per-declaration agreement
        // across files and filter renames that lack cross-file consensus weighted
        // by population edit_success trust.
        let willingness = 1.0 / (1.0 + (-5.0_f64 * (population_edit_success - 0.5)).exp());
        let population_edit_bar = 1.0 - willingness;
        let mut decl_proposer_count: FxHashMap<ClangDeclKey, usize> = FxHashMap::default();
        for result in results.iter() {
            if result.error.is_some() {
                continue;
            }
            for plan in &result.outcome.rename_plans {
                *decl_proposer_count.entry(plan.decl.clone()).or_insert(0) += 1;
            }
        }
        let total_files_with_plans = results
            .iter()
            .filter(|r| r.error.is_none() && !r.outcome.rename_plans.is_empty())
            .count()
            .max(1);
        rename_by_decl.retain(|decl, _| {
            let proposers = decl_proposer_count.get(decl).copied().unwrap_or(0);
            let agreement = proposers as f64 / total_files_with_plans as f64;
            let score = agreement * population_edit_success;
            score >= population_edit_bar
        });
        if rename_by_decl.is_empty() {
            return;
        }

        let targets = results
            .iter()
            .enumerate()
            .filter(|(_, result)| Self::should_process_semantic_propagation_result(result))
            .map(|(index, result)| (index, result.meta.path.clone(), result.outcome.pending_text.clone()))
            .collect::<Vec<_>>();
        if targets.is_empty() {
            return;
        }

        let rename_by_decl = Arc::new(rename_by_decl);
        let parser_for_rename = Arc::new(parser_manager.clone());
        let file_io_for_rename = Arc::new(file_io.clone());
        let outcomes: Vec<_> = if let Some(pool) = parallel_pool {
            let rename_by_decl = rename_by_decl.clone();
            let parser = parser_for_rename.clone();
            let file_io = file_io_for_rename.clone();
            pool.install(|| {
                targets
                    .into_par_iter()
                    .map(|(index, path, pending_text)| {
                        Self::apply_semantic_rename_target(
                            index,
                            path,
                            pending_text,
                            &rename_by_decl,
                            &parser,
                            &file_io,
                        )
                    })
                    .collect()
            })
        } else {
            targets
                .into_iter()
                .map(|(index, path, pending_text)| {
                    Self::apply_semantic_rename_target(
                        index,
                        path,
                        pending_text,
                        &rename_by_decl,
                        &parser_for_rename,
                        &file_io_for_rename,
                    )
                })
                .collect()
        };

        for outcome in outcomes {
            let Some(result) = results.get_mut(outcome.index) else {
                continue;
            };
            if result.error.is_some() {
                continue;
            }
            if let Some(error) = outcome.error {
                result.error = Some(error);
                result.outcome.changed = false;
                result.outcome.pending_text = None;
                continue;
            }
            if outcome.edits.is_empty() || outcome.pending_text.is_none() {
                continue;
            }
            let Some(text) = outcome.pending_text else {
                continue;
            };
            if result
                .outcome
                .pending_text
                .as_ref()
                .is_none_or(|existing| existing != &text)
            {
                result.outcome.pending_text = Some(text);
                result.outcome.changed = true;
                if let Some(warning) = outcome.warning {
                    result.warnings.push(warning);
                }
            }
            result.outcome.edits.extend(outcome.edits);
        }
    }

    fn apply_semantic_rename_target(
        index: usize,
        path: PathBuf,
        pending_text: Option<String>,
        rename_by_decl: &FxHashMap<ClangDeclKey, (String, String)>,
        parser_manager: &ParserManager,
        file_io: &FileIo,
    ) -> SemanticPropagationOutcome {
        if Self::should_skip_project_wide_semantic_propagation(parser_manager, path.as_path()) {
            return SemanticPropagationOutcome {
                index,
                pending_text: None,
                edits: Vec::new(),
                warning: Some(format!(
                    "naming_conventions: skipped project-wide semantic propagation for {} because no compile_commands context is available",
                    path.display()
                )),
                error: None,
            };
        }

        let mut text = if let Some(pending) = pending_text {
            pending
        } else {
            match file_io.read_text(&path) {
                Ok(content) => content,
                Err(err) => {
                    return SemanticPropagationOutcome {
                        index,
                        pending_text: None,
                        edits: Vec::new(),
                        warning: None,
                        error: Some(format!("read failed for semantic propagation: {err}")),
                    };
                }
            }
        };

        let parse_result = match parser_manager.parse_clang(text.as_str(), &path) {
            Ok(value) => value,
            Err(err) => {
                let message = err.to_string();
                if message.contains("semantic parse fidelity requires compile_commands entry") {
                    return SemanticPropagationOutcome {
                        index,
                        pending_text: None,
                        edits: Vec::new(),
                        warning: Some(format!(
                            "naming_conventions: skipped project-wide semantic propagation for {} because compile_commands entry is missing",
                            path.display()
                        )),
                        error: None,
                    };
                }
                return SemanticPropagationOutcome {
                    index,
                    pending_text: None,
                    edits: Vec::new(),
                    warning: None,
                    error: Some(format!("semantic propagation parse failed: {message}")),
                };
            }
        };

        let line_starts = Self::line_starts(text.as_str());
        let mut replacements = Vec::<(usize, usize, usize, String, String)>::new();
        for (decl, (old_name, new_name)) in rename_by_decl {
            let offsets = parse_result.ref_offsets(decl);
            for offset in offsets {
                let end = offset.saturating_add(old_name.len());
                if end > text.len() || !text.is_char_boundary(offset) || !text.is_char_boundary(end)
                {
                    continue;
                }
                let Some(current) = text.get(offset..end) else {
                    continue;
                };
                if current == new_name {
                    continue;
                }
                if current != old_name {
                    continue;
                }
                if !Self::has_identifier_boundaries(text.as_bytes(), offset, end) {
                    continue;
                }
                let line = Self::line_for_offset(&line_starts, offset);
                replacements.push((offset, end, line, old_name.clone(), new_name.clone()));
            }
        }
        if replacements.is_empty() {
            return SemanticPropagationOutcome {
                index,
                pending_text: None,
                edits: Vec::new(),
                warning: None,
                error: None,
            };
        }

        replacements.sort_by_key(|(start, _, _, _, _)| *start);
        let mut deduped = Vec::with_capacity(replacements.len());
        let mut last_end = 0usize;
        for replacement in replacements {
            if !deduped.is_empty() && replacement.0 < last_end {
                continue;
            }
            last_end = replacement.1;
            deduped.push(replacement);
        }

        let mut edits = Vec::with_capacity(deduped.len());
        for (start, end, line, old_name, new_name) in deduped.iter().rev() {
            text.replace_range(*start..*end, new_name.as_str());
            edits.push(Edit {
                policy: "naming_conventions".into(),
                line: *line,
                before: old_name.clone(),
                after: new_name.clone(),
            });
        }

        SemanticPropagationOutcome {
            index,
            pending_text: Some(text),
            edits,
            warning: Some(
                "naming_conventions: applied project-wide semantic reference updates".to_string(),
            ),
            error: None,
        }
    }

    fn should_skip_project_wide_semantic_propagation(
        parser_manager: &ParserManager,
        path: &Path,
    ) -> bool {
        let exact = parser_manager.has_exact_compdb(path);
        if exact {
            return false;
        }
        let context_kind = parser_manager.semantic_compdb_kind(path);
        Self::should_skip_project_wide_semantic_propagation_for_context(context_kind)
    }

    #[cfg(test)]
    fn should_skip_project_wide_semantic_propagation_for_exact_compdb(exact_compdb: bool) -> bool {
        !exact_compdb
    }

    fn should_skip_project_wide_semantic_propagation_for_context(
        context_kind: SemanticCompdbContextKind,
    ) -> bool {
        matches!(context_kind, SemanticCompdbContextKind::None)
    }

    fn should_process_semantic_propagation_result(result: &FileResult) -> bool {
        result.error.is_none() && !result.outcome.rename_plans.is_empty()
    }

    fn line_starts(text: &str) -> Vec<usize> {
        text_scan::line_starts(text, false)
    }

    fn line_for_offset(line_starts: &[usize], offset: usize) -> usize {
        match line_starts.binary_search(&offset) {
            Ok(index) => index + 1,
            Err(0) => 1,
            Err(index) => index,
        }
    }

    fn has_identifier_boundaries(bytes: &[u8], start: usize, end: usize) -> bool {
        let is_ident = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
        if start > 0 && is_ident(bytes[start - 1]) {
            return false;
        }
        if end < bytes.len() && is_ident(bytes[end]) {
            return false;
        }
        true
    }

    pub(crate) fn apply_write_phase(file_io: &FileIo, results: &mut [FileResult], parallel_pool: Option<&rayon::ThreadPool>) {
        let write_fn = |result: &mut FileResult| {
            if result.error.is_some() {
                result.outcome.pending_text = None;
                result.outcome.changed = false;
                return;
            }
            if !result.outcome.changed {
                result.outcome.pending_text = None;
                return;
            }
            let Some(text) = result.outcome.pending_text.take() else {
                result.outcome.changed = false;
                return;
            };
            match file_io.write_text(result.meta.path.as_path(), text.as_str()) {
                Ok(backup_path) => {
                    result.meta.backup_path = backup_path;
                }
                Err(err) => {
                    result.outcome.changed = false;
                    result.error = Some(format!("write failed: {err}"));
                }
            }
        };
        if let Some(pool) = parallel_pool {
            pool.install(|| results.par_iter_mut().for_each(write_fn));
        } else {
            results.iter_mut().for_each(write_fn);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::App;
    use crate::model::file_result::FileResult;
    use crate::model::rename_plan::SemanticRenamePlan;
    use crate::parser::clang_types::ClangDeclKey;
    use crate::parser::manager::SemanticCompdbContextKind;

    #[test]
    fn propagation_requires_compdb() {
        assert!(App::should_skip_project_wide_semantic_propagation_for_exact_compdb(false));
        assert!(!App::should_skip_project_wide_semantic_propagation_for_exact_compdb(true));
    }

    #[test]
    fn propagation_allows_heuristic() {
        assert!(!App::should_skip_project_wide_semantic_propagation_for_context(
            SemanticCompdbContextKind::Exact,
        ));
        assert!(!App::should_skip_project_wide_semantic_propagation_for_context(
            SemanticCompdbContextKind::PairedSourceHeuristic,
        ));
        assert!(!App::should_skip_project_wide_semantic_propagation_for_context(
            SemanticCompdbContextKind::HeaderConsensus,
        ));
        assert!(!App::should_skip_project_wide_semantic_propagation_for_context(
            SemanticCompdbContextKind::SourceConsensus,
        ));
        assert!(App::should_skip_project_wide_semantic_propagation_for_context(
            SemanticCompdbContextKind::None,
        ));
    }

    #[test]
    fn propagation_targets_renames() {
        let mut result = FileResult::default();
        assert!(!App::should_process_semantic_propagation_result(&result));

        result.outcome.rename_plans.push(SemanticRenamePlan {
            decl: ClangDeclKey::new(
                "/tmp/sample.cpp".to_string(),
                7,
                3,
                clang_sys::CXCursor_FunctionDecl,
            ),
            old_name: "BadName".to_string(),
            new_name: "bad_name".to_string(),
        });
        assert!(App::should_process_semantic_propagation_result(&result));

        result.error = Some("failed".to_string());
        assert!(!App::should_process_semantic_propagation_result(&result));
    }
}
