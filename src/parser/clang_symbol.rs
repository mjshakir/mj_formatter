use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClangSymbol {
    pub name: String,
    pub kind: i32,
    pub line: usize,
    pub column: usize,
    pub usr: Option<String>,
    pub scope_usr: Option<String>,
    pub storage_class: Option<i32>,
    pub is_const: bool,
    pub is_volatile: bool,
    pub type_kind: i32,
    pub type_display: String,
    pub canonical_type_kind: i32,
    pub template_name: Option<String>,
}

impl Default for ClangSymbol {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: 0,
            line: 0,
            column: 0,
            usr: None,
            scope_usr: None,
            storage_class: None,
            is_const: false,
            is_volatile: false,
            type_kind: clang_sys::CXType_Unexposed,
            type_display: String::new(),
            canonical_type_kind: clang_sys::CXType_Unexposed,
            template_name: None,
        }
    }
}
