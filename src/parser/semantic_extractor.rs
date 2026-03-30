use rustc_hash::{FxHashMap, FxHashSet};
use std::fs;
use std::path::Path;

use anyhow::Result;

use crate::parser::clang_types::{cxstring_to_option, cxstring_to_string, ClangDeclKey};
use crate::parser::clang_result::{
    ClangDiagnosticEntry, DiagnosticCounts,
};
use crate::parser::clang_types::ClangSymbolKey;
use crate::parser::file_context::{SemanticDeclaration, SemanticFileContext};


struct SpellingLocation {
    file: clang_sys::CXFile,
    line: u32,
    column: u32,
    offset: u32,
}

fn get_spelling_location(loc: clang_sys::CXSourceLocation) -> SpellingLocation {
    let mut file = std::ptr::null_mut();
    let (mut line, mut column, mut offset) = (0u32, 0u32, 0u32);
    unsafe {
        clang_sys::clang_getSpellingLocation(loc, &mut file, &mut line, &mut column, &mut offset);
    }
    SpellingLocation {
        file,
        line,
        column,
        offset,
    }
}

fn visit_children(
    cursor: clang_sys::CXCursor,
    mut callback: impl FnMut(clang_sys::CXCursor, clang_sys::CXCursor) -> clang_sys::CXChildVisitResult,
) {
    extern "C" fn visit_trampoline(
        cursor: clang_sys::CXCursor,
        parent: clang_sys::CXCursor,
        client_data: clang_sys::CXClientData,
    ) -> clang_sys::CXChildVisitResult {
        unsafe {
            let callback =
                &mut *(client_data as *mut &mut dyn FnMut(clang_sys::CXCursor, clang_sys::CXCursor) -> clang_sys::CXChildVisitResult);
            callback(cursor, parent)
        }
    }

    let mut visitor: &mut dyn FnMut(clang_sys::CXCursor, clang_sys::CXCursor) -> clang_sys::CXChildVisitResult =
        &mut callback;
    unsafe {
        clang_sys::clang_visitChildren(
            cursor,
            visit_trampoline,
            &mut visitor as *mut _ as clang_sys::CXClientData,
        );
    }
}

fn find_references_in_file(
    cursor: clang_sys::CXCursor,
    file: clang_sys::CXFile,
    callback: &mut impl FnMut(clang_sys::CXCursor, clang_sys::CXSourceRange),
) {
    extern "C" fn ref_visitor(
        context: *mut std::ffi::c_void,
        cursor: clang_sys::CXCursor,
        range: clang_sys::CXSourceRange,
    ) -> clang_sys::CXVisitorResult {
        unsafe {
            let callback = &mut *(context
                as *mut &mut dyn FnMut(clang_sys::CXCursor, clang_sys::CXSourceRange));
            callback(cursor, range);
        }
        clang_sys::CXVisit_Continue
    }

    let mut cb: &mut dyn FnMut(clang_sys::CXCursor, clang_sys::CXSourceRange) = callback;
    let visitor = clang_sys::CXCursorAndRangeVisitor {
        context: &mut cb as *mut _ as *mut std::ffi::c_void,
        visit: Some(ref_visitor),
    };
    unsafe {
        clang_sys::clang_findReferencesInFile(cursor, file, visitor);
    }
}

pub(crate) struct SemanticExtractor;

