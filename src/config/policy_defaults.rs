use rustc_hash::FxHashMap;
use toml::{Table, Value};

use crate::config::policy_config::PolicyConfig;

fn str_val(s: &str) -> Value {
    Value::String(s.to_string())
}

fn int_val(n: i64) -> Value {
    Value::Integer(n)
}

fn bool_val(b: bool) -> Value {
    Value::Boolean(b)
}

fn str_array(items: &[&str]) -> Value {
    Value::Array(items.iter().map(|s| str_val(s)).collect())
}

fn naming_standard(
    local: &str,
    member: &str,
    global: &str,
    static_p: &str,
    const_p: &str,
    atomic: &str,
    pointer: &str,
    shared_ptr: &str,
    unique_ptr: &str,
    weak_ptr: &str,
    function: &str,
    reference: &str,
    volatile: &str,
    enum_var: &str,
    struct_var: &str,
    constexpr_upper: &str,
    static_upper: &str,
) -> Value {
    let mut t = Table::new();
    t.insert("local_prefix".into(), str_val(local));
    t.insert("member_prefix".into(), str_val(member));
    t.insert("global_prefix".into(), str_val(global));
    t.insert("static_prefix".into(), str_val(static_p));
    t.insert("const_prefix".into(), str_val(const_p));
    t.insert("atomic_prefix".into(), str_val(atomic));
    t.insert("pointer_prefix".into(), str_val(pointer));
    t.insert("shared_ptr_prefix".into(), str_val(shared_ptr));
    t.insert("unique_ptr_prefix".into(), str_val(unique_ptr));
    t.insert("weak_ptr_prefix".into(), str_val(weak_ptr));
    t.insert("function_prefix".into(), str_val(function));
    t.insert("reference_prefix".into(), str_val(reference));
    t.insert("volatile_prefix".into(), str_val(volatile));
    t.insert("enum_var_prefix".into(), str_val(enum_var));
    t.insert("struct_var_prefix".into(), str_val(struct_var));
    t.insert("constexpr_prefix_upper".into(), str_val(constexpr_upper));
    t.insert("static_prefix_upper".into(), str_val(static_upper));
    Value::Table(t)
}

fn convergence_table(domain: &str, priority: i64) -> Value {
    let mut t = Table::new();
    t.insert("domain".into(), str_val(domain));
    t.insert("priority".into(), int_val(priority));
    Value::Table(t)
}

fn build_policy(table: Table) -> PolicyConfig {
    PolicyConfig::from_policy_table(&table).expect("hardcoded policy table must be valid")
}

