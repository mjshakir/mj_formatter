mod case_utils;
mod prefix;
mod replacement;

pub use prefix::PrefixConfig;

use smallvec::SmallVec;

use tree_sitter::{Node, StreamingIterator};

use crate::model::policy_context::PolicyContext;
use crate::model::policy_result::PolicyResult;
use crate::model::rename_plan::SemanticRenamePlan;
use crate::model::violation::Violation;
use crate::parser::clang_types::ClangDeclKey;
use crate::parser::file_context::{SemanticDeclaration, SemanticFileContext};
use crate::parser::ts_traversal;
use crate::policy::Policy;
use crate::parser::text_scan;
use crate::parser::ts_cpp_symbols;

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

#[derive(Clone, Debug, Default)]
struct IdentifierContext<'a> {
    pub(crate) is_field: bool,
    pub(crate) is_global: bool,
    pub(crate) is_method_local: bool,
    pub(crate) ts_static: bool,
    pub(crate) ts_const: bool,
    pub(crate) ts_volatile: bool,
    pub(crate) ts_pointer: bool,
    pub(crate) ts_reference: bool,
    pub(crate) canonical_type_kind: i32,
    pub(crate) num_template_args: i32,
    pub(crate) template_base_name: Option<&'a str>,
    pub(crate) type_spelling: Option<&'a str>,
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

impl NamingConventionsPolicy {
    fn semantic_parse_clean(semantic: &SemanticFileContext) -> bool {
        if semantic.diagnostic_counts[clang_sys::CXDiagnostic_Fatal as usize] > 0 {
            return false;
        }
        if !semantic.declarations.is_empty() {
            return true;
        }
        let summary = semantic.summary();
        if summary.declaration_count > 0 && summary.reference_count > 0 {
            return true;
        }
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
        let type_node = decl_node.child_by_field_id(ts_cpp_symbols::field_type)?;
        if type_node.kind_id() != ts_cpp_symbols::sym_template_type {
            return None;
        }
        let name_node = type_node.child_by_field_id(ts_cpp_symbols::field_name)?;
        let leaf = ts_traversal::first_descendant(
            name_node,
            &[ts_cpp_symbols::alias_sym_type_identifier],
            &[],
        ).unwrap_or(name_node);
        leaf.utf8_text(source).ok()
    }

