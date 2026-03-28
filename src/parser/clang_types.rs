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

pub(crate) fn ensure_clang_loaded() {
    if !clang_sys::is_loaded() {
        clang_sys::load().ok();
    }
}

pub(crate) fn cxstring_to_string(cx: clang_sys::CXString) -> String {
    unsafe {
        let c_str = clang_sys::clang_getCString(cx);
        let result = if c_str.is_null() {
            String::new()
        } else {
            std::ffi::CStr::from_ptr(c_str)
                .to_str()
                .unwrap_or("")
                .to_string()
        };
        clang_sys::clang_disposeString(cx);
        result
    }
}

pub(crate) fn cxstring_to_option(cx: clang_sys::CXString) -> Option<String> {
    let s = cxstring_to_string(cx);
    if s.is_empty() { None } else { Some(s) }
}

pub fn cursor_kind_spelling(kind: i32) -> String {
    ensure_clang_loaded();
    cxstring_to_string(unsafe { clang_sys::clang_getCursorKindSpelling(kind) })
}

pub fn is_declaration_kind(kind: i32) -> bool {
    ensure_clang_loaded();
    unsafe { clang_sys::clang_isDeclaration(kind) != 0 }
}

pub fn is_preprocessing_kind(kind: i32) -> bool {
    ensure_clang_loaded();
    unsafe { clang_sys::clang_isPreprocessing(kind) != 0 }
}

pub fn graph_node_kind(kind: i32) -> crate::graph::types::GraphNodeKind {
    use crate::graph::types::GraphNodeKind;
    match kind {
        clang_sys::CXCursor_FunctionDecl
        | clang_sys::CXCursor_FunctionTemplate
        | clang_sys::CXCursor_Constructor
        | clang_sys::CXCursor_Destructor
        | clang_sys::CXCursor_ConversionFunction => GraphNodeKind::Function,
        clang_sys::CXCursor_CXXMethod => GraphNodeKind::Method,
        clang_sys::CXCursor_FieldDecl => GraphNodeKind::Field,
        clang_sys::CXCursor_ParmDecl => GraphNodeKind::Parameter,
        clang_sys::CXCursor_StructDecl
        | clang_sys::CXCursor_ClassDecl
        | clang_sys::CXCursor_UnionDecl
        | clang_sys::CXCursor_EnumDecl
        | clang_sys::CXCursor_TypedefDecl
        | clang_sys::CXCursor_TypeAliasDecl
        | clang_sys::CXCursor_TypeAliasTemplateDecl
        | clang_sys::CXCursor_ClassTemplate => GraphNodeKind::Type,
        clang_sys::CXCursor_Namespace
        | clang_sys::CXCursor_NamespaceAlias => GraphNodeKind::Namespace,
        clang_sys::CXCursor_MacroDefinition
        | clang_sys::CXCursor_MacroExpansion => GraphNodeKind::Macro,
        clang_sys::CXCursor_VarDecl => GraphNodeKind::Variable,
        _ => GraphNodeKind::Variable,
    }
}

pub fn is_function_like_kind(kind: i32) -> bool {
    matches!(
        kind,
        clang_sys::CXCursor_FunctionDecl
            | clang_sys::CXCursor_FunctionTemplate
            | clang_sys::CXCursor_CXXMethod
            | clang_sys::CXCursor_Constructor
            | clang_sys::CXCursor_Destructor
            | clang_sys::CXCursor_ConversionFunction
    )
}

pub fn is_variable_like_kind(kind: i32) -> bool {
    matches!(
        kind,
        clang_sys::CXCursor_VarDecl | clang_sys::CXCursor_FieldDecl | clang_sys::CXCursor_ParmDecl
    )
}