pub fn default_policy_configs() -> FxHashMap<String, PolicyConfig> {
    let mut result = FxHashMap::default();

    // clang_format
    {
        let mut t = Table::new();
        t.insert("name".into(), str_val("clang_format"));
        t.insert("enabled".into(), bool_val(true));
        t.insert("style".into(), str_val("{BasedOnStyle: LLVM, ColumnLimit: 100, UseTab: Never, IndentWidth: 4, TabWidth: 4, NamespaceIndentation: All, IndentAccessModifiers: true, AccessModifierOffset: 0, BreakBeforeBraces: Attach, AllowShortIfStatementsOnASingleLine: Never, AllowShortBlocksOnASingleLine: Never, AllowShortFunctionsOnASingleLine: Empty, AllowShortLoopsOnASingleLine: true, PointerAlignment: Left, ReferenceAlignment: Left, AlwaysBreakTemplateDeclarations: Yes, IndentPPDirectives: BeforeHash, ReflowComments: false, AlignConsecutiveAssignments: Consecutive, AlignConsecutiveDeclarations: Consecutive, SpaceAfterTemplateKeyword: false, SpaceBeforeParens: Never}"));
        t.insert("convergence".into(), convergence_table("layout.whitespace", 1000));
        result.insert("clang_format".into(), build_policy(t));
    }

    // class_layout
    {
        let mut t = Table::new();
        t.insert("name".into(), str_val("class_layout"));
        t.insert("enabled".into(), bool_val(true));
        t.insert("source_extensions".into(), str_array(&[".cpp", ".cc", ".cxx"]));
        t.insert("header_extensions".into(), str_array(&[".hpp", ".h", ".hh", ".hxx"]));
        t.insert("header_search_roots".into(), str_array(&["include"]));
        result.insert("class_layout".into(), build_policy(t));
    }

    // compact_declarations
    {
        let mut t = Table::new();
        t.insert("name".into(), str_val("compact_declarations"));
        t.insert("enabled".into(), bool_val(true));
        result.insert("compact_declarations".into(), build_policy(t));
    }

    // dash_comment_normalizer
    {
        let mut t = Table::new();
        t.insert("name".into(), str_val("dash_comment_normalizer"));
        t.insert("enabled".into(), bool_val(true));
        t.insert("mode".into(), str_val("auto"));
        result.insert("dash_comment_normalizer".into(), build_policy(t));
    }

    // function_void_params
    {
        let mut t = Table::new();
        t.insert("name".into(), str_val("function_void_params"));
        t.insert("enabled".into(), bool_val(true));
        result.insert("function_void_params".into(), build_policy(t));
    }

    // include_guards
    {
        let mut t = Table::new();
        t.insert("name".into(), str_val("include_guards"));
        t.insert("enabled".into(), bool_val(true));
        t.insert("header_extensions".into(), str_array(&[".h", ".hpp", ".hh", ".hxx"]));
        result.insert("include_guards".into(), build_policy(t));
    }

    // include_order
    {
        let mut t = Table::new();
        t.insert("name".into(), str_val("include_order"));
        t.insert("enabled".into(), bool_val(true));
        t.insert("order_header".into(), str_array(&["standard", "third_party", "project", "local"]));
        t.insert("order_source".into(), str_array(&["main", "standard", "third_party", "project", "local"]));
        t.insert("main_header_extensions".into(), str_array(&[".hpp", ".h", ".hh", ".hxx"]));
        let mut group_titles = Table::new();
        group_titles.insert("main".into(), str_val("Main header"));
        group_titles.insert("standard".into(), str_val("Standard Cpp Libraries"));
        group_titles.insert("third_party".into(), str_val("Third-party headers"));
        group_titles.insert("project".into(), str_val("Project headers"));
        group_titles.insert("local".into(), str_val("User Defined Headers"));
        t.insert("group_titles".into(), Value::Table(group_titles));
        t.insert("third_party_labels".into(), Value::Table(Table::new()));
        t.insert("convergence".into(), convergence_table("includes.structure", 900));
        result.insert("include_order".into(), build_policy(t));
    }

    // logical_keyword_operators
    {
        let mut t = Table::new();
        t.insert("name".into(), str_val("logical_keyword_operators"));
        t.insert("enabled".into(), bool_val(true));
        result.insert("logical_keyword_operators".into(), build_policy(t));
    }

    // lua_macro_spacing
    {
        let mut t = Table::new();
        t.insert("name".into(), str_val("lua_macro_spacing"));
        t.insert("enabled".into(), bool_val(true));
        result.insert("lua_macro_spacing".into(), build_policy(t));
    }

    // namespace_end_comments
    {
        let mut t = Table::new();
        t.insert("name".into(), str_val("namespace_end_comments"));
        t.insert("enabled".into(), bool_val(true));
        t.insert("control_block_kinds".into(), str_array(&["if", "while", "for", "switch", "catch"]));
        result.insert("namespace_end_comments".into(), build_policy(t));
    }

    // naming_conventions
    {
        let mut t = Table::new();
        t.insert("name".into(), str_val("naming_conventions"));
        t.insert("enabled".into(), bool_val(true));
        let mut standards = Table::new();
        standards.insert("mj".into(), naming_standard(
            "_", "m_", "g_", "s_", "c_", "a_", "p_", "sp_", "up_", "wp_",
            "f_", "r_", "v_", "e_", "t_", "C_", "S_",
        ));
        standards.insert("google".into(), naming_standard(
            "", "", "g_", "", "", "", "", "", "", "",
            "", "", "", "", "", "k", "",
        ));
        standards.insert("llvm".into(), naming_standard(
            "", "m", "", "", "", "", "", "", "", "",
            "", "", "", "", "", "k", "",
        ));
        standards.insert("qt".into(), naming_standard(
            "", "m_", "", "", "", "", "", "", "", "",
            "", "", "", "", "", "k", "",
        ));
        t.insert("standards".into(), Value::Table(standards));
        result.insert("naming_conventions".into(), build_policy(t));
    }

    // numeric_literal_suffix
    {
        let mut t = Table::new();
        t.insert("name".into(), str_val("numeric_literal_suffix"));
        t.insert("enabled".into(), bool_val(true));
        t.insert("enforcement".into(), str_val("soft"));
        t.insert("convergence".into(), convergence_table("literal.suffix", 1001));
        result.insert("numeric_literal_suffix".into(), build_policy(t));
    }

    // operator_overload_spacing
    {
        let mut t = Table::new();
        t.insert("name".into(), str_val("operator_overload_spacing"));
        t.insert("enabled".into(), bool_val(true));
        result.insert("operator_overload_spacing".into(), build_policy(t));
    }

    // pragma_once_spacing
    {
        let mut t = Table::new();
        t.insert("name".into(), str_val("pragma_once_spacing"));
        t.insert("enabled".into(), bool_val(true));
        result.insert("pragma_once_spacing".into(), build_policy(t));
    }

    // section_title_normalizer
    {
        let mut t = Table::new();
        t.insert("name".into(), str_val("section_title_normalizer"));
        t.insert("enabled".into(), bool_val(true));
        let mut mapping = Table::new();
        mapping.insert("Standard cpp library".into(), str_val("Standard cpp library"));
        mapping.insert("User Defined libraries".into(), str_val("User-defined libraries"));
        mapping.insert("Main Header".into(), str_val("Main Header"));
        t.insert("mapping".into(), Value::Table(mapping));
        t.insert("convergence".into(), convergence_table("includes.structure", 930));
        result.insert("section_title_normalizer".into(), build_policy(t));
    }

    // declaration_alignment
    {
        let mut t = Table::new();
        t.insert("name".into(), str_val("declaration_alignment"));
        t.insert("enabled".into(), bool_val(true));
        t.insert("convergence".into(), convergence_table("layout.whitespace", 950));
        result.insert("declaration_alignment".into(), build_policy(t));
    }

    // snake_case
    {
        let mut t = Table::new();
        t.insert("name".into(), str_val("snake_case"));
        t.insert("enabled".into(), bool_val(true));
        result.insert("snake_case".into(), build_policy(t));
    }

    result
}
