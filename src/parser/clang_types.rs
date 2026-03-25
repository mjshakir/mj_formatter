use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct ClangSymbolKey {
    pub name: String,
    pub kind: i32,
    pub line: usize,
}

impl ClangSymbolKey {
    pub fn new(name: String, kind: i32, line: usize) -> Self {
        Self { name, kind, line }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct ClangDeclKey {
    pub path: String,
    pub line: usize,
    pub column: usize,
    pub kind: i32,
}

impl ClangDeclKey {
    pub fn new(path: String, line: usize, column: usize, kind: i32) -> Self {
        Self {
            path,
            line,
            column,
            kind,
        }
    }
}

pub fn graph_node_kind(kind: i32) -> crate::graph::types::GraphNodeKind {
    use crate::graph::types::GraphNodeKind;
    match kind {
        clang_sys::CXCursor_FunctionDecl
        | clang_sys::CXCursor_FunctionTemplate
        | clang_sys::CXCursor_CXXMethod
        | clang_sys::CXCursor_Constructor
        | clang_sys::CXCursor_Destructor
        | clang_sys::CXCursor_ConversionFunction => GraphNodeKind::Function,
        clang_sys::CXCursor_VarDecl => GraphNodeKind::Variable,
        clang_sys::CXCursor_FieldDecl => GraphNodeKind::Field,
        clang_sys::CXCursor_ParmDecl => GraphNodeKind::Parameter,
        clang_sys::CXCursor_TypedefDecl
        | clang_sys::CXCursor_StructDecl
        | clang_sys::CXCursor_ClassDecl
        | clang_sys::CXCursor_UnionDecl
        | clang_sys::CXCursor_EnumDecl
        | clang_sys::CXCursor_TypeAliasDecl => GraphNodeKind::Type,
        clang_sys::CXCursor_Namespace => GraphNodeKind::Namespace,
        clang_sys::CXCursor_MacroDefinition => GraphNodeKind::Macro,
        clang_sys::CXCursor_UsingDeclaration
        | clang_sys::CXCursor_EnumConstantDecl
        | clang_sys::CXCursor_FriendDecl => GraphNodeKind::Variable,
        _ => GraphNodeKind::Variable,
    }
}

pub fn entity_kind_label(kind: i32, detailed: bool) -> &'static str {
    match kind {
        clang_sys::CXCursor_FunctionDecl | clang_sys::CXCursor_FunctionTemplate => "function",
        clang_sys::CXCursor_CXXMethod => "method",
        clang_sys::CXCursor_Constructor => "constructor",
        clang_sys::CXCursor_Destructor => "destructor",
        clang_sys::CXCursor_VarDecl => "variable",
        clang_sys::CXCursor_FieldDecl => "field",
        clang_sys::CXCursor_ParmDecl => "parameter",
        clang_sys::CXCursor_StructDecl => {
            if detailed {
                "struct"
            } else {
                "type"
            }
        }
        clang_sys::CXCursor_ClassDecl => {
            if detailed {
                "class"
            } else {
                "type"
            }
        }
        clang_sys::CXCursor_UnionDecl => {
            if detailed {
                "union"
            } else {
                "type"
            }
        }
        clang_sys::CXCursor_EnumDecl => {
            if detailed {
                "enum"
            } else {
                "type"
            }
        }
        clang_sys::CXCursor_TypedefDecl => {
            if detailed {
                "typedef"
            } else {
                "type"
            }
        }
        clang_sys::CXCursor_TypeAliasDecl => {
            if detailed {
                "type_alias"
            } else {
                "type"
            }
        }
        clang_sys::CXCursor_Namespace => "namespace",
        clang_sys::CXCursor_MacroDefinition => "macro",
        clang_sys::CXCursor_ConversionFunction => "conversion_function",
        clang_sys::CXCursor_UsingDeclaration => "using_decl",
        clang_sys::CXCursor_EnumConstantDecl => "enum_constant",
        clang_sys::CXCursor_FriendDecl => "friend_decl",
        _ => "other",
    }
}
