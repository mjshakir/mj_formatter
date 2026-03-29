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

pub(crate) fn type_declaration_name(cursor: clang_sys::CXCursor) -> Option<String> {
    let ty = unsafe { clang_sys::clang_getCursorType(cursor) };
    let decl_cursor = unsafe { clang_sys::clang_getTypeDeclaration(ty) };
    if unsafe { clang_sys::clang_Cursor_isNull(decl_cursor) } != 0 {
        return None;
    }
    cxstring_to_option(unsafe { clang_sys::clang_getCursorSpelling(decl_cursor) })
}

pub(crate) fn num_template_arguments(cursor: clang_sys::CXCursor) -> i32 {
    let ty = unsafe { clang_sys::clang_getCursorType(cursor) };
    unsafe { clang_sys::clang_Type_getNumTemplateArguments(ty) }
}