impl SemanticExtractor {
    pub(crate) fn extract_diagnostics(
        tu: clang_sys::CXTranslationUnit,
    ) -> (
        bool,
        Vec<String>,
        DiagnosticCounts,
        Vec<ClangDiagnosticEntry>,
    ) {
        let mut success = true;
        let mut diagnostics = Vec::new();
        let mut diagnostic_counts: DiagnosticCounts = [0; 5];
        let mut diagnostic_entries = Vec::new();

        let num_diags = unsafe { clang_sys::clang_getNumDiagnostics(tu) };
        for i in 0..num_diags {
            let diag = unsafe { clang_sys::clang_getDiagnostic(tu, i) };
            let severity = unsafe { clang_sys::clang_getDiagnosticSeverity(diag) };

            if matches!(
                severity,
                clang_sys::CXDiagnostic_Error | clang_sys::CXDiagnostic_Fatal
            ) {
                success = false;
            }

            let clamped = (severity as usize).min(clang_sys::CXDiagnostic_Fatal as usize);
            diagnostic_counts[clamped] = diagnostic_counts[clamped].saturating_add(1);
            let mapped_severity = clamped as u32;

            let loc = unsafe { clang_sys::clang_getDiagnosticLocation(diag) };
            let mut presumed_file = clang_sys::CXString::default();
            let (mut presumed_line, mut presumed_column) = (0u32, 0u32);
            unsafe {
                clang_sys::clang_getPresumedLocation(
                    loc,
                    &mut presumed_file,
                    &mut presumed_line,
                    &mut presumed_column,
                );
            }
            let file_str = cxstring_to_string(presumed_file);
            let text = cxstring_to_string(unsafe { clang_sys::clang_getDiagnosticSpelling(diag) });

            let warning_option = {
                let mut disable = clang_sys::CXString::default();
                let enable = unsafe { clang_sys::clang_getDiagnosticOption(diag, &mut disable) };
                let s = cxstring_to_string(enable);
                cxstring_to_string(disable);
                s
            };

            let mut fix_its = Vec::new();
            let num_fix_its = unsafe { clang_sys::clang_getDiagnosticNumFixIts(diag) };
            for fi in 0..num_fix_its {
                let mut range = clang_sys::CXSourceRange::default();
                let replacement_cx = unsafe { clang_sys::clang_getDiagnosticFixIt(diag, fi, &mut range) };
                let replacement = cxstring_to_string(replacement_cx);
                let start = unsafe { clang_sys::clang_getRangeStart(range) };
                let end = unsafe { clang_sys::clang_getRangeEnd(range) };
                let (mut sl, mut sc, mut el, mut ec) = (0u32, 0u32, 0u32, 0u32);
                let mut dummy_offset = 0u32;
                unsafe {
                    clang_sys::clang_getSpellingLocation(start, std::ptr::null_mut(), &mut sl, &mut sc, &mut dummy_offset);
                    clang_sys::clang_getSpellingLocation(end, std::ptr::null_mut(), &mut el, &mut ec, &mut dummy_offset);
                }
                fix_its.push(crate::parser::clang_result::ClangFixIt {
                    replacement,
                    start_line: sl as usize,
                    start_column: sc as usize,
                    end_line: el as usize,
                    end_column: ec as usize,
                });
            }

            diagnostic_entries.push(ClangDiagnosticEntry {
                line: presumed_line as usize,
                column: presumed_column as usize,
                severity: mapped_severity,
                warning_option,
                fix_its,
            });

            let severity_label = match severity {
                clang_sys::CXDiagnostic_Ignored => "Ignored",
                clang_sys::CXDiagnostic_Note => "Note",
                clang_sys::CXDiagnostic_Warning => "Warning",
                clang_sys::CXDiagnostic_Error => "Error",
                _ => "Fatal",
            };
            diagnostics.push(format!(
                "{}:{}:{}: {}: {}",
                file_str, presumed_line, presumed_column, severity_label, text
            ));

            unsafe {
                clang_sys::clang_disposeDiagnostic(diag);
            }
        }
        (success, diagnostics, diagnostic_counts, diagnostic_entries)
    }