    fn extract_type_context_from_clang<'a>(
        decl: &'a SemanticDeclaration,
        decl_node: Node<'a>,
        source: &'a [u8],
    ) -> IdentifierContext<'a> {
        let ts_reference = decl_node
            .child_by_field_id(ts_cpp_symbols::field_declarator)
            .is_some_and(|declarator| {
                if declarator.kind_id() == ts_cpp_symbols::sym_reference_declarator {
                    return true;
                }
                if declarator.kind_id() == ts_cpp_symbols::sym_init_declarator {
                    if let Some(inner) = declarator.child_by_field_id(ts_cpp_symbols::field_declarator) {
                        return inner.kind_id() == ts_cpp_symbols::sym_reference_declarator;
                    }
                }
                false
            });
        let is_pointer = decl.pointee_type_kind.is_some_and(|k| k != clang_sys::CXType_Invalid)
            || decl.canonical_type_kind == clang_sys::CXType_Pointer as i32;
        let is_reference = ts_reference
            || decl.canonical_type_kind == clang_sys::CXType_LValueReference as i32
            || decl.canonical_type_kind == clang_sys::CXType_RValueReference as i32;
        let is_method_local = matches!(
            decl.semantic_parent_kind,
            clang_sys::CXCursor_CXXMethod
                | clang_sys::CXCursor_Constructor
                | clang_sys::CXCursor_Destructor
                | clang_sys::CXCursor_FunctionDecl
                | clang_sys::CXCursor_FunctionTemplate
        ) && decl.kind == clang_sys::CXCursor_VarDecl as i32;
        IdentifierContext {
            ts_static: decl.storage_class == clang_sys::CX_SC_Static,
            ts_const: decl.is_const_qualified,
            ts_volatile: decl.is_volatile_qualified,
            ts_pointer: is_pointer,
            ts_reference: is_reference,
            canonical_type_kind: decl.canonical_type_kind,
            num_template_args: decl.num_template_args,
            template_base_name: decl.template_base_name.as_deref()
                .or_else(|| Self::extract_ts_template_name(decl_node, source)),
            type_spelling: decl.type_spelling.as_deref(),
            is_method_local,
            ..IdentifierContext::default()
        }
    }

    fn extract_ts_type_context<'a>(decl_node: Node<'a>, source: &'a [u8]) -> IdentifierContext<'a> {
        let mut ctx = IdentifierContext::default();
        for i in 0..decl_node.named_child_count() {
            let Some(child) = decl_node.named_child(i as u32) else { continue };
            match child.kind_id() {
                ts_cpp_symbols::sym_storage_class_specifier => {
                    if child.child(0).is_some_and(|c| c.kind_id() == ts_cpp_symbols::anon_sym_static) {
                        ctx.ts_static = true;
                    }
                }
                ts_cpp_symbols::sym_type_qualifier => {
                    if let Some(kw) = child.child(0) {
                        match kw.kind_id() {
                            ts_cpp_symbols::anon_sym_const => ctx.ts_const = true,
                            ts_cpp_symbols::anon_sym_volatile => ctx.ts_volatile = true,
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(declarator) = decl_node.child_by_field_id(ts_cpp_symbols::field_declarator) {
            match declarator.kind_id() {
                ts_cpp_symbols::sym_pointer_declarator => ctx.ts_pointer = true,
                ts_cpp_symbols::sym_reference_declarator => ctx.ts_reference = true,
                ts_cpp_symbols::sym_init_declarator => {
                    if let Some(inner) = declarator.child_by_field_id(ts_cpp_symbols::field_declarator) {
                        match inner.kind_id() {
                            ts_cpp_symbols::sym_pointer_declarator => ctx.ts_pointer = true,
                            ts_cpp_symbols::sym_reference_declarator => ctx.ts_reference = true,
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        ctx.template_base_name = Self::extract_ts_template_name(decl_node, source);
        ctx.type_spelling = decl_node
            .child_by_field_id(ts_cpp_symbols::field_type)
            .and_then(|n| n.utf8_text(source).ok());
        ctx.is_method_local = Self::ts_is_inside_method(decl_node);
        ctx
    }

    fn ts_is_inside_method(node: Node<'_>) -> bool {
        let mut current = node.parent();
        let mut in_function = false;
        while let Some(n) = current {
            if n.kind_id() == ts_cpp_symbols::sym_function_definition {
                in_function = true;
            }
            if in_function
                && (n.kind_id() == ts_cpp_symbols::sym_class_specifier
                    || n.kind_id() == ts_cpp_symbols::sym_struct_specifier)
            {
                return true;
            }
            current = n.parent();
        }
        false
    }

    const RENAME_CANDIDATE_QUERY: &str = r#"[
        (function_definition) @node
        (declaration) @node
        (field_declaration) @node
    ]"#;

    fn has_semantic_rename_candidates(
        root: Node<'_>,
        query_cache: Option<&crate::parser::query_cache::TsQueryCache>,
        source: &[u8],
    ) -> bool {
        let cached = query_cache
            .and_then(|qc| qc.get_or_compile(Self::RENAME_CANDIDATE_QUERY).ok());
        let language: tree_sitter::Language = tree_sitter_cpp::LANGUAGE.into();
        let direct = if cached.is_none() {
            tree_sitter::Query::new(&language, Self::RENAME_CANDIDATE_QUERY).ok()
        } else {
            None
        };
        if let Some(query) = cached.as_deref().or(direct.as_ref()) {
            let mut cursor = tree_sitter::QueryCursor::new();
            let mut matches = cursor.matches(query, root, source);
            matches.advance();
            return matches.get().is_some();
        }
        false
    }

    fn is_loop_header_declaration(node: Node<'_>) -> bool {
        let Some(parent) = node.parent() else {
            return false;
        };
        match parent.kind_id() {
            ts_cpp_symbols::sym_for_statement
            | ts_cpp_symbols::sym_for_range_loop
            | ts_cpp_symbols::sym_while_statement => {
                let body = parent.child_by_field_id(ts_cpp_symbols::field_body);
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
                parent.kind_id(),
                ts_cpp_symbols::sym_class_specifier | ts_cpp_symbols::sym_struct_specifier
            ) {
                if let Some(name_node) = parent.child_by_field_id(ts_cpp_symbols::field_name) {
                    if name_node.utf8_text(source).is_ok_and(|cn| cn == name) {
                        return true;
                    }
                }
                for i in 0..parent.named_child_count() {
                    if let Some(child) = parent.named_child(i as u32) {
                        if child.kind_id() == ts_cpp_symbols::alias_sym_type_identifier
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

    fn is_ts_destructor<'a>(name_node: Node<'a>, _name: &str, _source: &[u8]) -> bool {
        let mut cursor = name_node;
        while let Some(parent) = cursor.parent() {
            if parent.kind_id() == ts_cpp_symbols::sym_destructor_name {
                return true;
            }
            if parent.kind_id() == ts_cpp_symbols::sym_function_declarator
                || parent.kind_id() == ts_cpp_symbols::sym_function_definition
            {
                break;
            }
            cursor = parent;
        }
        false
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

        let policy_id = crate::policy::id::PolicyId::from_str_lossy(self.name());
        let mut violations = Vec::with_capacity(64);
        let mut warnings = Vec::with_capacity(8);
        let mut strict_issues = StrictIssues::new(self.semantic_mode && self.semantic_strict);
        let mut rename_plans = Vec::with_capacity(32);
        let mut prefix_buf = String::with_capacity(16);
        let mut snake_buf = String::with_capacity(64);
        let mut upper_pos_buf: SmallVec<[usize; 16]> = SmallVec::new();
        let semantic_query = context.semantic_query();
        let semantic_file_context = context.semantic_file_context;
        let semantic_enabled = self.semantic_mode
            && semantic_file_context
                .is_some_and(Self::semantic_parse_clean);

        let root = tree.root_node();
        let has_candidate_nodes =
            Self::has_semantic_rename_candidates(root, context.query_cache, context.text.as_bytes());

        if self.semantic_mode {
            match semantic_file_context {
                Some(ctx)
                    if has_candidate_nodes
                        && !Self::semantic_parse_clean(ctx) =>
                {
                    let counts = ctx.diagnostic_counts;
                    warnings.push(
                        format!(
                            "naming_conventions: semantic rename skipped due insufficient semantic parse reliability (success={}, symbols={}, errors={}, fatals={})",
                            ctx.clang_success,
                            ctx.declarations.len(),
                            counts[clang_sys::CXDiagnostic_Error as usize],
                            counts[clang_sys::CXDiagnostic_Fatal as usize]
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
        let candidates = ts_traversal::query_or_traverse_in_ranges_collect(
            root,
            Self::RENAME_CANDIDATE_QUERY,
            context.query_cache,
            &[ts_cpp_symbols::sym_function_definition, ts_cpp_symbols::sym_declaration, ts_cpp_symbols::sym_field_declaration],
            context.text.as_bytes(),
            context.changed_ranges,
        );
        for node in candidates {
            'process: {
                if node.kind_id() == ts_cpp_symbols::sym_function_definition {
                    if let Some(declarator) =
                        ts_traversal::first_descendant(node, &[ts_cpp_symbols::sym_function_declarator], &[ts_cpp_symbols::sym_compound_statement])
                    {
                        if let Some(name_node) = ts_traversal::rightmost_descendant(
                            declarator,
                            &[
                                ts_cpp_symbols::sym_identifier,
                                ts_cpp_symbols::alias_sym_field_identifier,
                                ts_cpp_symbols::alias_sym_type_identifier,
                                ts_cpp_symbols::sym_destructor_name,
                            ],
                            &[ts_cpp_symbols::sym_parameter_list, ts_cpp_symbols::sym_template_parameter_list],
                        ) {
                            let name = name_node.utf8_text(context.text.as_bytes()).unwrap_or("");
                            let short = name;
                            let line = name_node.start_position().row + 1;
                            let source_column = name_node.start_position().column + 1;
                            if name_node.kind_id() == ts_cpp_symbols::sym_destructor_name {
                                break 'process;
                            }
                            let matched_symbol = semantic_file_context
                                .and_then(|ctx| ctx.symbol_on_line(short, line, &[]))
                                .filter(|decl| {
                                    matches!(
                                        decl.kind,
                                        clang_sys::CXCursor_FunctionDecl
                                            | clang_sys::CXCursor_FunctionTemplate
                                            | clang_sys::CXCursor_CXXMethod
                                            | clang_sys::CXCursor_Constructor
                                            | clang_sys::CXCursor_Destructor
                                            | clang_sys::CXCursor_ConversionFunction
                                    )
                                })
                                .map(|decl| (decl.kind, decl.column));
                            if matched_symbol.is_none() && name_node.kind_id() == ts_cpp_symbols::alias_sym_type_identifier {
                                break 'process;
                            }

                            let is_operator = matched_symbol
                                .is_some_and(|(kind, _)| kind == clang_sys::CXCursor_ConversionFunction)
                                || name_node.parent().is_some_and(|p|
                                    p.kind_id() == ts_cpp_symbols::sym_operator_name
                                    || p.parent().is_some_and(|gp| gp.kind_id() == ts_cpp_symbols::sym_operator_name)
                                );
                            if !is_operator
                                && !Self::is_snake_case(short)
                            {
                                if let Some((kind, _)) = matched_symbol {
                                    if kind == clang_sys::CXCursor_Constructor
                                        || kind == clang_sys::CXCursor_Destructor
                                    {
                                        break 'process;
                                    }
                                } else if Self::is_ts_constructor(name_node, short, context.text.as_bytes())
                                    || Self::is_ts_destructor(name_node, short, context.text.as_bytes())
                                {
                                    break 'process;
                                }
                                Self::to_snake_case_into(short, &mut upper_pos_buf, &mut snake_buf);
                                violations.push(Violation {
                                    policy: policy_id.clone(),
                                    message: format!(
                                        "function '{}' is not snake_case; suggested '{}'",
                                        short, &snake_buf
                                    ),
                                    line,
                                    column: Some(source_column),
                                });

                                if semantic_enabled {
                                    let Some(sem_ctx) = semantic_file_context else {
                                        break 'process;
                                    };
                                    if let Some((kind, column)) = matched_symbol {
                                        if !Self::resolve_rename_plan(
                                            &semantic_query, sem_ctx, short, snake_buf.clone(),
                                            line, source_column, kind, column, &[],
                                            &mut rename_plans,
                                        ) {
                                            break 'process;
                                        }
                                    } else {
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
                    if let Some(sem_ctx) = semantic_file_context {
                        let error_start_line = node.start_position().row + 1;
                        let error_end_line = node.end_position().row + 1;
                        for decl in &sem_ctx.declarations {
                            if decl.line < error_start_line || decl.line > error_end_line {
                                continue;
                            }
                            if !matches!(
                                decl.kind,
                                clang_sys::CXCursor_FunctionDecl
                                    | clang_sys::CXCursor_FunctionTemplate
                                    | clang_sys::CXCursor_CXXMethod
                                    | clang_sys::CXCursor_Constructor
                                    | clang_sys::CXCursor_Destructor
                                    | clang_sys::CXCursor_ConversionFunction
                            ) {
                                continue;
                            }
                            let short = decl.name.as_str();
                            if short.starts_with("operator")
                                || Self::is_snake_case(short)
                            {
                                continue;
                            }
                            let line = decl.line;
                            let source_column = decl.column;
                            Self::to_snake_case_into(short, &mut upper_pos_buf, &mut snake_buf);
                            violations.push(Violation {
                                policy: policy_id.clone(),
                                message: format!(
                                    "function '{}' is not snake_case; suggested '{}'",
                                    short, &snake_buf
                                ),
                                line,
                                column: Some(source_column),
                            });

                            if semantic_enabled
                                && !Self::resolve_rename_plan(
                                    &semantic_query, sem_ctx, short, snake_buf.clone(),
                                    line, source_column, decl.kind, source_column, &[],
                                    &mut rename_plans,
                                )
                            {
                                continue;
                            }
                        }
                    }
                }

                if matches!(
                    node.kind_id(),
                    ts_cpp_symbols::sym_declaration | ts_cpp_symbols::sym_field_declaration | ts_cpp_symbols::sym_parameter_declaration
                ) {
                    if node.kind_id() == ts_cpp_symbols::sym_parameter_declaration
                        || node.kind_id() == ts_cpp_symbols::sym_field_declaration
                    {
                        break 'process;
                    }
                    if Self::is_loop_header_declaration(node) {
                        break 'process;
                    }
                    if let Some(name_node) = ts_traversal::declarator_identifier(node) {
                        let name = name_node.utf8_text(context.text.as_bytes()).unwrap_or("");
                        if Self::is_cpp_keyword(name) {
                            break 'process;
                        }
                        let line = name_node.start_position().row + 1;
                        let source_column = name_node.start_position().column + 1;
                        let raw_sym = semantic_file_context
                            .and_then(|ctx| ctx.symbol_on_line(name, line, &[]));
                        if raw_sym.is_some_and(|d| !matches!(
                            d.kind,
                            clang_sys::CXCursor_VarDecl
                                | clang_sys::CXCursor_FieldDecl
                                | clang_sys::CXCursor_ParmDecl
                        )) {
                            break 'process;
                        }
                        let sym = raw_sym.filter(|decl| {
                            matches!(
                                    decl.kind,
                                    clang_sys::CXCursor_VarDecl
                                        | clang_sys::CXCursor_FieldDecl
                                        | clang_sys::CXCursor_ParmDecl
                                )
                        });
                        if sym.is_some_and(|d| d.kind == clang_sys::CXCursor_ParmDecl) {
                            break 'process;
                        }

                        if self.prefixes.has_known_prefix(name)
                            || Self::is_constant_like_identifier(name)
                        {
                            break 'process;
                        }

                        let id_ctx = if let Some(decl) = sym {
                            let is_field = decl.kind == clang_sys::CXCursor_FieldDecl;
                            let is_global = decl.scope_usr.is_none() && decl.kind == clang_sys::CXCursor_VarDecl;
                            let mut ctx = Self::extract_type_context_from_clang(decl, node, context.text.as_bytes());
                            ctx.is_field = is_field;
                            ctx.is_global = is_global;
                            ctx
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
                            policy: policy_id.clone(),
                            message: msg,
                            line,
                            column: Some(source_column),
                        });

                        if semantic_enabled {
                            if let (Some(sem_ctx), Some(decl)) = (semantic_file_context, sym) {
                                if !Self::resolve_rename_plan(
                                    &semantic_query, sem_ctx, name, snake_buf.clone(),
                                    line, source_column, decl.kind, decl.column, &[],
                                    &mut rename_plans,
                                ) {
                                    break 'process;
                                }
                            }
                        }
                    }
                }
            }

        }

        if let Some(sem_ctx) = semantic_file_context {
            if !sem_ctx.clang_success {
                warnings.push(
                    "naming_conventions: clang syntax diagnostics detected; semantic confidence reduced"
                        .to_string(),
                );
                for message in sem_ctx.diagnostics.iter().take(5) {
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
            if let Some(sem_ctx) = semantic_file_context {
                let mut diag = RenameDiagnostics {
                    warnings: &mut warnings,
                    strict_issues: &mut strict_issues,
                };
                self.apply_semantic_renames(
                    context.text,
                    sem_ctx,
                    &trust_filtered_plans,
                    &semantic_query,
                    &non_code_ranges,
                    &mut diag,
                )
            } else {
                warnings.push(
                    "naming_conventions: semantic rename skipped due missing semantic context"
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
    use crate::parser::clang_result::ClangParseResult;
    use crate::parser::file_context::SemanticDeclaration;
    use crate::parser::clang_types::ClangSymbolKey;
    fn parse_cpp(text: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .expect("cpp language");
        parser.parse(text, None).expect("parse tree")
    }

    fn semantic_from(
        text: &str,
        path: &std::path::Path,
        tree: &tree_sitter::Tree,
        clang: &ClangParseResult,
    ) -> crate::parser::file_context::SemanticFileContext {
        crate::parser::file_context::SemanticFileContext::from_parses(text, path, Some(tree), Some(clang))
    }

    #[test]
    fn clang_miss_falls_through_to_ts() {
        let policy = NamingConventionsPolicy::new(true, true);
        let text = "int CamelVar = 0;\nint BadName() { return CamelVar; }\n";
        let tree = parse_cpp(text);
        let clang_parse_result = ClangParseResult::new(
            true,
            Vec::new(),
            vec![SemanticDeclaration {
                name: "DifferentName".to_string(),
                kind: clang_sys::CXCursor_FunctionDecl,
                line: 2,
                column: 5,
                ..Default::default()
            }],
            [0; 5],
            Vec::new(),
        );
        let path = PathBuf::from("sample.cpp");
        let semantic = semantic_from(text, &path, &tree, &clang_parse_result);
        let context = PolicyContext::new(text, &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic));
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
        let symbol = SemanticDeclaration {
            name: "CamelVar".to_string(),
            kind: clang_sys::CXCursor_VarDecl,
            line: 2,
            column: 7,
            scope_usr: Some("c:@F@f#".to_string()),
            ..Default::default()
        };
        let rename_offsets = FxHashMap::from_iter([(
            ClangSymbolKey::new(symbol.name.clone(), symbol.kind, symbol.line),
            vec![declaration_offset, reference_offset],
        )]);
        let path = PathBuf::from("sample.cpp");
        let canonical = std::fs::canonicalize(&path)
            .unwrap_or_else(|_| path.clone())
            .to_string_lossy()
            .to_string();
        let decl_key = crate::parser::clang_types::ClangDeclKey::new(
            canonical,
            symbol.line,
            symbol.column,
            symbol.kind,
        );
        let reference_offsets = FxHashMap::from_iter([(
            decl_key,
            vec![reference_offset],
        )]);
        let clang_parse_result = ClangParseResult::with_semantic_offsets(
            true,
            Vec::new(),
            vec![symbol],
            rename_offsets,
            reference_offsets,
            [0; 5],
            Vec::new(),
        );

        let semantic = semantic_from(text, &path, &tree, &clang_parse_result);
        let context = PolicyContext::new(text, &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic));
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

        let camel_symbol = SemanticDeclaration {
            name: "CamelVar".to_string(),
            kind: clang_sys::CXCursor_VarDecl,
            line: 1,
            column: 5,
            ..Default::default()
        };
        let existing_symbol = SemanticDeclaration {
            name: "G_CAMEL_VAR".to_string(),
            kind: clang_sys::CXCursor_VarDecl,
            line: 2,
            column: 5,
            ..Default::default()
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
            [0; 5],
            Vec::new(),
        );

        let path = PathBuf::from("sample.cpp");
        let semantic = semantic_from(text, &path, &tree, &clang_parse_result);
        let context = PolicyContext::new(text, &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic));
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
            {
                let mut c: [usize; 5] = [0; 5];
                c[clang_sys::CXDiagnostic_Error as usize] = 1;
                c
            },
            vec![crate::parser::clang_result::ClangDiagnosticEntry {
                line: 1,
                column: 1,
                severity: clang_sys::CXDiagnostic_Error as u32,
                warning_option: String::new(),
                fix_its: Vec::new(),
            }],
        );
        let path = PathBuf::from("sample.cpp");
        let semantic = semantic_from(text, &path, &tree, &clang_parse_result);
        let context = PolicyContext::new(text, &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic));
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
        let symbol = SemanticDeclaration {
            name: "CamelVar".to_string(),
            kind: clang_sys::CXCursor_VarDecl,
            line: 1,
            column: 5,
            usr: Some("usr:test:camelvar".to_string()),
            scope_usr: Some("usr:test:scope".to_string()),
            ..Default::default()
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
            {
                let mut c: [usize; 5] = [0; 5];
                c[clang_sys::CXDiagnostic_Fatal as usize] = 1;
                c
            },
            vec![crate::parser::clang_result::ClangDiagnosticEntry {
                line: 10,
                column: 4,
                severity: clang_sys::CXDiagnostic_Fatal as u32,
                warning_option: String::new(),
                fix_its: Vec::new(),
            }],
        );

        let path = PathBuf::from("sample.cpp");
        let semantic = semantic_from(text, &path, &tree, &clang_parse_result);
        let context = PolicyContext::new(text, &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic));
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
            vec![SemanticDeclaration {
                name: "Node".to_string(),
                kind: clang_sys::CXCursor_Constructor,
                line: 2,
                column: 3,
                ..Default::default()
            }],
            [0; 5],
            Vec::new(),
        );
        let path = PathBuf::from("sample.cpp");
        let semantic = semantic_from(text, &path, &tree, &clang_parse_result);
        let context = PolicyContext::new(text, &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic));
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
            [0; 5],
            Vec::new(),
        );
        let path = PathBuf::from("HashSet.cpp");
        let semantic = semantic_from(text, &path, &tree, &clang_parse_result);
        let context = PolicyContext::new(text, &path)
            .with_tree(Some(&tree))
            .with_semantic(Some(&semantic));
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

    // --- build_stacked_prefix unit tests (tree-sitter path only) ---

    #[test]
    fn prefix_shared_ptr_via_ts() {
        let policy = NamingConventionsPolicy::new(true, true);
        let prefix = policy.build_stacked_prefix(&IdentifierContext { template_base_name: Some("shared_ptr"), num_template_args: 1, ..Default::default() });
        assert_eq!(prefix, "_sp_", "shared_ptr local should be '_sp_'");
    }

    #[test]
    fn prefix_unique_ptr_via_ts() {
        let policy = NamingConventionsPolicy::new(true, true);
        let prefix = policy.build_stacked_prefix(&IdentifierContext { template_base_name: Some("unique_ptr"), num_template_args: 1, ..Default::default() });
        assert_eq!(prefix, "_up_", "unique_ptr local should be '_up_'");
    }

    #[test]
    fn prefix_weak_ptr_via_ts() {
        let policy = NamingConventionsPolicy::new(true, true);
        let prefix = policy.build_stacked_prefix(&IdentifierContext { template_base_name: Some("weak_ptr"), num_template_args: 1, ..Default::default() });
        assert_eq!(prefix, "_wp_", "weak_ptr local should be '_wp_'");
    }

    #[test]
    fn prefix_function_type_via_ts() {
        let policy = NamingConventionsPolicy::new(true, true);
        let prefix = policy.build_stacked_prefix(&IdentifierContext { template_base_name: Some("function"), num_template_args: 1, ..Default::default() });
        assert_eq!(prefix, "_f_", "function type local should be '_f_'");
    }

    #[test]
    fn prefix_atomic_via_ts() {
        let policy = NamingConventionsPolicy::new(true, true);
        let prefix = policy.build_stacked_prefix(&IdentifierContext { template_base_name: Some("atomic"), num_template_args: 1, ..Default::default() });
        assert_eq!(prefix, "_a_", "atomic local should be '_a_'");
    }

    #[test]
    fn prefix_pointer_member_via_ts() {
        let policy = NamingConventionsPolicy::new(true, true);
        let prefix = policy.build_stacked_prefix(&IdentifierContext { is_field: true, ts_pointer: true, ..Default::default() });
        assert_eq!(prefix, "m_p_", "pointer member should be 'm_p_'");
    }

    #[test]
    fn prefix_static_const_pointer_member_via_ts() {
        let policy = NamingConventionsPolicy::new(true, true);
        let prefix = policy.build_stacked_prefix(&IdentifierContext { is_field: true, ts_static: true, ts_const: true, ts_pointer: true, ..Default::default() });
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
            template_base_name: Some("shared_ptr"),
            num_template_args: 1,
            ..Default::default()
        });
        assert_eq!(prefix, "_sp_", "shared_ptr via tree-sitter should produce '_sp_'");
    }

    #[test]
    fn prefix_ts_unique_ptr() {
        let policy = NamingConventionsPolicy::new(false, false);
        let prefix = policy.build_stacked_prefix(&IdentifierContext {
            template_base_name: Some("unique_ptr"),
            num_template_args: 1,
            ..Default::default()
        });
        assert_eq!(prefix, "_up_", "unique_ptr via tree-sitter should produce '_up_'");
    }

    #[test]
    fn prefix_ts_pointer_shared_ptr() {
        let policy = NamingConventionsPolicy::new(false, false);
        let prefix = policy.build_stacked_prefix(&IdentifierContext {
            ts_pointer: true,
            template_base_name: Some("shared_ptr"),
            num_template_args: 1,
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
