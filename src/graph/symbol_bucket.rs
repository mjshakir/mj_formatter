use std::path::Path;

use crate::parser::clang_types::ClangDeclKey;
use crate::parser::clang_symbol::ClangSymbol;
use crate::parser::clang_types::ClangSymbolKind;
use crate::graph::types::GraphNodeKind;
use crate::graph::symbol_id::SymbolId;

fn kind_label(kind: ClangSymbolKind) -> &'static str {
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

fn sanitize_id_component(raw: &str) -> String {
    raw.replace('|', "%7C")
}

pub trait ToSymbolId {
    fn symbol_id(&self) -> SymbolId;
}

impl ToSymbolId for ClangSymbol {
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
                kind_label(self.kind),
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
            kind_label(self.kind),
            sanitize_id_component(self.path.as_str()),
            self.line,
            self.column
        ))
    }
}

pub fn legacy_id(symbol: &ClangSymbol) -> SymbolId {
    SymbolId::new(format!(
        "bucket|{}|{}",
        kind_label(symbol.kind),
        sanitize_id_component(symbol.name.as_str())
    ))
}

#[cfg(test)]
pub fn id_candidates(symbol: &ClangSymbol) -> Vec<SymbolId> {
    let canonical = symbol.symbol_id();
    let legacy = legacy_id(symbol);
    if canonical == legacy {
        vec![canonical]
    } else {
        vec![canonical, legacy]
    }
}

impl From<ClangSymbolKind> for GraphNodeKind {
    fn from(kind: ClangSymbolKind) -> Self {
        match kind {
            ClangSymbolKind::Function
            | ClangSymbolKind::FunctionTemplate
            | ClangSymbolKind::Method
            | ClangSymbolKind::Constructor
            | ClangSymbolKind::Destructor => GraphNodeKind::Function,
            ClangSymbolKind::Variable => GraphNodeKind::Variable,
            ClangSymbolKind::Field => GraphNodeKind::Field,
            ClangSymbolKind::Parameter => GraphNodeKind::Parameter,
            ClangSymbolKind::Type
            | ClangSymbolKind::Struct
            | ClangSymbolKind::Class
            | ClangSymbolKind::Union
            | ClangSymbolKind::Enum
            | ClangSymbolKind::Typedef
            | ClangSymbolKind::TypeAlias => GraphNodeKind::Type,
            ClangSymbolKind::Namespace => GraphNodeKind::Namespace,
            ClangSymbolKind::Macro => GraphNodeKind::Macro,
            ClangSymbolKind::ConversionFunction => GraphNodeKind::Function,
            ClangSymbolKind::UsingDecl
            | ClangSymbolKind::EnumConstant
            | ClangSymbolKind::FriendDecl
            | ClangSymbolKind::Other => GraphNodeKind::Variable,
        }
    }
}

pub fn file_symbol_id(path: &Path) -> SymbolId {
    SymbolId::new(format!("file|{}", path.to_string_lossy()))
}

#[cfg(test)]
mod tests {
    use crate::parser::clang_types::ClangDeclKey;
    use crate::parser::clang_symbol::ClangSymbol;
    use crate::parser::clang_types::ClangSymbolKind;

    use super::{legacy_id, id_candidates, ToSymbolId};

    #[test]
    fn canonical_uses_usr() {
        let symbol = ClangSymbol {
            name: "Foo".to_string(),
            kind: ClangSymbolKind::Type,
            line: 1,
            column: 1,
            usr: Some("c:@S@Foo".to_string()),
            scope_usr: None,
        };
        assert_eq!(symbol.symbol_id().as_str(), "usr|c:@S@Foo");
    }

    #[test]
    fn legacy_id_stable() {
        let symbol = ClangSymbol {
            name: "Foo".to_string(),
            kind: ClangSymbolKind::Type,
            line: 1,
            column: 1,
            usr: None,
            scope_usr: None,
        };
        assert_eq!(legacy_id(&symbol).as_str(), "bucket|type|Foo");
    }

    #[test]
    fn candidates_include_legacy() {
        let symbol = ClangSymbol {
            name: "Value".to_string(),
            kind: ClangSymbolKind::Variable,
            line: 8,
            column: 2,
            usr: Some("c:@value".to_string()),
            scope_usr: None,
        };
        let ids = id_candidates(&symbol);
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0].as_str(), "usr|c:@value");
        assert_eq!(ids[1].as_str(), "bucket|variable|Value");
    }

    #[test]
    fn decl_key_stable() {
        let key = ClangDeclKey::new(
            "/tmp/demo.hpp".to_string(),
            12,
            4,
            ClangSymbolKind::Function,
        );
        assert_eq!(
            key.symbol_id().as_str(),
            "decl|function|/tmp/demo.hpp|12|4"
        );
    }
}
