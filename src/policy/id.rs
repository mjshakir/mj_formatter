use std::borrow::Borrow;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

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

impl Default for PolicyId {
    fn default() -> Self {
        Self::Unknown(String::new())
    }
}

impl PolicyId {
    pub fn from_str_lossy(value: &str) -> Self {
        let trimmed = value.trim();
        if trimmed.eq_ignore_ascii_case("dash_comment_normalizer") { return Self::DashCommentNormalizer; }
        if trimmed.eq_ignore_ascii_case("section_title_normalizer") { return Self::SectionTitleNormalizer; }
        if trimmed.eq_ignore_ascii_case("compact_declarations") { return Self::CompactDeclarations; }
        if trimmed.eq_ignore_ascii_case("class_layout") { return Self::ClassLayout; }
        if trimmed.eq_ignore_ascii_case("lua_macro_spacing") { return Self::LuaMacroSpacing; }
        if trimmed.eq_ignore_ascii_case("namespace_end_comments") { return Self::NamespaceEndComments; }
        if trimmed.eq_ignore_ascii_case("pragma_once_spacing") { return Self::PragmaOnceSpacing; }
        if trimmed.eq_ignore_ascii_case("include_guards") { return Self::IncludeGuards; }
        if trimmed.eq_ignore_ascii_case("include_order") { return Self::IncludeOrder; }
        if trimmed.eq_ignore_ascii_case("logical_keyword_operators") { return Self::LogicalKeywordOperators; }
        if trimmed.eq_ignore_ascii_case("function_void_params") { return Self::FunctionVoidParams; }
        if trimmed.eq_ignore_ascii_case("operator_overload_spacing") { return Self::OperatorOverloadSpacing; }
        if trimmed.eq_ignore_ascii_case("clang_format") { return Self::ClangFormat; }
        if trimmed.eq_ignore_ascii_case("naming_conventions") { return Self::NamingConventions; }
        if trimmed.eq_ignore_ascii_case("snake_case") { return Self::SnakeCase; }
        if trimmed.eq_ignore_ascii_case("numeric_literal_suffix") { return Self::NumericLiteralSuffix; }
        Self::Unknown(trimmed.to_ascii_lowercase())
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Self::Unknown(value) => value.is_empty(),
            _ => false,
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

impl Serialize for PolicyId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PolicyId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from_str_lossy(value.as_str()))
    }
}

impl PartialEq<&str> for PolicyId {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<PolicyId> for &str {
    fn eq(&self, other: &PolicyId) -> bool {
        *self == other.as_str()
    }
}

impl Borrow<str> for PolicyId {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for PolicyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for PolicyId {
    fn from(value: &str) -> Self {
        Self::from_str_lossy(value)
    }
}

impl From<String> for PolicyId {
    fn from(value: String) -> Self {
        Self::from_str_lossy(value.as_str())
    }
}

impl From<&String> for PolicyId {
    fn from(value: &String) -> Self {
        Self::from_str_lossy(value.as_str())
    }
}

#[cfg(test)]
mod tests {
    use crate::policy::id::PolicyId;

    #[test]
    fn parses_ids_insensitively() {
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
    fn preserves_unknown_policy() {
        assert_eq!(
            PolicyId::from_str_lossy("  Custom_Policy "),
            PolicyId::Unknown("custom_policy".to_string())
        );
    }

    #[test]
    fn formats_policy_name() {
        assert_eq!(PolicyId::SnakeCase.to_string(), "snake_case");
        assert_eq!(
            PolicyId::Unknown("custom_policy".to_string()).to_string(),
            "custom_policy"
        );
    }

    #[test]
    fn serde_roundtrip() {
        let id = PolicyId::NamingConventions;
        let json = serde_json::to_value(&id).expect("serialize");
        assert_eq!(json, "naming_conventions");
        let restored: PolicyId = serde_json::from_value(json).expect("deserialize");
        assert_eq!(restored, id);
    }

    #[test]
    fn default_is_empty_unknown() {
        let id = PolicyId::default();
        assert_eq!(id, PolicyId::Unknown(String::new()));
    }

    #[test]
    fn partial_eq_str() {
        assert!(PolicyId::ClangFormat == "clang_format");
        assert!("snake_case" == PolicyId::SnakeCase);
    }
}
