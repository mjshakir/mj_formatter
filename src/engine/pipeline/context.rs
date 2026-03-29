use std::collections::BTreeSet;
use std::path::Path;

use tracing::debug;
use tree_sitter::{StreamingIterator, Tree};

use crate::engine::context_tracker::{block_context_index, FileContextKind};
use crate::model::edit::Edit;
use crate::parser::manager::SemanticCompdbContextKind;
use crate::parser::query_cache::TsQueryCache;

use super::{PolicyPipeline, PipelineState};

impl PolicyPipeline {
    pub(super) fn context_modifier_for_policy(&self, state: &PipelineState<'_>, policy_name: &str, block_index: Option<usize>) -> f64 {
        let policy_idx = crate::engine::context_tracker::policy_index(policy_name)
            .unwrap_or(u8::MAX);
        if (policy_idx as usize) < 22 {
            let file_mod = state.telem.context_mods[policy_idx as usize] as f64;
            let block_mod = match block_index {
                Some(bi) => state.telem.block_mods[bi][policy_idx as usize] as f64,
                None => 1.0,
            };
            file_mod * block_mod
        } else {
            1.0
        }
    }

    pub(super) fn batch_context_modifiers(&self, path: &Path) -> [f32; 24] {
        let file_kind = FileContextKind::from_path(path);
        self.context_tracker.load().batch_file_modifiers(file_kind)
    }

    pub(super) fn batch_block_mods(&self) -> [[f32; 24]; 6] {
        let guard = self.context_tracker.load();
        let mut result = [[1.0f32; 24]; 6];
        for i in 0..6 {
            result[i] = guard.batch_block_modifiers(i);
        }
        result
    }

    pub(super) fn record_initial_fidelity_warnings(&self, state: &mut PipelineState<'_>) {
        debug!(
            fidelity = state.parse.fidelity_score,
            path = %state.path.display(),
            "semantic fidelity"
        );
        if state.parse.exact_compdb {
            return;
        }
        let detail = match state.parse.semantic_kind {
            SemanticCompdbContextKind::PairedSourceHeuristic => {
                "using paired-source heuristic semantic context"
            }
            SemanticCompdbContextKind::HeaderConsensus => "using multi-TU header consensus context",
            SemanticCompdbContextKind::SourceConsensus => {
                "using compdb-derived source consensus context"
            }
            SemanticCompdbContextKind::Exact | SemanticCompdbContextKind::None => {
                "using compdb-derived semantic context"
            }
        };
        debug!(
            path = %state.path.display(),
            context = detail,
            "semantic fidelity lock: no exact compile_commands entry"
        );
    }

    pub(super) fn dominant_block_index(
        state: &PipelineState<'_>,
        edits: &[Edit],
    ) -> usize {
        if edits.is_empty() {
            return 4;
        }
        let semantic = match &state.parse.semantic {
            Some(ctx) => ctx,
            None => return 4,
        };
        let mut counts = [0u32; 6];
        for edit in edits {
            if edit.line == 0 {
                counts[4] += 1;
                continue;
            }
            let idx = semantic
                .scope_at_location(crate::parser::file_context::SourceLocation::new(edit.line, 1))
                .map(|scope| block_context_index(scope.node_kind_id))
                .unwrap_or(4);
            counts[idx] += 1;
        }
        counts
            .iter()
            .enumerate()
            .max_by_key(|(_, &c)| c)
            .map(|(i, _)| i)
            .unwrap_or(4)
    }

    pub(super) fn comment_lines_from_tree(
        tree: Option<&Tree>,
        source: &[u8],
        query_cache: &TsQueryCache,
    ) -> BTreeSet<usize> {
        let Some(tree) = tree else {
            return BTreeSet::new();
        };
        let Ok(query) = query_cache.get_or_compile("(comment) @c") else {
            return BTreeSet::new();
        };
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), source);
        let mut lines = BTreeSet::<usize>::new();
        while let Some(m) = {
            matches.advance();
            matches.get()
        } {
            for capture in m.captures {
                let start = capture.node.start_position().row.saturating_add(1);
                let end = capture.node.end_position().row.saturating_add(1).max(start);
                for line in start..=end {
                    lines.insert(line);
                }
            }
        }
        lines
    }

    #[cfg(test)]
    pub(super) fn needs_exact_compdb(policy_name: &str) -> bool {
        use crate::engine::catalog::policy_catalog;
        policy_catalog()
            .behavior(policy_name)
            .needs_exact_compdb
    }
}
