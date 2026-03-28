use rustc_hash::{FxHashMap, FxHashSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::parser::clang_types::ClangDeclKey;
use crate::parser::clang_result::ClangParseResult;
use crate::graph::types::GraphEdge;
use crate::graph::types::GraphEdgeKind;
use crate::graph::types::GraphNode;
use crate::graph::types::GraphNodeKind;
use crate::graph::types::NodeMetrics;
use crate::graph::state::ProjectGraphState;
use crate::graph::symbol_bucket::{file_symbol_id, legacy_id, ToSymbolId};
use crate::graph::symbol_id::SymbolId;

pub struct GraphUpdater;

impl GraphUpdater {
    pub fn apply_clang_parse(state: &mut ProjectGraphState, path: &Path, parse: &ClangParseResult) {
        let now = current_unix_ms();
        let path_string = path.to_string_lossy().to_string();
        let canonical_path = std::fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .to_string();

        let file_id = file_symbol_id(path);
        let mut file_node = GraphNode::new(
            file_id.clone(),
            path_string.clone(),
            GraphNodeKind::File,
            path_string.clone(),
            0,
            0,
        );
        file_node.last_seen_unix_ms = now;
        file_node.parser_consensus = 1.0;
        state.upsert_node(file_node);

        let symbol_consensus = if parse.success { 1.0 } else { 0.5 };
        let mut seen_symbols_for_file: FxHashSet<SymbolId> = FxHashSet::default();
        let mut declaration_symbol_ids: FxHashMap<ClangDeclKey, SymbolId> = FxHashMap::default();
        let mut alias_map: FxHashMap<SymbolId, SymbolId> = FxHashMap::default();

        for symbol in &parse.symbols {
            let symbol_id = symbol.symbol_id();
            let legacy_symbol_id = legacy_id(symbol);
            let decl_key = ClangDeclKey::new(
                canonical_path.clone(),
                symbol.line,
                symbol.column,
                symbol.kind,
            );
            let decl_symbol_id = decl_key.symbol_id();
            if decl_symbol_id != symbol_id {
                alias_map.insert(decl_symbol_id, symbol_id.clone());
            }
            declaration_symbol_ids.insert(decl_key, symbol_id.clone());
            seen_symbols_for_file.insert(symbol_id.clone());

            let mut node = state.node(&symbol_id).cloned().unwrap_or_else(|| {
                if symbol_id != legacy_symbol_id {
                    if let Some(mut legacy_node) = state.node(&legacy_symbol_id).cloned() {
                        legacy_node.symbol_id = symbol_id.clone();
                        return legacy_node;
                    }
                }
                GraphNode::new(
                    symbol_id.clone(),
                    symbol.name.clone(),
                    GraphNodeKind::from(symbol.kind),
                    path_string.clone(),
                    symbol.line,
                    symbol.column,
                )
            });
            node.kind = GraphNodeKind::from(symbol.kind);
            node.name = symbol.name.clone();
            node.file_path = path_string.clone();
            node.line = symbol.line;
            node.column = symbol.column;
            if symbol.usr.is_some() {
                node.scope_symbol_id = symbol.scope_usr.clone().map(SymbolId::new);
            }
            node.last_seen_unix_ms = now;
            node.parser_consensus = if node.parser_consensus <= 0.0 {
                symbol_consensus
            } else {
                ((0.85 * node.parser_consensus) + (0.15 * symbol_consensus)).clamp(0.0, 1.0)
            };
            state.upsert_node(node);

            let mut metrics = state.node_metrics(&symbol_id).cloned().unwrap_or_default();
            if symbol_id != legacy_symbol_id && metrics.reference_count == 0 {
                if let Some(legacy_metrics) = state.node_metrics(&legacy_symbol_id) {
                    metrics.reference_count = legacy_metrics.reference_count;
                    metrics.file_count = legacy_metrics.file_count;
                    metrics.consensus_score = legacy_metrics.consensus_score;
                }
            }
            metrics.consensus_score = if metrics.consensus_score <= 0.0 {
                symbol_consensus
            } else {
                ((0.90 * metrics.consensus_score) + (0.10 * symbol_consensus)).clamp(0.0, 1.0)
            };
            metrics.last_updated_unix_ms = now;
            state.set_metrics(symbol_id, metrics);
        }

        Self::apply_aliases(state, &alias_map, now);

        let mut reference_targets: FxHashMap<SymbolId, u32> = FxHashMap::default();
        for (decl_key, offsets) in parse.reference_offsets_map() {
            if offsets.is_empty() {
                continue;
            }
            let symbol_id = declaration_symbol_ids
                .get(decl_key)
                .cloned()
                .unwrap_or_else(|| decl_key.symbol_id());
            Self::ensure_declaration_node(state, &symbol_id, decl_key, now, symbol_consensus);
            let weight = offsets.len().min(u32::MAX as usize) as u32;
            *reference_targets.entry(symbol_id).or_insert(0) += weight.max(1);
        }

        let mut affected_symbols =
            Self::replace_file_contains_edges(state, &file_id, &seen_symbols_for_file, now);
        let removed_reference_targets =
            Self::replace_file_reference_edges(state, &file_id, &reference_targets, now);
        affected_symbols.extend(removed_reference_targets);
        affected_symbols.extend(seen_symbols_for_file.clone());
        affected_symbols.extend(reference_targets.keys().cloned());
        let file_counts = Self::contains_counts_for_symbols(state, &affected_symbols);
        let reference_counts = Self::reference_counts_for_symbols(state, &affected_symbols);

        for symbol_id in affected_symbols {
            let file_count = file_counts.get(&symbol_id).copied().unwrap_or(0);
            let mut metrics = state.node_metrics(&symbol_id).cloned().unwrap_or_default();
            metrics.file_count = file_count;
            metrics.reference_count = reference_counts.get(&symbol_id).copied().unwrap_or(0);
            if metrics.reference_count == 0 && seen_symbols_for_file.contains(&symbol_id) {
                metrics.reference_count = 1;
            }
            metrics.last_updated_unix_ms = now;
            state.set_metrics(symbol_id, metrics);
        }
    }

    fn replace_file_contains_edges(
        state: &mut ProjectGraphState,
        file_id: &SymbolId,
        symbol_ids: &FxHashSet<SymbolId>,
        now: u64,
    ) -> FxHashSet<SymbolId> {
        let mut removed: FxHashSet<SymbolId> = FxHashSet::default();
        state.edges.retain(|edge| {
            let is_file_contains = edge.kind == GraphEdgeKind::Contains && edge.from == *file_id;
            if is_file_contains {
                removed.insert(edge.to.clone());
                false
            } else {
                true
            }
        });

        for symbol_id in symbol_ids {
            let mut edge =
                GraphEdge::new(file_id.clone(), symbol_id.clone(), GraphEdgeKind::Contains);
            edge.last_seen_unix_ms = now;
            edge.weight = 1;
            state.upsert_edge(edge);
        }

        removed
    }

    fn replace_file_reference_edges(
        state: &mut ProjectGraphState,
        file_id: &SymbolId,
        targets: &FxHashMap<SymbolId, u32>,
        now: u64,
    ) -> FxHashSet<SymbolId> {
        let mut removed: FxHashSet<SymbolId> = FxHashSet::default();
        state.edges.retain(|edge| {
            let is_file_reference = edge.kind == GraphEdgeKind::Reference && edge.from == *file_id;
            if is_file_reference {
                removed.insert(edge.to.clone());
                false
            } else {
                true
            }
        });

        for (symbol_id, count) in targets {
            let mut edge =
                GraphEdge::new(file_id.clone(), symbol_id.clone(), GraphEdgeKind::Reference);
            edge.last_seen_unix_ms = now;
            edge.weight = (*count).max(1);
            state.upsert_edge(edge);
        }

        removed
    }

    fn reference_counts_for_symbols(
        state: &ProjectGraphState,
        symbol_ids: &FxHashSet<SymbolId>,
    ) -> FxHashMap<SymbolId, u64> {
        if symbol_ids.is_empty() {
            return FxHashMap::default();
        }

        let mut counts = FxHashMap::default();
        for edge in &state.edges {
            if edge.kind != GraphEdgeKind::Reference {
                continue;
            }
            if !symbol_ids.contains(&edge.to) {
                continue;
            }
            let count = counts.entry(edge.to.clone()).or_insert(0_u64);
            *count = (*count).saturating_add(edge.weight as u64);
        }
        counts
    }

    fn ensure_declaration_node(
        state: &mut ProjectGraphState,
        symbol_id: &SymbolId,
        decl_key: &ClangDeclKey,
        now: u64,
        symbol_consensus: f64,
    ) {
        if let Some(existing) = state.nodes.get_mut(symbol_id) {
            existing.last_seen_unix_ms = now;
            if existing.parser_consensus <= 0.0 {
                existing.parser_consensus = symbol_consensus;
            } else {
                existing.parser_consensus = ((0.90 * existing.parser_consensus)
                    + (0.10 * symbol_consensus))
                    .clamp(0.0, 1.0);
            }
            return;
        }

        let mut node = GraphNode::new(
            symbol_id.clone(),
            format!("{:?}@{}:{}", decl_key.kind, decl_key.line, decl_key.column),
            crate::parser::clang_types::graph_node_kind(decl_key.kind),
            decl_key.path.clone(),
            decl_key.line,
            decl_key.column,
        );
        node.last_seen_unix_ms = now;
        node.parser_consensus = symbol_consensus;
        state.upsert_node(node);
    }

    fn apply_aliases(
        state: &mut ProjectGraphState,
        aliases: &FxHashMap<SymbolId, SymbolId>,
        now: u64,
    ) {
        if aliases.is_empty() {
            return;
        }

        for (from_id, to_id) in aliases {
            if from_id == to_id {
                continue;
            }
            if let Some(from_node) = state.nodes.remove(from_id) {
                if let Some(existing) = state.nodes.get_mut(to_id) {
                    existing.last_seen_unix_ms =
                        existing.last_seen_unix_ms.max(from_node.last_seen_unix_ms);
                    existing.parser_consensus = existing
                        .parser_consensus
                        .max(from_node.parser_consensus)
                        .clamp(0.0, 1.0);
                } else {
                    let mut node = from_node;
                    node.symbol_id = to_id.clone();
                    node.last_seen_unix_ms = node.last_seen_unix_ms.max(now);
                    state.nodes.insert(to_id.clone(), node);
                }
            }

            if let Some(from_metrics) = state.metrics.remove(from_id) {
                let mut to_metrics = state
                    .metrics
                    .get(to_id)
                    .cloned()
                    .unwrap_or_else(NodeMetrics::default);
                to_metrics.reference_count = to_metrics
                    .reference_count
                    .saturating_add(from_metrics.reference_count);
                to_metrics.file_count = to_metrics.file_count.max(from_metrics.file_count);
                to_metrics.consensus_score = to_metrics
                    .consensus_score
                    .max(from_metrics.consensus_score)
                    .clamp(0.0, 1.0);
                to_metrics.last_updated_unix_ms = to_metrics
                    .last_updated_unix_ms
                    .max(from_metrics.last_updated_unix_ms)
                    .max(now);
                state.metrics.insert(to_id.clone(), to_metrics);
            }
        }

        for edge in &mut state.edges {
            if let Some(mapped) = aliases.get(&edge.from) {
                edge.from = mapped.clone();
            }
            if let Some(mapped) = aliases.get(&edge.to) {
                edge.to = mapped.clone();
            }
        }
        let existing_edges = std::mem::take(&mut state.edges);
        for edge in existing_edges {
            state.upsert_edge(edge);
        }
    }

    fn contains_counts_for_symbols(
        state: &ProjectGraphState,
        symbol_ids: &FxHashSet<SymbolId>,
    ) -> FxHashMap<SymbolId, u32> {
        if symbol_ids.is_empty() {
            return FxHashMap::default();
        }

        let mut counts = FxHashMap::default();
        for edge in &state.edges {
            if edge.kind != GraphEdgeKind::Contains {
                continue;
            }
            if !symbol_ids.contains(&edge.to) {
                continue;
            }
            let count = counts.entry(edge.to.clone()).or_insert(0_u32);
            *count = (*count).saturating_add(1);
        }
        counts
    }
}

fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rustc_hash::FxHashMap;
    use crate::parser::clang_types::ClangDeclKey;
    use crate::parser::clang_result::ClangParseResult;
    use crate::parser::file_context::SemanticDeclaration;
    use crate::graph::types::GraphEdgeKind;
    use crate::graph::state::ProjectGraphState;
    use crate::graph::state_updater::GraphUpdater;
    use crate::graph::symbol_bucket::ToSymbolId;
    use crate::graph::symbol_id::SymbolId;

    #[test]
    fn adds_symbol_metrics() {
        let path = PathBuf::from("src/demo.cpp");
        let parse = ClangParseResult::new(
            true,
            Vec::new(),
            vec![SemanticDeclaration {
                name: "DemoFn".to_string(),
                kind: clang_sys::CXCursor_FunctionDecl,
                line: 12,
                column: 4,
                ..Default::default()
            }],
            crate::parser::clang_result::ClangDiagnosticSummary::default(),
            Vec::new(),
        );
        let mut state = ProjectGraphState::new();
        GraphUpdater::apply_clang_parse(&mut state, &path, &parse);
        let symbol = SymbolId::new("bucket|FunctionDecl|DemoFn");
        assert!(state.node(&symbol).is_some());
        assert!(state.symbol_reference_count(&symbol) > 0);
    }

    #[test]
    fn replaces_file_edges() {
        let path = PathBuf::from("src/demo.cpp");
        let first = ClangParseResult::new(
            true,
            Vec::new(),
            vec![
                SemanticDeclaration {
                    name: "DemoFn".to_string(),
                    kind: clang_sys::CXCursor_FunctionDecl,
                    line: 12,
                    column: 4,
                    ..Default::default()
                },
                SemanticDeclaration {
                    name: "OldFn".to_string(),
                    kind: clang_sys::CXCursor_FunctionDecl,
                    line: 24,
                    column: 3,
                    ..Default::default()
                },
            ],
            crate::parser::clang_result::ClangDiagnosticSummary::default(),
            Vec::new(),
        );
        let second = ClangParseResult::new(
            true,
            Vec::new(),
            vec![SemanticDeclaration {
                name: "DemoFn".to_string(),
                kind: clang_sys::CXCursor_FunctionDecl,
                line: 12,
                column: 4,
                ..Default::default()
            }],
            crate::parser::clang_result::ClangDiagnosticSummary::default(),
            Vec::new(),
        );

        let mut state = ProjectGraphState::new();
        GraphUpdater::apply_clang_parse(&mut state, &path, &first);
        GraphUpdater::apply_clang_parse(&mut state, &path, &second);

        let file_id = SymbolId::new("file|src/demo.cpp");
        let demo = SymbolId::new("bucket|FunctionDecl|DemoFn");
        let old = SymbolId::new("bucket|FunctionDecl|OldFn");
        let contains_from_file = state
            .edges
            .iter()
            .filter(|edge| edge.kind == GraphEdgeKind::Contains && edge.from == file_id)
            .collect::<Vec<_>>();

        assert_eq!(contains_from_file.len(), 1);
        assert_eq!(contains_from_file[0].to, demo);
        assert_eq!(
            state.symbol_file_count(&SymbolId::new("bucket|FunctionDecl|DemoFn")),
            1
        );
        assert_eq!(state.symbol_file_count(&old), 0);
    }

    #[test]
    fn builds_reference_edges() {
        let decl_path = PathBuf::from("src/ref_target.cpp");
        let use_path = PathBuf::from("src/ref_user.cpp");
        let canonical_decl = std::fs::canonicalize(&decl_path)
            .unwrap_or_else(|_| decl_path.clone())
            .to_string_lossy()
            .to_string();
        let decl_key = ClangDeclKey::new(canonical_decl, 10, 3, clang_sys::CXCursor_FunctionDecl);

        let mut references = FxHashMap::<ClangDeclKey, Vec<usize>>::default();
        references.insert(decl_key.clone(), vec![5, 17]);
        let use_parse = ClangParseResult::with_semantic_offsets(
            true,
            Vec::new(),
            Vec::new(),
            FxHashMap::default(),
            references,
            crate::parser::clang_result::ClangDiagnosticSummary::default(),
            Vec::new(),
        );

        let decl_parse = ClangParseResult::new(
            true,
            Vec::new(),
            vec![SemanticDeclaration {
                name: "TargetFn".to_string(),
                kind: clang_sys::CXCursor_FunctionDecl,
                line: 10,
                column: 3,
                ..Default::default()
            }],
            crate::parser::clang_result::ClangDiagnosticSummary::default(),
            Vec::new(),
        );

        let mut state = ProjectGraphState::new();
        GraphUpdater::apply_clang_parse(&mut state, &use_path, &use_parse);

        let decl_symbol = decl_key.symbol_id();
        assert!(state.node(&decl_symbol).is_some());
        assert_eq!(state.symbol_reference_count(&decl_symbol), 2);

        GraphUpdater::apply_clang_parse(&mut state, &decl_path, &decl_parse);

        let canonical_symbol = SymbolId::new("bucket|FunctionDecl|TargetFn");
        assert!(state.node(&canonical_symbol).is_some());
        assert_eq!(state.symbol_reference_count(&canonical_symbol), 2);
        assert!(state
            .edges
            .iter()
            .any(|edge| edge.kind == GraphEdgeKind::Reference && edge.to == canonical_symbol));
    }

    #[test]
    fn replaces_same_file() {
        let use_path = PathBuf::from("src/ref_refresh.cpp");
        let key_a = ClangDeclKey::new("src/a.hpp".to_string(), 1, 1, clang_sys::CXCursor_FunctionDecl);
        let key_b = ClangDeclKey::new("src/b.hpp".to_string(), 2, 1, clang_sys::CXCursor_FunctionDecl);
        let parse_a = ClangParseResult::with_semantic_offsets(
            true,
            Vec::new(),
            Vec::new(),
            FxHashMap::default(),
            FxHashMap::from_iter([(key_a.clone(), vec![3, 9])]),
            crate::parser::clang_result::ClangDiagnosticSummary::default(),
            Vec::new(),
        );
        let parse_b = ClangParseResult::with_semantic_offsets(
            true,
            Vec::new(),
            Vec::new(),
            FxHashMap::default(),
            FxHashMap::from_iter([(key_b.clone(), vec![5])]),
            crate::parser::clang_result::ClangDiagnosticSummary::default(),
            Vec::new(),
        );

        let mut state = ProjectGraphState::new();
        GraphUpdater::apply_clang_parse(&mut state, &use_path, &parse_a);
        GraphUpdater::apply_clang_parse(&mut state, &use_path, &parse_b);

        let file_id = SymbolId::new("file|src/ref_refresh.cpp");
        let refs = state
            .edges
            .iter()
            .filter(|edge| edge.kind == GraphEdgeKind::Reference && edge.from == file_id)
            .collect::<Vec<_>>();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].to, key_b.symbol_id());
        assert_eq!(refs[0].weight, 1);
    }
}
