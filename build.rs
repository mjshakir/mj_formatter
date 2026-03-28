/// Build script that extracts tree-sitter-cpp's native C symbol constants
/// from parser.c and generates Rust `pub const` bindings.
///
/// When tree-sitter-cpp updates, the constants regenerate automatically.
/// Renamed/removed symbols cause compile errors at the exact usage sites.
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

fn main() {
    let parser_c = find_parser_c();
    println!("cargo:rerun-if-changed={}", parser_c.display());
    println!("cargo:rerun-if-changed=Cargo.lock");

    let source = fs::read_to_string(&parser_c)
        .unwrap_or_else(|e| panic!("failed reading {}: {e}", parser_c.display()));

    let symbols = extract_enum(&source, "enum ts_symbol_identifiers");
    let fields = extract_enum(&source, "enum ts_field_identifiers");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let out_path = out_dir.join("ts_cpp_symbols.rs");
    let mut out = fs::File::create(&out_path).expect("create output file");

    writeln!(out, "// Auto-generated from tree-sitter-cpp parser.c — do not edit.").unwrap();
    writeln!(out).unwrap();

    writeln!(out, "// Node kind symbols (from enum ts_symbol_identifiers)").unwrap();
    for (name, value) in &symbols {
        writeln!(out, "pub const {name}: u16 = {value};").unwrap();
    }
    writeln!(out).unwrap();
    writeln!(out, "pub const SYMBOL_COUNT: usize = {};", symbols.len()).unwrap();
    writeln!(out).unwrap();

    writeln!(out, "// Field identifiers (from enum ts_field_identifiers)").unwrap();
    for (name, value) in &fields {
        writeln!(out, "pub const {name}: u16 = {value};").unwrap();
    }
    writeln!(out).unwrap();
    writeln!(out, "pub const FIELD_COUNT: usize = {};", fields.len()).unwrap();
    writeln!(out).unwrap();

    // Generate is_preproc() from all sym_preproc_* symbols
    let preproc_syms: Vec<&String> = symbols
        .keys()
        .filter(|name| name.starts_with("sym_preproc_"))
        .collect();
    writeln!(out, "#[inline]").unwrap();
    writeln!(out, "pub fn is_preproc(kind_id: u16) -> bool {{").unwrap();
    writeln!(out, "    matches!(kind_id, {}", preproc_syms[0]).unwrap();
    for sym in &preproc_syms[1..] {
        writeln!(out, "        | {sym}").unwrap();
    }
    writeln!(out, "    )").unwrap();
    writeln!(out, "}}").unwrap();
    writeln!(out).unwrap();

    // Generate is_string_like() from string/char/concatenated literal symbols
    let string_like_syms: Vec<&String> = symbols
        .keys()
        .filter(|name| {
            matches!(
                name.as_str(),
                "sym_string_literal"
                    | "sym_raw_string_literal"
                    | "sym_char_literal"
                    | "sym_system_lib_string"
                    | "sym_concatenated_string"
            )
        })
        .collect();
    if !string_like_syms.is_empty() {
        writeln!(out, "#[inline]").unwrap();
        writeln!(out, "pub fn is_string_like(kind_id: u16) -> bool {{").unwrap();
        writeln!(out, "    matches!(kind_id, {}", string_like_syms[0]).unwrap();
        for sym in &string_like_syms[1..] {
            writeln!(out, "        | {sym}").unwrap();
        }
        writeln!(out, "    )").unwrap();
        writeln!(out, "}}").unwrap();
    }
    writeln!(out).unwrap();

    // Generate is_comment() for comment node
    emit_static_helper(&mut out, &symbols, "is_comment", &["sym_comment"]);

    // Generate is_identifier_like() from all identifier aliases + sym_identifier
    emit_static_helper(
        &mut out,
        &symbols,
        "is_identifier_like",
        &[
            "sym_identifier",
            "alias_sym_field_identifier",
            "alias_sym_namespace_identifier",
            "alias_sym_type_identifier",
            "alias_sym_statement_identifier",
        ],
    );

    // Generate is_type_specifier() from type-related nodes
    emit_static_helper(
        &mut out,
        &symbols,
        "is_type_specifier",
        &[
            "sym_primitive_type",
            "sym_sized_type_specifier",
            "alias_sym_type_identifier",
            "sym_type_specifier",
            "sym_class_specifier",
            "sym_struct_specifier",
            "sym_union_specifier",
            "sym_enum_specifier",
        ],
    );

    // Generate is_compound_body() from body-like compound nodes
    emit_static_helper(
        &mut out,
        &symbols,
        "is_compound_body",
        &[
            "sym_compound_statement",
            "sym_declaration_list",
            "sym_field_declaration_list",
        ],
    );
}

