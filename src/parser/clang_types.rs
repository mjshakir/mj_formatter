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