    #[allow(clippy::type_complexity)]
    pub(crate) fn extract_symbols_and_offsets(
        tu: clang_sys::CXTranslationUnit,
        source_path: &str,
    ) -> Result<(
        Vec<SemanticDeclaration>,
        FxHashMap<ClangSymbolKey, Vec<usize>>,
        FxHashMap<ClangDeclKey, Vec<usize>>,
    )> {
        let canonical_path = Self::normalize_path_for_key(Path::new(source_path));
        let root_cursor = unsafe { clang_sys::clang_getTranslationUnitCursor(tu) };
        let mut seen = FxHashSet::default();
        let mut symbols = Vec::new();
        let mut symbol_cursors = Vec::new();
        Self::collect_symbols(
            root_cursor,
            &canonical_path,
            &mut seen,
            &mut symbols,
            &mut symbol_cursors,
        );
        symbols.sort_by(|left, right| {
            left.line
                .cmp(&right.line)
                .then(left.column.cmp(&right.column))
                .then(left.name.cmp(&right.name))
        });

        let c_source = std::ffi::CString::new(source_path).unwrap();
        let file = unsafe { clang_sys::clang_getFile(tu, c_source.as_ptr()) };
        let rename_offsets_by_symbol =
            Self::collect_rename_offsets(file, &symbol_cursors);
        let reference_offsets_by_decl = Self::collect_reference_offsets(root_cursor);

        Ok((symbols, rename_offsets_by_symbol, reference_offsets_by_decl))
    }

    fn collect_symbols(
        cursor: clang_sys::CXCursor,
        canonical_path: &str,
        seen: &mut FxHashSet<(String, Option<String>, i32, usize, usize)>,
        symbols: &mut Vec<SemanticDeclaration>,
        symbol_cursors: &mut Vec<(SemanticDeclaration, clang_sys::CXCursor)>,
    ) {
        visit_children(cursor, |child, _parent| {
            if let Some(decl) = Self::decl_from_cursor(child, canonical_path) {
                let key = (
                    decl.name.clone(),
                    decl.usr.clone(),
                    decl.kind,
                    decl.line,
                    decl.column,
                );
                if seen.insert(key) {
                    symbols.push(decl.clone());
                    symbol_cursors.push((decl, child));
                }
            }
            clang_sys::CXChildVisit_Recurse
        });
    }

    fn collect_rename_offsets(
        file: clang_sys::CXFile,
        symbol_cursors: &[(SemanticDeclaration, clang_sys::CXCursor)],
    ) -> FxHashMap<ClangSymbolKey, Vec<usize>> {
        if file.is_null() {
            return FxHashMap::default();
        }

        let mut rename_offsets_by_symbol = FxHashMap::default();
        for (decl, cursor) in symbol_cursors {
            let mut offsets = FxHashSet::default();

            let loc = unsafe { clang_sys::clang_getCursorLocation(*cursor) };
            let spelling = get_spelling_location(loc);
            if !spelling.file.is_null()
                && spelling.line > 0
                && spelling.column > 0
                && unsafe { clang_sys::clang_Location_isFromMainFile(loc) != 0 }
            {
                offsets.insert(spelling.offset as usize);
            }

            find_references_in_file(*cursor, file, &mut |ref_cursor, _range| {
                let ref_loc = unsafe { clang_sys::clang_getCursorLocation(ref_cursor) };
                if unsafe { clang_sys::clang_Location_isFromMainFile(ref_loc) == 0 } {
                    return;
                }
                let ref_spelling = get_spelling_location(ref_loc);
                if !ref_spelling.file.is_null()
                    && ref_spelling.line > 0
                    && ref_spelling.column > 0
                {
                    offsets.insert(ref_spelling.offset as usize);
                }
            });

            if offsets.is_empty() {
                continue;
            }

            let key = ClangSymbolKey::new(decl.name.clone(), decl.kind, decl.line);
            let entry = rename_offsets_by_symbol.entry(key).or_insert_with(Vec::new);
            entry.extend(offsets.into_iter());
            entry.sort_unstable();
            entry.dedup();
        }

        rename_offsets_by_symbol
    }

