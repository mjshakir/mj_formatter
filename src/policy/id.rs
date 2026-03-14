use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum PolicyId {
    DashCommentNormalizer,
    SectionTitleNormalizer,
    CompactDeclarations,
    ClassLayout,
    LuaMacroSpacing,
    NamespaceEndComments,
    PragmaOnceSpacing,
    IncludeGuards,
    IncludeOrder,
    LogicalKeywordOperators,
    FunctionVoidParams,
    OperatorOverloadSpacing,
    ClangFormat,
    NamingConventions,
    SnakeCase,
    NumericLiteralSuffix,
    Unknown(String),
}

impl PolicyId {
    pub fn from_str_lossy(value: &str) -> Self {
        let normalized = value.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "dash_comment_normalizer" => Self::DashCommentNormalizer,
            "section_title_normalizer" => Self::SectionTitleNormalizer,
            "compact_declarations" => Self::CompactDeclarations,
            "class_layout" => Self::ClassLayout,
            "lua_macro_spacing" => Self::LuaMacroSpacing,
            "namespace_end_comments" => Self::NamespaceEndComments,
            "pragma_once_spacing" => Self::PragmaOnceSpacing,
            "include_guards" => Self::IncludeGuards,
            "include_order" => Self::IncludeOrder,
            "logical_keyword_operators" => Self::LogicalKeywordOperators,
            "function_void_params" => Self::FunctionVoidParams,
            "operator_overload_spacing" => Self::OperatorOverloadSpacing,
            "clang_format" => Self::ClangFormat,
            "naming_conventions" => Self::NamingConventions,
            "snake_case" => Self::SnakeCase,
            "numeric_literal_suffix" => Self::NumericLiteralSuffix,
            _ => Self::Unknown(normalized),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::DashCommentNormalizer => "dash_comment_normalizer",
            Self::SectionTitleNormalizer => "section_title_normalizer",
            Self::CompactDeclarations => "compact_declarations",
            Self::ClassLayout => "class_layout",
            Self::LuaMacroSpacing => "lua_macro_spacing",
            Self::NamespaceEndComments => "namespace_end_comments",
            Self::PragmaOnceSpacing => "pragma_once_spacing",
            Self::IncludeGuards => "include_guards",
            Self::IncludeOrder => "include_order",
            Self::LogicalKeywordOperators => "logical_keyword_operators",
            Self::FunctionVoidParams => "function_void_params",
            Self::OperatorOverloadSpacing => "operator_overload_spacing",
            Self::ClangFormat => "clang_format",
            Self::NamingConventions => "naming_conventions",
            Self::SnakeCase => "snake_case",
            Self::NumericLiteralSuffix => "numeric_literal_suffix",
            Self::Unknown(value) => value.as_str(),
        }
    }
}

impl fmt::Display for PolicyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use crate::policy::id::PolicyId;

    #[test]
    fn parses_known_policy_ids_case_insensitively() {
        assert_eq!(
            PolicyId::from_str_lossy("Naming_Conventions"),
            PolicyId::NamingConventions
        );
        assert_eq!(
            PolicyId::from_str_lossy("  CLANG_FORMAT "),
            PolicyId::ClangFormat
        );
    }

    #[test]
    fn preserves_unknown_policy_as_normalized_string() {
        assert_eq!(
            PolicyId::from_str_lossy("  Custom_Policy "),
            PolicyId::Unknown("custom_policy".to_string())
        );
    }

    #[test]
    fn formats_policy_id_as_policy_name() {
        assert_eq!(PolicyId::SnakeCase.to_string(), "snake_case");
        assert_eq!(
            PolicyId::Unknown("custom_policy".to_string()).to_string(),
            "custom_policy"
        );
    }
}
