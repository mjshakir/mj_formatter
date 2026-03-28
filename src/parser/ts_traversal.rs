use std::cell::RefCell;
use std::collections::BTreeSet;

use tree_sitter::{Node, Parser, StreamingIterator, Tree};

use crate::parser::query_cache::TsQueryCache;
use crate::parser::ts_cpp_symbols;

pub fn query_or_traverse<'a, F>(
    root: Node<'a>,
    pattern: &str,
    query_cache: Option<&TsQueryCache>,
    fallback_ids: &[u16],
    source: &[u8],
    mut process: F,
) where
    F: FnMut(Node<'a>),
{
    // Try cached query first, then direct compilation, then DFS fallback
    let cached = query_cache.and_then(|qc| qc.get_or_compile(pattern).ok());
    if let Some(ref query) = cached {
        run_query_matches(query, root, source, &mut process);
        return;
    }
    let language: tree_sitter::Language = tree_sitter_cpp::LANGUAGE.into();
    if let Ok(query) = tree_sitter::Query::new(&language, pattern) {
        run_query_matches(&query, root, source, &mut process);
        return;
    }
    // DFS fallback — should only be reached if pattern is invalid
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        if fallback_ids.contains(&node.kind_id()) {
            process(node);
        }
        if cursor.goto_first_child() {
            continue;
        }
        if cursor.goto_next_sibling() {
            continue;
        }
        loop {
            if !cursor.goto_parent() {
                return;
            }
            if cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

fn run_query_matches<'a>(
    query: &tree_sitter::Query,
    root: Node<'a>,
    source: &[u8],
    process: &mut impl FnMut(Node<'a>),
) {
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(query, root, source);
    while let Some(m) = {
        matches.advance();
        matches.get()
    } {
        for capture in m.captures {
            process(capture.node);
        }
    }
}

pub fn query_or_traverse_in_ranges<'a, F>(
    root: Node<'a>,
    pattern: &str,
    query_cache: Option<&TsQueryCache>,
    fallback_ids: &[u16],
    source: &[u8],
    changed_ranges: Option<&[tree_sitter::Range]>,
    mut process: F,
) where
    F: FnMut(Node<'a>),
{
    let Some(ranges) = changed_ranges else {
        query_or_traverse(root, pattern, query_cache, fallback_ids, source, process);
        return;
    };

    let cached = query_cache.and_then(|qc| qc.get_or_compile(pattern).ok());
    let language: tree_sitter::Language = tree_sitter_cpp::LANGUAGE.into();
    let direct = if cached.is_none() {
        tree_sitter::Query::new(&language, pattern).ok()
    } else {
        None
    };
    let Some(query) = cached.as_deref().or(direct.as_ref()) else {
        query_or_traverse(root, pattern, query_cache, fallback_ids, source, process);
        return;
    };

    for range in ranges {
        let mut cursor = tree_sitter::QueryCursor::new();
        cursor.set_byte_range(range.start_byte..range.end_byte);
        let mut matches = cursor.matches(query, root, source);
        while let Some(m) = {
            matches.advance();
            matches.get()
        } {
            for capture in m.captures {
                process(capture.node);
            }
        }
    }
}

pub fn query_or_traverse_in_ranges_collect<'a>(
    root: Node<'a>,
    pattern: &str,
    query_cache: Option<&TsQueryCache>,
    fallback_ids: &[u16],
    source: &[u8],
    changed_ranges: Option<&[tree_sitter::Range]>,
) -> Vec<Node<'a>> {
    let mut nodes = Vec::new();
    query_or_traverse_in_ranges(root, pattern, query_cache, fallback_ids, source, changed_ranges, |node| {
        nodes.push(node);
    });
    nodes
}

pub fn first_descendant<'a>(
    node: Node<'a>,
    target_ids: &[u16],
    stop_ids: &[u16],
) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    loop {
        let current = cursor.node();
        if target_ids.contains(&current.kind_id()) {
            return Some(current);
        }
        if !stop_ids.contains(&current.kind_id()) && cursor.goto_first_child() {
            continue;
        }
        if cursor.goto_next_sibling() {
            continue;
        }
        loop {
            if !cursor.goto_parent() {
                return None;
            }
            if cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

pub fn first_descendant_excluding_root<'a>(
    node: Node<'a>,
    target_ids: &[u16],
    stop_ids: &[u16],
) -> Option<Node<'a>> {
    let root_id = node.id();
    let mut cursor = node.walk();
    loop {
        let current = cursor.node();
        let is_root = current.id() == root_id;
        if !is_root && target_ids.contains(&current.kind_id()) {
            return Some(current);
        }
        if !is_root && stop_ids.contains(&current.kind_id()) {
            // skip subtree — fall through to sibling/parent advance
        } else if cursor.goto_first_child() {
            continue;
        }
        if cursor.goto_next_sibling() {
            continue;
        }
        loop {
            if !cursor.goto_parent() {
                return None;
            }
            if cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

pub fn rightmost_descendant<'a>(
    node: Node<'a>,
    target_ids: &[u16],
    stop_ids: &[u16],
) -> Option<Node<'a>> {
    let mut best: Option<Node<'a>> = None;
    let mut best_start = 0usize;
    let mut cursor = node.walk();
    loop {
        let current = cursor.node();
        if target_ids.contains(&current.kind_id())
            && (best.is_none() || current.start_byte() >= best_start)
        {
            best = Some(current);
            best_start = current.start_byte();
        }
        if !stop_ids.contains(&current.kind_id()) && cursor.goto_first_child() {
            continue;
        }
        if cursor.goto_next_sibling() {
            continue;
        }
        loop {
            if !cursor.goto_parent() {
                return best;
            }
            if cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

pub fn first_child_by_kind<'a>(node: Node<'a>, kind_id: u16) -> Option<Node<'a>> {
    for idx in 0..node.named_child_count() {
        if let Some(child) = node.named_child(idx as u32) {
            if child.kind_id() == kind_id {
                return Some(child);
            }
        }
    }
    None
}

pub struct TreeErrorStats {
    pub total_nodes: usize,
    pub error_nodes: usize,
    pub error_lines: BTreeSet<usize>,
}

impl TreeErrorStats {
    pub fn error_ratio(&self) -> f64 {
        if self.total_nodes == 0 {
            0.0
        } else {
            (self.error_nodes as f64 / self.total_nodes as f64).clamp(0.0, 1.0)
        }
    }
}

pub fn tree_error_stats(tree: &Tree) -> TreeErrorStats {
    let root = tree.root_node();
    let total_nodes = count_nodes(root);
    if !root.has_error() {
        return TreeErrorStats {
            total_nodes,
            error_nodes: 0,
            error_lines: BTreeSet::new(),
        };
    }
    let mut error_nodes = 0usize;
    let mut error_lines = BTreeSet::<usize>::new();
    let language: tree_sitter::Language = tree_sitter_cpp::LANGUAGE.into();
    if let Ok(query) = tree_sitter::Query::new(&language, "(ERROR) @e") {
        let mut cursor = tree_sitter::QueryCursor::new();
        let empty: &[u8] = &[];
        let mut matches = cursor.matches(&query, root, empty);
        while let Some(m) = {
            matches.advance();
            matches.get()
        } {
            for capture in m.captures {
                error_nodes = error_nodes.saturating_add(1);
                error_lines.insert(capture.node.start_position().row.saturating_add(1));
            }
        }
    }
    // Also count MISSING nodes (not captured by ERROR query)
    collect_missing_nodes(root, &mut error_nodes, &mut error_lines);
    TreeErrorStats {
        total_nodes,
        error_nodes,
        error_lines,
    }
}

fn count_nodes(root: Node<'_>) -> usize {
    let mut total = 0usize;
    let mut cursor = root.walk();
    loop {
        total = total.saturating_add(1);
        if cursor.goto_first_child() {
            continue;
        }
        if cursor.goto_next_sibling() {
            continue;
        }
        loop {
            if !cursor.goto_parent() {
                return total;
            }
            if cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

fn collect_missing_nodes(
    root: Node<'_>,
    error_nodes: &mut usize,
    error_lines: &mut BTreeSet<usize>,
) {
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        if node.is_missing() {
            *error_nodes = error_nodes.saturating_add(1);
            error_lines.insert(node.start_position().row.saturating_add(1));
        }
        if cursor.goto_first_child() {
            continue;
        }
        if cursor.goto_next_sibling() {
            continue;
        }
        loop {
            if !cursor.goto_parent() {
                return;
            }
            if cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

thread_local! {
    static VALIDATION_CPP_PARSER: RefCell<Parser> = RefCell::new({
        let mut parser = Parser::new();
        let _ = parser.set_language(&tree_sitter_cpp::LANGUAGE.into());
        parser
    });
}

pub fn declarator_identifier(decl_node: Node<'_>) -> Option<Node<'_>> {
    let mut current = decl_node.child_by_field_id(ts_cpp_symbols::field_declarator)?;
    loop {
        if ts_cpp_symbols::is_identifier_like(current.kind_id()) {
            return Some(current);
        }
        if let Some(child) = current.child_by_field_id(ts_cpp_symbols::field_declarator) {
            current = child;
            continue;
        }
        for i in 0..current.named_child_count() {
            if let Some(child) = current.named_child(i as u32) {
                if ts_cpp_symbols::is_identifier_like(child.kind_id()) {
                    return Some(child);
                }
            }
        }
        return None;
    }
}

pub fn quick_error_stats_cpp(text: &str) -> Option<TreeErrorStats> {
    VALIDATION_CPP_PARSER.with(|parser| {
        let tree = parser.borrow_mut().parse(text, None)?;
        Some(tree_error_stats(&tree))
    })
}