    fn collect_reference_offsets(
        cursor: clang_sys::CXCursor,
    ) -> FxHashMap<ClangDeclKey, Vec<usize>> {
        let mut offsets_by_decl: FxHashMap<ClangDeclKey, Vec<usize>> = FxHashMap::default();
        let mut path_cache: FxHashMap<std::path::PathBuf, String> = FxHashMap::default();

        visit_children(cursor, |child, _parent| {
            let referenced = unsafe { clang_sys::clang_getCursorReferenced(child) };
            if unsafe { clang_sys::clang_Cursor_isNull(referenced) != 0 } {
                return clang_sys::CXChildVisit_Recurse;
            }
            let child_loc = unsafe { clang_sys::clang_getCursorLocation(child) };
            if unsafe { clang_sys::clang_Location_isFromMainFile(child_loc) == 0 } {
                return clang_sys::CXChildVisit_Recurse;
            }
            let child_spelling = get_spelling_location(child_loc);
            if child_spelling.file.is_null()
                || child_spelling.line == 0
                || child_spelling.column == 0
            {
                return clang_sys::CXChildVisit_Recurse;
            }
            let Some(decl_key) =
                Self::decl_key_from_cursor_cached(referenced, &mut path_cache)
            else {
                return clang_sys::CXChildVisit_Recurse;
            };

            offsets_by_decl
                .entry(decl_key)
                .or_default()
                .push(child_spelling.offset as usize);
            clang_sys::CXChildVisit_Recurse
        });

        for offsets in offsets_by_decl.values_mut() {
            offsets.sort_unstable();
            offsets.dedup();
        }

        offsets_by_decl
    }

    fn decl_key_from_cursor_cached(
        cursor: clang_sys::CXCursor,
        path_cache: &mut FxHashMap<std::path::PathBuf, String>,
    ) -> Option<ClangDeclKey> {
        let kind = unsafe { clang_sys::clang_getCursorKind(cursor) } as i32;
        if !Self::is_relevant_kind(kind) {
            return None;
        }
        let loc = unsafe { clang_sys::clang_getCursorLocation(cursor) };
        let spelling = get_spelling_location(loc);
        if spelling.file.is_null() {
            return None;
        }
        let raw_path = std::path::PathBuf::from(cxstring_to_string(unsafe {
            clang_sys::clang_getFileName(spelling.file)
        }));
        let path = path_cache
            .entry(raw_path.clone())
            .or_insert_with(|| Self::normalize_path_for_key(&raw_path))
            .clone();
        let line = spelling.line as usize;
        let column = spelling.column as usize;
        if line == 0 || column == 0 {
            return None;
        }

        Some(ClangDeclKey::new(path, line, column, kind))
    }

    fn normalize_path_for_key(path: &Path) -> String {
        fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .to_string()
    }

