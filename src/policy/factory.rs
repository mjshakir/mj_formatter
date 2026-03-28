use rustc_hash::FxHashSet;

use crate::config::app_config::AppConfig;
use crate::config::policy_config::PolicyConfig;
use crate::policy::clang_format::ClangFormatPolicy;
use crate::policy::class_layout::ClassLayoutPolicy;
use crate::policy::compact_decls::CompactDeclarationsPolicy;
use crate::policy::Policy;
use crate::policy::dash_comment::DashCommentNormalizerPolicy;
use crate::policy::void_params::FunctionVoidParamsPolicy;
use crate::policy::include_guards::{IncludeGuardMode, IncludeGuardsPolicy};
use crate::policy::include_order::IncludeOrderPolicy;
use crate::policy::keyword_operators::LogicalKeywordOperatorsPolicy;
use crate::policy::macro_spacing::LuaMacroSpacingPolicy;
use crate::policy::ns_comments::NsCommentsPolicy;
use crate::policy::naming_conventions::NamingConventionsPolicy;
use crate::policy::numeric_suffix::NumericLiteralSuffixPolicy;
use crate::policy::op_spacing::OperatorOverloadSpacingPolicy;
use crate::policy::id::PolicyId;
use crate::policy::pragma_once::PragmaOnceSpacingPolicy;
use crate::policy::section_title::SectionTitleNormalizerPolicy;
use crate::policy::snake_case::{SnakeCaseApplyTarget, SnakeCasePolicy};
use crate::policy::stub::StubPolicy;

pub struct PolicyFactory {
    clang_format_binary: String,
}

impl PolicyFactory {
    pub fn new(config: &AppConfig) -> Self {
        Self {
            clang_format_binary: config.clang_format_binary.clone(),
        }
    }

