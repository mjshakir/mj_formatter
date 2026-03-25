use std::cell::RefCell;
use std::collections::BTreeSet;

use tree_sitter::{Node, Parser, StreamingIterator, Tree};

use crate::parser::query_cache::TsQueryCache;

pub fn query_or_traverse<'a, F>(
    root: Node<'a>,
    pattern: &str,
    query_cache: Option<&TsQueryCache>,
    fallback_kinds: &[&str],
    mut process: F,
) where
    F: FnMut(Node<'a>),
{
    if let Some(query) = query_cache.and_then(|qc| qc.get_or_compile(pattern).ok()) {
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut matches = cursor.matches(&query, root, "".as_bytes());
        while let Some(m) = {
            matches.advance();
            matches.get()
        } {
            for capture in m.captures {
                process(capture.node);
            }
        }
    } else {
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if fallback_kinds.contains(&node.kind()) {
                process(node);
            }
            for idx in (0..node.child_count()).rev() {
                if let Some(child) = node.child(idx as u32) {
                    stack.push(child);
                }
            }
        }
    }
}

pub fn query_or_traverse_collect<'a>(
    root: Node<'a>,
    pattern: &str,
    query_cache: Option<&TsQueryCache>,
    fallback_kinds: &[&str],
) -> Vec<Node<'a>> {
    let mut nodes = Vec::new();
    query_or_traverse(root, pattern, query_cache, fallback_kinds, |node| {
        nodes.push(node);
    });
    nodes
}

pub fn first_descendant<'a>(
    node: Node<'a>,
    target_kinds: &[&str],
    stop_kinds: &[&str],
) -> Option<Node<'a>> {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if target_kinds.contains(&current.kind()) {
            return Some(current);
        }
        if stop_kinds.contains(&current.kind()) {
            continue;
        }
        for idx in (0..current.child_count()).rev() {
            if let Some(child) = current.child(idx as u32) {
                stack.push(child);
            }
        }
    }
    None
}

pub fn first_descendant_excluding_root<'a>(
    node: Node<'a>,
    target_kinds: &[&str],
    stop_kinds: &[&str],
) -> Option<Node<'a>> {
    let root_id = node.id();
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.id() != root_id && target_kinds.contains(&current.kind()) {
            return Some(current);
        }
        if current.id() != root_id && stop_kinds.contains(&current.kind()) {
            continue;
        }
        for idx in (0..current.child_count()).rev() {
            if let Some(child) = current.child(idx as u32) {
                stack.push(child);
            }
        }
    }
    None
}

pub fn rightmost_descendant<'a>(
    node: Node<'a>,
    target_kinds: &[&str],
    stop_kinds: &[&str],
) -> Option<Node<'a>> {
    let mut best = None;
    let mut best_start = 0usize;
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if target_kinds.contains(&current.kind())
            && (best.is_none() || current.start_byte() >= best_start)
        {
            best = Some(current);
            best_start = current.start_byte();
        }
        if stop_kinds.contains(&current.kind()) {
            continue;
        }
        for idx in (0..current.child_count()).rev() {
            if let Some(child) = current.child(idx as u32) {
                stack.push(child);
            }
        }
    }
    best
}

pub fn first_child_by_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    for idx in 0..node.child_count() {
        if let Some(child) = node.child(idx as u32) {
            if child.kind() == kind {
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
    let mut total_nodes = 0usize;
    let mut error_nodes = 0usize;
    let mut error_lines = BTreeSet::<usize>::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        total_nodes = total_nodes.saturating_add(1);
        if node.is_error() || node.is_missing() {
            error_nodes = error_nodes.saturating_add(1);
            error_lines.insert(node.start_position().row.saturating_add(1));
        }
        for idx in (0..node.child_count()).rev() {
            if let Some(child) = node.child(idx as u32) {
                stack.push(child);
            }
        }
    }
    TreeErrorStats {
        total_nodes,
        error_nodes,
        error_lines,
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
    use crate::parser::node_kind;
    let mut current = decl_node.child_by_field_name("declarator")?;
    loop {
        let kind = current.kind();
        if kind == node_kind::IDENTIFIER || kind == node_kind::FIELD_IDENTIFIER {
            return Some(current);
        }
        if let Some(child) = current.child_by_field_name("declarator") {
            current = child;
            continue;
        }
        for i in 0..current.child_count() {
            if let Some(child) = current.child(i as u32) {
                let ck = child.kind();
                if ck == node_kind::IDENTIFIER || ck == node_kind::FIELD_IDENTIFIER {
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
