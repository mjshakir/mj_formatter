pub const ACCESS_SPECIFIER: &str = "access_specifier";
pub const BINARY_EXPRESSION: &str = "binary_expression";
pub const CATCH_CLAUSE: &str = "catch_clause";
pub const CHAR_LITERAL: &str = "char_literal";
pub const CLASS_SPECIFIER: &str = "class_specifier";
pub const COMMENT: &str = "comment";
pub const COMPOUND_STATEMENT: &str = "compound_statement";
pub const CONCATENATED_STRING: &str = "concatenated_string";
pub const DECLARATION: &str = "declaration";
pub const DECLARATION_LIST: &str = "declaration_list";
pub const DESTRUCTOR_NAME: &str = "destructor_name";
pub const FIELD_DECLARATION: &str = "field_declaration";
pub const FIELD_DECLARATION_LIST: &str = "field_declaration_list";
pub const FIELD_IDENTIFIER: &str = "field_identifier";
pub const FOR_RANGE_LOOP: &str = "for_range_loop";
pub const FOR_STATEMENT: &str = "for_statement";
pub const FUNCTION_DECLARATOR: &str = "function_declarator";
pub const FUNCTION_DEFINITION: &str = "function_definition";
pub const IDENTIFIER: &str = "identifier";
pub const IF_STATEMENT: &str = "if_statement";
pub const INIT_DECLARATOR: &str = "init_declarator";
pub const NAMESPACE_DEFINITION: &str = "namespace_definition";
pub const NAMESPACE_IDENTIFIER: &str = "namespace_identifier";
pub const NUMBER_LITERAL: &str = "number_literal";
pub const PARAMETER_DECLARATION: &str = "parameter_declaration";
pub const PARAMETER_LIST: &str = "parameter_list";
pub const PREPROC_CALL: &str = "preproc_call";
#[cfg(test)]
pub const PREPROC_IF: &str = "preproc_if";
#[cfg(test)]
pub const PREPROC_INCLUDE: &str = "preproc_include";
pub const PRIMITIVE_TYPE: &str = "primitive_type";
pub const RAW_STRING_LITERAL: &str = "raw_string_literal";
pub const SIZED_TYPE_SPECIFIER: &str = "sized_type_specifier";
pub const STRING_LITERAL: &str = "string_literal";
pub const STRUCT_SPECIFIER: &str = "struct_specifier";
pub const SWITCH_STATEMENT: &str = "switch_statement";
pub const SYSTEM_LIB_STRING: &str = "system_lib_string";
pub const TEMPLATE_PARAMETER_LIST: &str = "template_parameter_list";
pub const TYPE_IDENTIFIER: &str = "type_identifier";
pub const WHILE_STATEMENT: &str = "while_statement";

pub fn is_string_like(kind: &str) -> bool {
    matches!(
        kind,
        STRING_LITERAL | RAW_STRING_LITERAL | CHAR_LITERAL | SYSTEM_LIB_STRING | CONCATENATED_STRING
    )
}