    fn decl_from_cursor(
        cursor: clang_sys::CXCursor,
        canonical_path: &str,
    ) -> Option<SemanticDeclaration> {
        let kind = unsafe { clang_sys::clang_getCursorKind(cursor) } as i32;
        if !Self::is_relevant_kind(kind) {
            return None;
        }
        let name = cxstring_to_option(unsafe { clang_sys::clang_getCursorSpelling(cursor) })
            .or_else(|| {
                cxstring_to_option(unsafe { clang_sys::clang_getCursorDisplayName(cursor) })
            })
            .unwrap_or_default();
        if name.is_empty() {
            return None;
        }

        let source_location = unsafe { clang_sys::clang_getCursorLocation(cursor) };
        if unsafe { clang_sys::clang_Location_isFromMainFile(source_location) == 0 } {
            return None;
        }
        let location = get_spelling_location(source_location);
        let line = location.line as usize;
        let column = location.column as usize;
        if line == 0 || column == 0 {
            return None;
        }

        let canonical_type_kind = if unsafe { clang_sys::clang_isDeclaration(kind) != 0 } {
            let ty = unsafe { clang_sys::clang_getCursorType(cursor) };
            let canonical = unsafe { clang_sys::clang_getCanonicalType(ty) };
            canonical.kind as i32
        } else {
            clang_sys::CXType_Unexposed
        };

        let usr = cxstring_to_option(unsafe { clang_sys::clang_getCursorUSR(cursor) })
            .and_then(|raw| {
                let trimmed = raw.trim();
                (!trimmed.is_empty()).then_some(trimmed.to_string())
            });
        let semantic_parent = unsafe { clang_sys::clang_getCursorSemanticParent(cursor) };
        let scope_usr = cxstring_to_option(unsafe { clang_sys::clang_getCursorUSR(semantic_parent) })
            .and_then(|raw| {
                let trimmed = raw.trim();
                (!trimmed.is_empty()).then_some(trimmed.to_string())
            });

        let definition = unsafe { clang_sys::clang_getCursorDefinition(cursor) };
        let is_definition = unsafe { !clang_sys::clang_Cursor_isNull(definition) != 0 }
            && unsafe { clang_sys::clang_equalCursors(cursor, definition) != 0 };

        let is_anonymous = unsafe { clang_sys::clang_Cursor_isAnonymous(cursor) != 0 };

        let lexical_parent = unsafe { clang_sys::clang_getCursorLexicalParent(cursor) };
        let lexical_parent_usr =
            cxstring_to_option(unsafe { clang_sys::clang_getCursorUSR(lexical_parent) })
                .and_then(|raw| {
                    let trimmed = raw.trim();
                    (!trimmed.is_empty()).then_some(trimmed.to_string())
                });

        let pointee_type_kind = if unsafe { clang_sys::clang_isDeclaration(kind) != 0 } {
            let ty = unsafe { clang_sys::clang_getCursorType(cursor) };
            let pointee = unsafe { clang_sys::clang_getPointeeType(ty) };
            if pointee.kind != clang_sys::CXType_Invalid {
                Some(pointee.kind as i32)
            } else {
                None
            }
        } else {
            None
        };

        let storage_class = if unsafe { clang_sys::clang_isDeclaration(kind) != 0 } {
            unsafe { clang_sys::clang_Cursor_getStorageClass(cursor) }
        } else {
            clang_sys::CX_SC_Invalid
        };

        let (is_const_qualified, is_volatile_qualified) = if unsafe { clang_sys::clang_isDeclaration(kind) != 0 } {
            let ty = unsafe { clang_sys::clang_getCursorType(cursor) };
            let canonical = unsafe { clang_sys::clang_getCanonicalType(ty) };
            (
                unsafe { clang_sys::clang_isConstQualifiedType(canonical) } != 0,
                unsafe { clang_sys::clang_isVolatileQualifiedType(canonical) } != 0,
            )
        } else {
            (false, false)
        };

        let type_spelling = if unsafe { clang_sys::clang_isDeclaration(kind) != 0 } {
            let ty = unsafe { clang_sys::clang_getCursorType(cursor) };
            cxstring_to_option(unsafe { clang_sys::clang_getTypeSpelling(ty) })
        } else {
            None
        };

        let semantic_parent_kind =
            unsafe { clang_sys::clang_getCursorKind(semantic_parent) } as i32;

        let num_template_args = {
            let ty = unsafe { clang_sys::clang_getCursorType(cursor) };
            unsafe { clang_sys::clang_Type_getNumTemplateArguments(ty) }
        };
        let template_base_name = if num_template_args > 0 {
            let ty = unsafe { clang_sys::clang_getCursorType(cursor) };
            let decl_cursor = unsafe { clang_sys::clang_getTypeDeclaration(ty) };
            if unsafe { clang_sys::clang_Cursor_isNull(decl_cursor) } != 0 {
                None
            } else {
                cxstring_to_option(unsafe { clang_sys::clang_getCursorSpelling(decl_cursor) })
            }
        } else {
            None
        };

        let (stable_id, provenance) = SemanticFileContext::stable_id_for_decl(
            canonical_path,
            &name,
            kind,
            line,
            usr.as_deref(),
            scope_usr.as_deref(),
        );

        Some(SemanticDeclaration {
            stable_id,
            provenance,
            name,
            kind,
            line,
            column,
            usr,
            scope_usr,
            canonical_type_kind,
            is_definition,
            is_anonymous,
            lexical_parent_usr,
            pointee_type_kind,
            storage_class,
            is_const_qualified,
            is_volatile_qualified,
            template_base_name,
            num_template_args,
            type_spelling,
            semantic_parent_kind,
        })
    }