    pub fn create(&self, name: &str, settings: &PolicyConfig) -> Box<dyn Policy> {
        match PolicyId::from_str_lossy(name) {
            PolicyId::DashCommentNormalizer => {
                let mode = settings
                    .string_value("mode")
                    .unwrap_or_else(|| "threshold".to_string());
                let mode_auto = mode.eq_ignore_ascii_case("auto");
                let long_length = settings.usize_value("long_length").unwrap_or(64);
                let short_length = settings.usize_value("short_length").unwrap_or(28);
                let long_threshold = settings.usize_value("long_threshold").unwrap_or(50);
                let min_length = settings.usize_value("min_length").unwrap_or(short_length);
                Box::new(DashCommentNormalizerPolicy::new(
                    mode_auto,
                    long_length,
                    short_length,
                    long_threshold,
                    min_length,
                ))
            }
            PolicyId::SectionTitleNormalizer => {
                let mapping = settings.table_string_map("mapping");
                Box::new(SectionTitleNormalizerPolicy::new(mapping))
            }
            PolicyId::CompactDeclarations => {
                let min_group_size = settings.usize_value("min_group_size").unwrap_or(3).max(2);
                Box::new(CompactDeclarationsPolicy::new(min_group_size))
            }
            PolicyId::ClassLayout => {
                let source_extensions = settings.string_list_value("source_extensions");
                let header_extensions = settings.string_list_value("header_extensions");
                let header_search_roots = settings.string_list_value("header_search_roots");
                Box::new(ClassLayoutPolicy::new(
                    source_extensions,
                    header_extensions,
                    header_search_roots,
                ))
            }
            PolicyId::LuaMacroSpacing => {
                Box::new(LuaMacroSpacingPolicy::new())
            }
            PolicyId::NamespaceEndComments => {
                let blocks = settings.string_list_value("blocks");
                let control_block_kinds = settings.string_list_value("control_block_kinds");
                let max_named_lines = settings.usize_value("max_named_lines").unwrap_or(40).max(1);
                let max_label_length = settings
                    .usize_value("max_label_length")
                    .unwrap_or(48)
                    .max(8);
                let replace_existing = settings.bool_value("replace_existing").unwrap_or(true);
                Box::new(NsCommentsPolicy::new(
                    blocks,
                    control_block_kinds,
                    max_named_lines,
                    max_label_length,
                    replace_existing,
                ))
            }
            PolicyId::PragmaOnceSpacing => {
                let blank_lines_after = settings.usize_value("blank_lines_after").unwrap_or(1);
                Box::new(PragmaOnceSpacingPolicy::new(blank_lines_after))
            }
            PolicyId::IncludeGuards => {
                let mode = settings
                    .string_value("mode")
                    .map(|value| IncludeGuardMode::from_value(&value))
                    .unwrap_or(IncludeGuardMode::PragmaOnce);
                let header_extensions: FxHashSet<String> = settings
                    .string_list_value("header_extensions")
                    .into_iter()
                    .map(|value| value.to_lowercase())
                    .collect();
                let header_extensions = if header_extensions.is_empty() {
                    crate::files::file_unit::HEADER_EXTENSIONS
                        .iter()
                        .map(|e| format!(".{e}"))
                        .collect()
                } else {
                    header_extensions
                };
                Box::new(IncludeGuardsPolicy::new(mode, header_extensions))
            }
            PolicyId::IncludeOrder => {
                let order_header = settings.string_list_value("order_header");
                let order_source = settings.string_list_value("order_source");
                let standard_headers: FxHashSet<String> = settings
                    .string_list_value("standard_headers")
                    .into_iter()
                    .map(|item| item.to_lowercase())
                    .collect();
                let standard_prefixes = settings
                    .string_list_value("standard_prefixes")
                    .into_iter()
                    .map(|item| item.to_lowercase())
                    .collect();
                let project_headers: FxHashSet<String> = settings
                    .string_list_value("project_headers")
                    .into_iter()
                    .map(|item| item.to_lowercase())
                    .collect();
                let project_prefixes = settings
                    .string_list_value("project_prefixes")
                    .into_iter()
                    .map(|item| item.to_lowercase())
                    .collect();
                let main_header_extensions = settings.string_list_value("main_header_extensions");
                let separator_length = settings
                    .usize_value("separator_length")
                    .unwrap_or(64)
                    .max(2);
                let emit_group_comments =
                    settings.bool_value("emit_group_comments").unwrap_or(false);
                let group_titles = settings.table_string_map("group_titles");
                let third_party_labels = settings.table_string_map("third_party_labels");
                Box::new(IncludeOrderPolicy::new(
                    order_header,
                    order_source,
                    standard_headers,
                    standard_prefixes,
                    project_headers,
                    project_prefixes,
                    main_header_extensions,
                    separator_length,
                    emit_group_comments,
                    group_titles,
                    third_party_labels,
                ))
            }
            PolicyId::LogicalKeywordOperators => {
                let replace_and = settings.bool_value("replace_and").unwrap_or(true);
                let replace_or = settings.bool_value("replace_or").unwrap_or(true);
                let skip_preprocessor = settings.bool_value("skip_preprocessor").unwrap_or(true);
                Box::new(LogicalKeywordOperatorsPolicy::new(
                    replace_and,
                    replace_or,
                    skip_preprocessor,
                ))
            }
            PolicyId::FunctionVoidParams => {
                let require_void = settings.bool_value("require_void").unwrap_or(true);
                let no_space_before_paren =
                    settings.bool_value("no_space_before_paren").unwrap_or(true);
                Box::new(FunctionVoidParamsPolicy::new(
                    require_void,
                    no_space_before_paren,
                ))
            }
            PolicyId::OperatorOverloadSpacing => {
                Box::new(OperatorOverloadSpacingPolicy::new())
            }
            PolicyId::ClangFormat => {
                let style = settings
                    .string_value("style")
                    .unwrap_or_else(|| "file".to_string());
                Box::new(ClangFormatPolicy::new(
                    self.clang_format_binary.clone(),
                    style,
                ))
            }
            PolicyId::NamingConventions => {
                let semantic = settings.bool_value("use_semantic_rename").unwrap_or(true);
                let semantic_strict = settings
                    .bool_value("semantic_strict")
                    .or_else(|| settings.bool_value("strict_local_scope"))
                    .unwrap_or(true);
                let standard = settings.string_value("standard").unwrap_or_else(|| "mj".to_string());
                let std_table = settings.table_value("standards").and_then(|t| t.get(&standard).and_then(|v| v.as_table()));
                let get_prefix = |key: &str, default: &str| -> Box<str> {
                    std_table.and_then(|t| t.get(key).and_then(|v| v.as_str())).unwrap_or(default).into()
                };
                let prefix_config = crate::policy::naming_conventions::PrefixConfig {
                    local: get_prefix("local_prefix", "_"),
                    member: get_prefix("member_prefix", "m_"),
                    global: get_prefix("global_prefix", "g_"),
                    static_lower: get_prefix("static_prefix", "s_"),
                    static_upper: get_prefix("static_prefix_upper", "S_"),
                    const_lower: get_prefix("const_prefix", "c_"),
                    constexpr_upper: get_prefix("constexpr_prefix_upper", "C_"),
                    volatile: get_prefix("volatile_prefix", "v_"),
                    pointer: get_prefix("pointer_prefix", "p_"),
                    shared_ptr: get_prefix("shared_ptr_prefix", "sp_"),
                    unique_ptr: get_prefix("unique_ptr_prefix", "up_"),
                    weak_ptr: get_prefix("weak_ptr_prefix", "wp_"),
                    function: get_prefix("function_prefix", "f_"),
                    reference: get_prefix("reference_prefix", "r_"),
                    atomic: get_prefix("atomic_prefix", "a_"),
                    enum_var: get_prefix("enum_var_prefix", "e_"),
                    struct_var: get_prefix("struct_var_prefix", "t_"),
                };
                Box::new(NamingConventionsPolicy::new(
                    semantic,
                    semantic_strict,
                ).with_prefix_config(prefix_config))
            }
            PolicyId::SnakeCase => {
                let apply_target = settings
                    .string_value("apply_to")
                    .map(|value| SnakeCaseApplyTarget::from_value(&value))
                    .unwrap_or(SnakeCaseApplyTarget::Both);
                let exclude_class_namespace = settings
                    .bool_value("exclude_class_namespace")
                    .unwrap_or(true);
                let prefer_clang = settings.bool_value("prefer_clang").unwrap_or(true);
                let use_tree_sitter = settings.bool_value("use_tree_sitter").unwrap_or(true);
                Box::new(SnakeCasePolicy::new(
                    apply_target,
                    exclude_class_namespace,
                    prefer_clang,
                    use_tree_sitter,
                ))
            }
            PolicyId::NumericLiteralSuffix => {
                Box::new(NumericLiteralSuffixPolicy::new())
            }
            PolicyId::Unknown(other) => {
                Box::new(StubPolicy::new(other, "not ported yet".to_string()))
            }
        }
    }
}