/// Emit a `pub fn $name(kind_id: u16) -> bool` that matches the given symbol names.
fn emit_static_helper(
    out: &mut std::fs::File,
    symbols: &BTreeMap<String, u32>,
    fn_name: &str,
    sym_names: &[&str],
) {
    let present: Vec<&&str> = sym_names
        .iter()
        .filter(|name| symbols.contains_key(**name))
        .collect();
    if present.is_empty() {
        return;
    }
    writeln!(out, "#[inline]").unwrap();
    writeln!(out, "pub fn {fn_name}(kind_id: u16) -> bool {{").unwrap();
    writeln!(out, "    matches!(kind_id, {}", present[0]).unwrap();
    for sym in &present[1..] {
        writeln!(out, "        | {sym}").unwrap();
    }
    writeln!(out, "    )").unwrap();
    writeln!(out, "}}").unwrap();
    writeln!(out).unwrap();
}

/// Parse a C enum block from the source into name → value pairs.
fn extract_enum(source: &str, enum_header: &str) -> BTreeMap<String, u32> {
    let mut entries = BTreeMap::new();
    let Some(start) = source.find(enum_header) else {
        panic!("{enum_header} not found in parser.c");
    };
    let Some(brace) = source[start..].find('{') else {
        panic!("opening brace not found for {enum_header}");
    };
    let block_start = start + brace + 1;
    let Some(block_end_offset) = source[block_start..].find("};") else {
        panic!("closing brace not found for {enum_header}");
    };
    let block = &source[block_start..block_start + block_end_offset];

    for line in block.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        // Parse: "sym_identifier = 1," or "field_name = 28,"
        let trimmed = trimmed.trim_end_matches(',');
        let Some((name, value_str)) = trimmed.split_once('=') else {
            continue;
        };
        let name = name.trim().to_string();
        let value: u32 = value_str.trim().parse().unwrap_or_else(|e| {
            panic!("failed parsing value for {name}: {value_str:?}: {e}");
        });
        entries.insert(name, value);
    }

    if entries.is_empty() {
        panic!("{enum_header} parsed but found no entries");
    }
    entries
}

/// Locate tree-sitter-cpp's parser.c in the cargo registry.
fn find_parser_c() -> PathBuf {
    let version = find_ts_cpp_version();
    let cargo_home = env::var("CARGO_HOME")
        .unwrap_or_else(|_| {
            let home = env::var("HOME").expect("neither CARGO_HOME nor HOME set");
            format!("{home}/.cargo")
        });

    let registry_src = Path::new(&cargo_home).join("registry").join("src");
    let pattern = format!("tree-sitter-cpp-{version}");

    // Walk registry/src/*/tree-sitter-cpp-{version}/src/parser.c
    if let Ok(entries) = fs::read_dir(&registry_src) {
        for entry in entries.flatten() {
            let candidate = entry.path().join(&pattern).join("src").join("parser.c");
            if candidate.exists() {
                return candidate;
            }
        }
    }

    panic!(
        "tree-sitter-cpp {version} parser.c not found in {}/registry/src/*/",
        cargo_home
    );
}

/// Extract tree-sitter-cpp version from Cargo.lock.
fn find_ts_cpp_version() -> String {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let lock_path = Path::new(&manifest_dir).join("Cargo.lock");
    let lock_content =
        fs::read_to_string(&lock_path).expect("failed reading Cargo.lock");

    let mut in_ts_cpp = false;
    for line in lock_content.lines() {
        let trimmed = line.trim();
        if trimmed == r#"name = "tree-sitter-cpp""# {
            in_ts_cpp = true;
            continue;
        }
        if in_ts_cpp {
            if let Some(version) = trimmed.strip_prefix("version = \"") {
                if let Some(version) = version.strip_suffix('"') {
                    return version.to_string();
                }
            }
            in_ts_cpp = false;
        }
    }

    panic!("tree-sitter-cpp version not found in Cargo.lock");
}
