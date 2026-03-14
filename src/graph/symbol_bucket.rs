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

pub fn symbol_id_for_clang_symbol(symbol: &ClangSymbol) -> SymbolId {
    if let Some(usr) = symbol
        .usr
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return SymbolId::new(format!("usr|{}", sanitize_id_component(usr)));
    }

    if let Some(scope_usr) = symbol
        .scope_usr
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return SymbolId::new(format!(
            "scoped|{}|{}|{}",
            kind_label(symbol.kind),
            sanitize_id_component(scope_usr),
            sanitize_id_component(symbol.name.as_str())
        ));
    }

    legacy_symbol_bucket_id_for_clang_symbol(symbol)
}

pub fn legacy_symbol_bucket_id_for_clang_symbol(symbol: &ClangSymbol) -> SymbolId {
    SymbolId::new(format!(
        "bucket|{}|{}",
        kind_label(symbol.kind),
        sanitize_id_component(symbol.name.as_str())
    ))
}

#[cfg(test)]
pub fn symbol_id_candidates_for_clang_symbol(symbol: &ClangSymbol) -> Vec<SymbolId> {
    let canonical = symbol_id_for_clang_symbol(symbol);
    let legacy = legacy_symbol_bucket_id_for_clang_symbol(symbol);
    if canonical == legacy {
        vec![canonical]
    } else {
        vec![canonical, legacy]
    }
}

pub fn symbol_id_for_decl_key(decl_key: &ClangDeclKey) -> SymbolId {
    SymbolId::new(format!(
        "decl|{}|{}|{}|{}",
        kind_label(decl_key.kind),
        sanitize_id_component(decl_key.path.as_str()),
        decl_key.line,
        decl_key.column
    ))
}

pub fn graph_node_kind_from_clang(kind: ClangSymbolKind) -> GraphNodeKind {
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

pub fn file_symbol_id(path: &Path) -> SymbolId {
    SymbolId::new(format!("file|{}", path.to_string_lossy()))
}

#[cfg(test)]
mod tests {
    use crate::parser::clang_types::ClangDeclKey;
    use crate::parser::clang_symbol::ClangSymbol;
    use crate::parser::clang_types::ClangSymbolKind;

    use super::{
        legacy_symbol_bucket_id_for_clang_symbol, symbol_id_candidates_for_clang_symbol,
        symbol_id_for_clang_symbol, symbol_id_for_decl_key,
    };

    #[test]
    fn canonical_uses_usr_when_present() {
        let symbol = ClangSymbol {
            name: "Foo".to_string(),
            kind: ClangSymbolKind::Type,
            line: 1,
            column: 1,
            usr: Some("c:@S@Foo".to_string()),
            scope_usr: None,
        };
        assert_eq!(symbol_id_for_clang_symbol(&symbol).as_str(), "usr|c:@S@Foo");
    }

    #[test]
    fn legacy_id_is_stable() {
        let symbol = ClangSymbol {
            name: "Foo".to_string(),
            kind: ClangSymbolKind::Type,
            line: 1,
            column: 1,
            usr: None,
            scope_usr: None,
        };
        assert_eq!(
            legacy_symbol_bucket_id_for_clang_symbol(&symbol).as_str(),
            "bucket|type|Foo"
        );
    }

    #[test]
    fn candidates_include_legacy_for_migration() {
        let symbol = ClangSymbol {
            name: "Value".to_string(),
            kind: ClangSymbolKind::Variable,
            line: 8,
            column: 2,
            usr: Some("c:@value".to_string()),
            scope_usr: None,
        };
        let ids = symbol_id_candidates_for_clang_symbol(&symbol);
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0].as_str(), "usr|c:@value");
        assert_eq!(ids[1].as_str(), "bucket|variable|Value");
    }

    #[test]
    fn decl_key_symbol_id_is_stable() {
        let key = ClangDeclKey::new(
            "/tmp/demo.hpp".to_string(),
            12,
            4,
            ClangSymbolKind::Function,
        );
        assert_eq!(
            symbol_id_for_decl_key(&key).as_str(),
            "decl|function|/tmp/demo.hpp|12|4"
        );
    }
}
