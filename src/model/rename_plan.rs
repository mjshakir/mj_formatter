use crate::parser::clang_types::ClangDeclKey;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticRenamePlan {
    pub decl: ClangDeclKey,
    pub old_name: String,
    pub new_name: String,
}

impl SemanticRenamePlan {
    pub fn to_internal_warning(&self) -> String {
        format!(
            "internal:semantic_rename_plan:{}:{}:{}:{}:{}:{}",
            Self::kind_to_tag(self.decl.kind),
            self.decl.line,
            self.decl.column,
            self.old_name,
            self.new_name,
            self.decl.path
        )
    }

    pub fn from_internal_warning(value: &str) -> Option<Self> {
        let payload = value.strip_prefix("internal:semantic_rename_plan:")?;
        let mut parts = payload.splitn(6, ':');
        let kind = Self::tag_to_kind(parts.next()?)?;
        let line = parts.next()?.parse::<usize>().ok()?;
        let column = parts.next()?.parse::<usize>().ok()?;
        let old_name = parts.next()?.to_string();
        let new_name = parts.next()?.to_string();
        let path = parts.next()?.to_string();

        Some(Self {
            decl: ClangDeclKey::new(path, line, column, kind),
            old_name,
            new_name,
        })
    }

    fn kind_to_tag(kind: crate::parser::clang_types::ClangSymbolKind) -> &'static str {
        use crate::parser::clang_types::ClangSymbolKind;
        match kind {
            ClangSymbolKind::Function | ClangSymbolKind::FunctionTemplate => "function",
            ClangSymbolKind::Method => "method",
            ClangSymbolKind::Constructor => "constructor",
            ClangSymbolKind::Destructor => "destructor",
            ClangSymbolKind::Variable => "variable",
            ClangSymbolKind::Field => "field",
            ClangSymbolKind::Parameter => "parameter",
            ClangSymbolKind::Type
            | ClangSymbolKind::Struct
            | ClangSymbolKind::Class
            | ClangSymbolKind::Union
            | ClangSymbolKind::Enum
            | ClangSymbolKind::Typedef
            | ClangSymbolKind::TypeAlias => "type",
            ClangSymbolKind::Namespace => "namespace",
            ClangSymbolKind::Macro => "macro",
            ClangSymbolKind::ConversionFunction => "conversion_function",
            ClangSymbolKind::UsingDecl => "using_decl",
            ClangSymbolKind::EnumConstant => "enum_constant",
            ClangSymbolKind::FriendDecl => "friend_decl",
            ClangSymbolKind::Other => "other",
        }
    }

    fn tag_to_kind(tag: &str) -> Option<crate::parser::clang_types::ClangSymbolKind> {
        use crate::parser::clang_types::ClangSymbolKind;
        match tag {
            "function" => Some(ClangSymbolKind::Function),
            "method" => Some(ClangSymbolKind::Method),
            "constructor" => Some(ClangSymbolKind::Constructor),
            "destructor" => Some(ClangSymbolKind::Destructor),
            "variable" => Some(ClangSymbolKind::Variable),
            "field" => Some(ClangSymbolKind::Field),
            "parameter" => Some(ClangSymbolKind::Parameter),
            "type" => Some(ClangSymbolKind::Type),
            "namespace" => Some(ClangSymbolKind::Namespace),
            "macro" => Some(ClangSymbolKind::Macro),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SemanticRenamePlan;
    use crate::parser::clang_types::ClangDeclKey;
    use crate::parser::clang_types::ClangSymbolKind;

    #[test]
    fn roundtrip_internal_warning() {
        let plan = SemanticRenamePlan {
            decl: ClangDeclKey::new(
                "/tmp/sample.hpp".to_string(),
                42,
                7,
                ClangSymbolKind::Function,
            ),
            old_name: "BadName".to_string(),
            new_name: "bad_name".to_string(),
        };

        let serialized = plan.to_internal_warning();
        let parsed = SemanticRenamePlan::from_internal_warning(serialized.as_str())
            .expect("internal warning should parse");
        assert_eq!(parsed, plan);
    }
}