    pub(crate) fn is_relevant_kind(kind: i32) -> bool {
        (unsafe { clang_sys::clang_isDeclaration(kind) != 0 })
            || (unsafe { clang_sys::clang_isPreprocessing(kind) != 0 })
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::clang_result::ClangParseResult;
    use crate::parser::clang_service::ClangParseService;
    use crate::parser::clang_types::ensure_clang_loaded;

    #[test]
    fn tu_save_load_round_trip() {
        let cx_index = ClangParseService::test_index();
        ensure_clang_loaded();

        let source = r#"
namespace ns {
    struct Foo {
        int value;
        void bar(int x) const;
    };
    int global_var = 42;
}
"#;
        let source_path = "/tmp/test_tu_round_trip.cpp";
        std::fs::write(source_path, source).unwrap();

        // Parse with CXUnsavedFile (production path)
        let c_source = std::ffi::CString::new(source_path).unwrap();
        let c_text = std::ffi::CString::new(source).unwrap();
        let c_args: Vec<std::ffi::CString> = ["-std=c++17", "-x", "c++"]
            .iter()
            .map(|a| std::ffi::CString::new(*a).unwrap())
            .collect();
        let c_arg_ptrs: Vec<*const std::ffi::c_char> =
            c_args.iter().map(|a| a.as_ptr()).collect();

        let unsaved = clang_sys::CXUnsavedFile {
            Filename: c_source.as_ptr(),
            Contents: c_text.as_ptr(),
            Length: source.len() as std::ffi::c_ulong,
        };

        let mut tu: clang_sys::CXTranslationUnit = std::ptr::null_mut();
        let err = unsafe {
            clang_sys::clang_parseTranslationUnit2(
                cx_index,
                c_source.as_ptr(),
                c_arg_ptrs.as_ptr(),
                c_arg_ptrs.len() as std::ffi::c_int,
                &unsaved as *const _ as *mut _,
                1,
                clang_sys::CXTranslationUnit_DetailedPreprocessingRecord,
                &mut tu,
            )
        };
        assert_eq!(err, clang_sys::CXError_Success, "parse failed");
        assert!(!tu.is_null());

        // Extract using the production functions directly
        let (success, diagnostics, diagnostic_summary, diagnostic_entries) =
            SemanticExtractor::extract_diagnostics(tu);
        let (symbols, rename_offsets, reference_offsets) =
            SemanticExtractor::extract_symbols_and_offsets(tu, source_path)
                .expect("extract symbols");

        let original = ClangParseResult::with_semantic_offsets(
            success,
            diagnostics,
            symbols,
            rename_offsets,
            reference_offsets,
            diagnostic_summary,
            diagnostic_entries,
        );

        // Save TU to tempfile
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let c_tmp_path = std::ffi::CString::new(tmp.path().to_str().unwrap()).unwrap();
        let save_err = unsafe {
            clang_sys::clang_saveTranslationUnit(
                tu,
                c_tmp_path.as_ptr(),
                clang_sys::CXSaveTranslationUnit_None,
            )
        };
        assert_eq!(save_err, 0, "save failed");

        let tu_bytes = std::fs::read(tmp.path()).unwrap();
        assert!(
            tu_bytes.len() > 100,
            "TU binary too small: {} bytes",
            tu_bytes.len()
        );

        unsafe {
            clang_sys::clang_disposeTranslationUnit(tu);
        }

        // Load TU from saved file
        let loaded_tu = unsafe {
            clang_sys::clang_createTranslationUnit(cx_index, c_tmp_path.as_ptr())
        };
        assert!(!loaded_tu.is_null(), "load TU from AST failed");

        // Extract from loaded TU using the same production path
        let (loaded_symbols, _, _) =
            SemanticExtractor::extract_symbols_and_offsets(loaded_tu, source_path)
                .expect("extract from loaded TU");

        assert!(!loaded_symbols.is_empty(), "loaded TU should have symbols");
        assert_eq!(
            original.symbols.len(),
            loaded_symbols.len(),
            "symbol count mismatch: original={}, loaded={}",
            original.symbols.len(),
            loaded_symbols.len()
        );

        for (orig, loaded) in original.symbols.iter().zip(loaded_symbols.iter()) {
            assert_eq!(orig.name, loaded.name, "name mismatch at line {}", orig.line);
            assert_eq!(orig.kind, loaded.kind, "kind mismatch for {}", orig.name);
            assert_eq!(orig.line, loaded.line, "line mismatch for {}", orig.name);
        }

        // Verify full cursor API works on loaded TU
        let root = unsafe { clang_sys::clang_getTranslationUnitCursor(loaded_tu) };
        let mut found_foo = false;
        visit_children(root, |child, _| {
            let name = cxstring_to_option(unsafe { clang_sys::clang_getCursorSpelling(child) });
            if name.as_deref() == Some("Foo") {
                found_foo = true;
                let ty = unsafe { clang_sys::clang_getCursorType(child) };
                assert!(ty.kind != clang_sys::CXType_Invalid, "Foo should have a type");
                let usr = cxstring_to_option(unsafe { clang_sys::clang_getCursorUSR(child) });
                assert!(usr.is_some(), "Foo should have USR");
            }
            clang_sys::CXChildVisit_Recurse
        });
        assert!(found_foo, "should find struct Foo in loaded TU");

        // ECC round-trip on the same TU bytes
        {
            use crate::files::ecc_frame;

            let mut ecc_buf = Vec::new();
            ecc_frame::write_frame(&mut ecc_buf, &tu_bytes).expect("ecc encode");
            assert!(ecc_buf.len() > tu_bytes.len(), "ECC should add overhead");

            let decoded = ecc_frame::read_frame(&mut ecc_buf.as_slice())
                .expect("ecc decode")
                .expect("ecc frame should contain data");
            assert_eq!(decoded, tu_bytes, "ECC round-trip should preserve TU bytes");

            unsafe {
                clang_sys::clang_disposeTranslationUnit(loaded_tu);
            }
            let tmp_ecc = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(tmp_ecc.path(), &decoded).unwrap();
            let c_ecc_path =
                std::ffi::CString::new(tmp_ecc.path().to_str().unwrap()).unwrap();
            let ecc_tu = unsafe {
                clang_sys::clang_createTranslationUnit(cx_index, c_ecc_path.as_ptr())
            };
            assert!(!ecc_tu.is_null(), "load from ECC-decoded bytes failed");

            let ecc_root = unsafe { clang_sys::clang_getTranslationUnitCursor(ecc_tu) };
            let mut found_main_ns = false;
            visit_children(ecc_root, |child, _| {
                let name =
                    cxstring_to_option(unsafe { clang_sys::clang_getCursorSpelling(child) });
                if name.as_deref() == Some("ns") {
                    found_main_ns = true;
                }
                clang_sys::CXChildVisit_Recurse
            });
            assert!(
                found_main_ns,
                "should find namespace ns in ECC-decoded TU"
            );

            unsafe {
                clang_sys::clang_disposeTranslationUnit(ecc_tu);
            }
        }

        let _ = std::fs::remove_file(source_path);
    }
}
