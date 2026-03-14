use crate::parser::clang_types::ClangSymbolKind;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClangSymbol {
    pub name: String,
    pub kind: ClangSymbolKind,
    pub line: usize,
    pub column: usize,
    pub usr: Option<String>,
    pub scope_usr: Option<String>,
}
