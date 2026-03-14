use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[repr(u8)]
pub enum ClangSymbolKind {
    Function = 0,
    FunctionTemplate = 1,
    Method = 2,
    Constructor = 3,
    Destructor = 4,
    Variable = 5,
    Field = 6,
    Parameter = 7,
    Type = 8,
    Namespace = 9,
    Macro = 10,
    Struct = 11,
    Class = 12,
    Union = 13,
    Enum = 14,
    Typedef = 15,
    TypeAlias = 16,
    ConversionFunction = 17,
    UsingDecl = 18,
    EnumConstant = 19,
    FriendDecl = 20,
    Other = 255,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct ClangSymbolKey {
    pub name: String,
    pub kind: ClangSymbolKind,
    pub line: usize,
}

impl ClangSymbolKey {
    pub fn new(name: String, kind: ClangSymbolKind, line: usize) -> Self {
        Self { name, kind, line }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct ClangDeclKey {
    pub path: String,
    pub line: usize,
    pub column: usize,
    pub kind: ClangSymbolKind,
}

impl ClangDeclKey {
    pub fn new(path: String, line: usize, column: usize, kind: ClangSymbolKind) -> Self {
        Self {
            path,
            line,
            column,
            kind,
        }
    }
}
