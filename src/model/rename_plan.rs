use serde::{Deserialize, Serialize};

use crate::parser::clang_types::ClangDeclKey;

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
            self.decl.kind,
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
        let kind = parts.next()?.parse::<i32>().ok()?;
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
}

#[cfg(test)]
mod tests {
    use super::SemanticRenamePlan;
    use crate::parser::clang_types::ClangDeclKey;

    #[test]
    fn roundtrip_internal_warning() {
        let plan = SemanticRenamePlan {
            decl: ClangDeclKey::new(
                "/tmp/sample.hpp".to_string(),
                42,
                7,
                clang_sys::CXCursor_FunctionDecl,
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
