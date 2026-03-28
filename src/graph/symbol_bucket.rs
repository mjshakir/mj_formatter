use std::path::Path;

use crate::parser::clang_types::{self, ClangDeclKey};
use crate::parser::file_context::SemanticDeclaration;
use crate::graph::types::GraphNodeKind;
use crate::graph::symbol_id::SymbolId;

fn sanitize_id_component(raw: &str) -> String {
    raw.replace('|', "%7C")
}

pub trait ToSymbolId {
    fn symbol_id(&self) -> SymbolId;
}

impl ToSymbolId for SemanticDeclaration {
    fn symbol_id(&self) -> SymbolId {
        if let Some(usr) = self
            .usr
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return SymbolId::new(format!("usr|{}", sanitize_id_component(usr)));
        }

        if let Some(scope_usr) = self
            .scope_usr
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return SymbolId::new(format!(
                "scoped|{}|{}|{}",
                clang_types::cursor_kind_spelling(self.kind),
                sanitize_id_component(scope_usr),
                sanitize_id_component(self.name.as_str())
            ));
        }

        legacy_id(self)
    }
}

impl ToSymbolId for ClangDeclKey {
    fn symbol_id(&self) -> SymbolId {
        SymbolId::new(format!(
            "decl|{}|{}|{}|{}",
            clang_types::cursor_kind_spelling(self.kind),
            sanitize_id_component(self.path.as_str()),
            self.line,
            self.column
        ))
    }
}

pub fn legacy_id(decl: &SemanticDeclaration) -> SymbolId {
    SymbolId::new(format!(
        "bucket|{}|{}",
        clang_types::cursor_kind_spelling(decl.kind),
        sanitize_id_component(decl.name.as_str())
    ))
}

#[cfg(test)]
pub fn id_candidates(decl: &SemanticDeclaration) -> Vec<SymbolId> {
    let canonical = decl.symbol_id();
    let legacy = legacy_id(decl);
    if canonical == legacy {
        vec![canonical]
    } else {
        vec![canonical, legacy]
    }
}

impl From<i32> for GraphNodeKind {
    fn from(kind: i32) -> Self {
        clang_types::graph_node_kind(kind)
    }
}

pub fn file_symbol_id(path: &Path) -> SymbolId {
    SymbolId::new(format!("file|{}", path.to_string_lossy()))
}

#[cfg(test)]
mod tests {
    use crate::parser::clang_types::ClangDeclKey;
    use crate::parser::file_context::SemanticDeclaration;

    use super::{legacy_id, id_candidates, ToSymbolId};

    #[test]
    fn canonical_uses_usr() {
        let decl = SemanticDeclaration {
            name: "Foo".to_string(),
            kind: clang_sys::CXCursor_TypedefDecl,
            line: 1,
            column: 1,
            usr: Some("c:@S@Foo".to_string()),
            ..Default::default()
        };
        assert_eq!(decl.symbol_id().as_str(), "usr|c:@S@Foo");
    }

    #[test]
    fn legacy_id_stable() {
        let decl = SemanticDeclaration {
            name: "Foo".to_string(),
            kind: clang_sys::CXCursor_TypedefDecl,
            line: 1,
            column: 1,
            ..Default::default()
        };
        assert_eq!(legacy_id(&decl).as_str(), "bucket|TypedefDecl|Foo");
    }

    #[test]
    fn candidates_include_legacy() {
        let decl = SemanticDeclaration {
            name: "Value".to_string(),
            kind: clang_sys::CXCursor_VarDecl,
            line: 8,
            column: 2,
            usr: Some("c:@value".to_string()),
            ..Default::default()
        };
        let ids = id_candidates(&decl);
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0].as_str(), "usr|c:@value");
        assert_eq!(ids[1].as_str(), "bucket|VarDecl|Value");
    }

    #[test]
    fn decl_key_stable() {
        let key = ClangDeclKey::new(
            "/tmp/demo.hpp".to_string(),
            12,
            4,
            clang_sys::CXCursor_FunctionDecl,
        );
        assert_eq!(
            key.symbol_id().as_str(),
            "decl|FunctionDecl|/tmp/demo.hpp|12|4"
        );
    }
}
