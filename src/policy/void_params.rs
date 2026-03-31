use tree_sitter::Node;

use crate::model::edit::Edit;
use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::model::violation::Violation;
use crate::parser::query_cache::TsQueryCache;
use crate::parser::ts_cpp_symbols;
use crate::parser::ts_traversal;
use crate::policy::Policy;

pub struct FunctionVoidParamsPolicy {
    require_void: bool,
    no_space_before_paren: bool,
}

impl FunctionVoidParamsPolicy {
    pub fn new(require_void: bool, no_space_before_paren: bool) -> Self {
        Self {
            require_void,
            no_space_before_paren,
        }
    }

    fn is_empty_parameter_list(param_node: Node<'_>, source: &[u8]) -> bool {
        let named = param_node.named_child_count();
        if named == 0 {
            return true;
        }
        if named == 1 {
            if let Some(child) = param_node.named_child(0) {
                if child.kind_id() == ts_cpp_symbols::sym_parameter_declaration {
                    if let Some(type_node) = child.child_by_field_id(ts_cpp_symbols::field_type) {
                        return type_node.utf8_text(source).ok() == Some("void")
                            && child.child_by_field_id(ts_cpp_symbols::field_declarator).is_none();
                    }
                }
            }
        }
        false
    }

    fn is_operator_declarator(node: Node<'_>) -> bool {
        ts_traversal::first_descendant(
            node,
            &[ts_cpp_symbols::sym_operator_name],
            &[ts_cpp_symbols::sym_parameter_list],
        )
        .is_some()
    }

    fn collect_function_declarators<'a>(
        root: Node<'a>,
        query_cache: Option<&TsQueryCache>,
        source: &[u8],
        changed_ranges: Option<&[tree_sitter::Range]>,
    ) -> Vec<Node<'a>> {
        ts_traversal::query_or_traverse_in_ranges_collect(root, "(function_declarator) @decl", query_cache, &[ts_cpp_symbols::sym_function_declarator], source, changed_ranges)
    }

    fn line_edits(before: &str, after: &str) -> Vec<Edit> {
        let mut edits = Vec::new();
        let before_lines = before.split('\n').collect::<Vec<_>>();
        let after_lines = after.split('\n').collect::<Vec<_>>();
        let shared = before_lines.len().min(after_lines.len());
        for idx in 0..shared {
            if before_lines[idx] == after_lines[idx] {
                continue;
            }
            edits.push(Edit {
                policy: "function_void_params".into(),
                line: idx + 1,
                before: before_lines[idx].to_string(),
                after: after_lines[idx].to_string(),
            });
        }
        edits
    }
}

impl Policy for FunctionVoidParamsPolicy {
    fn name(&self) -> &str {
        "function_void_params"
    }
    fn apply(&self, context: &PolicyContext<'_>) -> PolicyResult {
        let Some(tree) = context.tree_sitter_tree else {
            return PolicyResult::unchanged_with_warning("function_void_params: tree-sitter context unavailable".to_string());
        };

        let semantic_query = context.semantic_query();
        if !semantic_query.is_available() {
            return PolicyResult::unchanged_with_warning(
                "function_void_params: semantic context unavailable; skipping heuristic edits"
                    .to_string(),
            );
        }
        let mut replacements: Vec<(usize, usize, String)> = Vec::new();
        let mut violations = Vec::new();
        let mut warnings = Vec::new();
        let root = tree.root_node();

        let declarators = Self::collect_function_declarators(root, context.query_cache, context.text.as_bytes(), context.changed_ranges);

        for node in &declarators {
            let node = *node;
            let Some(param_node) = ts_traversal::first_descendant(
                node,
                &[ts_cpp_symbols::sym_parameter_list],
                &[ts_cpp_symbols::sym_compound_statement, ts_cpp_symbols::sym_field_declaration_list],
            ) else {
                continue;
            };

            let param_start = param_node.start_byte();
            let param_end = param_node.end_byte();
            let Some(params_text) = context.text.get(param_start..param_end) else {
                continue;
            };

            if !Self::is_empty_parameter_list(param_node, context.text.as_bytes()) {
                continue;
            }

            if Self::is_operator_declarator(node) {
                continue;
            }

            let name_node = ts_traversal::rightmost_descendant(
                node,
                &[
                    ts_cpp_symbols::sym_identifier,
                    ts_cpp_symbols::alias_sym_field_identifier,
                    ts_cpp_symbols::alias_sym_type_identifier,
                    ts_cpp_symbols::sym_destructor_name,
                ],
                &[ts_cpp_symbols::sym_parameter_list, ts_cpp_symbols::sym_template_parameter_list],
            );
            let (line, column, name_text) = if let Some(name_node) = name_node {
                let name = name_node
                    .utf8_text(context.text.as_bytes())
                    .unwrap_or("")
                    .to_string();
                (
                    name_node.start_position().row + 1,
                    name_node.start_position().column + 1,
                    name,
                )
            } else {
                (
                    param_node.start_position().row + 1,
                    param_node.start_position().column + 1,
                    "function".to_string(),
                )
            };

            let replacement = if self.require_void {
                "(void)".to_string()
            } else {
                "()".to_string()
            };
            if !semantic_query.is_safe_edit(line, column) {
                warnings.push(format!(
                    "function_void_params: skipped semantic-unsafe edit for '{name_text}' at line {line}"
                ));
                continue;
            }
            if params_text != replacement {
                replacements.push((param_start, param_end, replacement));
                violations.push(Violation {
                    policy: self.name().into(),
                    message: format!("empty parameter list for '{name_text}'"),
                    line,
                    column: Some(column),
                });
            }

            if self.no_space_before_paren {
                let mut ws_start = param_start;
                while ws_start > node.start_byte() {
                    let prev = context
                        .text
                        .as_bytes()
                        .get(ws_start - 1)
                        .copied()
                        .unwrap_or_default();
                    if prev != b' ' && prev != b'\t' {
                        break;
                    }
                    ws_start -= 1;
                }
                if ws_start < param_start {
                    replacements.push((ws_start, param_start, String::new()));
                }
            }
        }

        if replacements.is_empty() {
            return PolicyResult {
                changed: false,
                violations,
                warnings,
                ..Default::default()
            };
        }

        let mut data = context.text.as_bytes().to_vec();
        replacements.sort_by(|left, right| right.0.cmp(&left.0));
        for (start, end, replacement) in replacements {
            data.splice(start..end, replacement.into_bytes());
        }
        let updated = String::from_utf8_lossy(&data).to_string();
        let edits = Self::line_edits(context.text, &updated);

        PolicyResult {
            text: updated,
            changed: true,
            violations,
            edits,
            warnings,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tree_sitter::Parser;

    use super::*;
    use crate::model::policy_context::PolicyContext;
    use crate::parser::file_context::SemanticFileContext;

    fn parse_cpp(text: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("set cpp language");
        parser.parse(text, None).expect("parse tree")
    }

    #[test]
    fn adds_void_params() {
        let policy = FunctionVoidParamsPolicy::new(true, true);
        let text = "int foo () { return 0; }\n".to_string();
        let tree = parse_cpp(text.as_str());
        let semantic = SemanticFileContext::default();
        let path = PathBuf::from("sample.cpp");
        let ctx = PolicyContext::new(text.as_str(), &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic));
        let result = policy.apply(&ctx);
        assert!(result.text.contains("foo(void)"));
    }
}
