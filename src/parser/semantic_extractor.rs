use std::collections::HashSet;

use rustc_hash::FxHashMap;
use std::fs;
use std::path::Path;

use anyhow::{anyhow, Result};
use clang::diagnostic::Severity as ClangSeverity;
use clang::source::Location;
use clang::{Entity, EntityVisitResult, Index, Unsaved};

use crate::parser::clang_types::ClangDeclKey;
use crate::parser::clang_result::{
    ClangDiagnosticEntry, ClangDiagnosticSeverity, ClangDiagnosticSummary, ClangParseResult,
};
use crate::parser::clang_symbol::ClangSymbol;
use crate::parser::clang_types::ClangSymbolKey;

pub(crate) struct SemanticExtractor;

impl SemanticExtractor {
    pub(crate) fn run_parse(
        index: &Index<'_>,
        source_path: &str,
        text: &str,
        arguments: &[String],
    ) -> Result<ClangParseResult> {
        let argument_refs = arguments.iter().map(String::as_str).collect::<Vec<_>>();
        let unsaved = [Unsaved::new(source_path, text)];
        let mut parser = index.parser(source_path);
        parser.arguments(argument_refs.as_slice());
        parser.unsaved(&unsaved);
        parser.detailed_preprocessing_record(true);
        let translation_unit = parser
            .parse()
            .map_err(|err| anyhow!("libclang parse failed: {err:?}"))?;

        let mut success = true;
        let mut diagnostics = Vec::new();
        let mut diagnostic_summary = ClangDiagnosticSummary::default();
        let mut diagnostic_entries = Vec::new();
        for diagnostic in translation_unit.get_diagnostics() {
            let severity = diagnostic.get_severity();
            if matches!(severity, ClangSeverity::Error | ClangSeverity::Fatal) {
                success = false;
            }
            let mapped_severity = match severity {
                ClangSeverity::Ignored => {
                    diagnostic_summary.ignored = diagnostic_summary.ignored.saturating_add(1);
                    ClangDiagnosticSeverity::Ignored
                }
                ClangSeverity::Note => {
                    diagnostic_summary.note = diagnostic_summary.note.saturating_add(1);
                    ClangDiagnosticSeverity::Note
                }
                ClangSeverity::Warning => {
                    diagnostic_summary.warning = diagnostic_summary.warning.saturating_add(1);
                    ClangDiagnosticSeverity::Warning
                }
                ClangSeverity::Error => {
                    diagnostic_summary.error = diagnostic_summary.error.saturating_add(1);
                    ClangDiagnosticSeverity::Error
                }
                ClangSeverity::Fatal => {
                    diagnostic_summary.fatal = diagnostic_summary.fatal.saturating_add(1);
                    ClangDiagnosticSeverity::Fatal
                }
            };
            let location = diagnostic.get_location().get_presumed_location();
            diagnostic_entries.push(ClangDiagnosticEntry {
                line: location.1 as usize,
                column: location.2 as usize,
                severity: mapped_severity,
            });
            diagnostics.push(format!(
                "{}:{}:{}: {:?}: {}",
                location.0,
                location.1,
                location.2,
                severity,
                diagnostic.get_text()
            ));
        }

        let mut seen = HashSet::new();
        let mut symbols = Vec::new();
        let mut symbol_entities = Vec::new();
        Self::collect_symbols(
            translation_unit.get_entity(),
            &mut seen,
            &mut symbols,
            &mut symbol_entities,
        );
        symbols.sort_by(|left, right| {
            left.line
                .cmp(&right.line)
                .then(left.column.cmp(&right.column))
                .then(left.name.cmp(&right.name))
        });
        let rename_offsets_by_symbol =
            Self::collect_rename_offsets(&translation_unit, source_path, &symbol_entities);
        let reference_offsets_by_decl =
            Self::collect_reference_offsets(translation_unit.get_entity());

        Ok(ClangParseResult::with_semantic_offsets(
            success,
            diagnostics,
            symbols,
            rename_offsets_by_symbol,
            reference_offsets_by_decl,
            diagnostic_summary,
            diagnostic_entries,
        ))
    }

    fn collect_symbols<'tu>(
        entity: Entity<'tu>,
        seen: &mut HashSet<(String, Option<String>, i32, usize, usize)>,
        symbols: &mut Vec<ClangSymbol>,
        symbol_entities: &mut Vec<(ClangSymbol, Entity<'tu>)>,
    ) {
        entity.visit_children(|child, _parent| {
            if let Some(symbol) = Self::symbol_from_entity(child) {
                let key = (
                    symbol.name.clone(),
                    symbol.usr.clone(),
                    symbol.kind,
                    symbol.line,
                    symbol.column,
                );
                if seen.insert(key) {
                    symbols.push(symbol.clone());
                    symbol_entities.push((symbol, child));
                }
            }
            EntityVisitResult::Recurse
        });
    }

    fn collect_rename_offsets<'tu>(
        translation_unit: &clang::TranslationUnit<'tu>,
        source_path: &str,
        symbol_entities: &[(ClangSymbol, Entity<'tu>)],
    ) -> FxHashMap<ClangSymbolKey, Vec<usize>> {
        let Some(file) = translation_unit.get_file(source_path) else {
            return FxHashMap::default();
        };

        let mut rename_offsets_by_symbol = FxHashMap::default();
        for (symbol, entity) in symbol_entities {
            let mut offsets = HashSet::new();

            if let Some(location) = entity.get_location().map(|loc| loc.get_spelling_location()) {
                if Self::is_main_file_location(&location) {
                    offsets.insert(location.offset as usize);
                }
            }

            for reference in file.get_references(*entity) {
                let Some(location) = reference.get_location() else {
                    continue;
                };
                if !location.is_in_main_file() {
                    continue;
                }
                let spelling = location.get_spelling_location();
                if Self::is_main_file_location(&spelling) {
                    offsets.insert(spelling.offset as usize);
                }
            }

            if offsets.is_empty() {
                continue;
            }

            let key = ClangSymbolKey::new(symbol.name.clone(), symbol.kind, symbol.line);
            let entry = rename_offsets_by_symbol.entry(key).or_insert_with(Vec::new);
            entry.extend(offsets.into_iter());
            entry.sort_unstable();
            entry.dedup();
        }

        rename_offsets_by_symbol
    }

    fn collect_reference_offsets(entity: Entity<'_>) -> FxHashMap<ClangDeclKey, Vec<usize>> {
        let mut offsets_by_decl: FxHashMap<ClangDeclKey, Vec<usize>> = FxHashMap::default();
        let mut path_cache: FxHashMap<std::path::PathBuf, String> = FxHashMap::default();

        entity.visit_children(|child, _parent| {
            let Some(reference) = child.get_reference() else {
                return EntityVisitResult::Recurse;
            };
            let Some(reference_location) = child.get_location() else {
                return EntityVisitResult::Recurse;
            };
            if !reference_location.is_in_main_file() {
                return EntityVisitResult::Recurse;
            }
            let reference_spelling = reference_location.get_spelling_location();
            if !Self::is_main_file_location(&reference_spelling) {
                return EntityVisitResult::Recurse;
            }
            let Some(decl_key) = Self::decl_key_from_entity_cached(reference, &mut path_cache) else {
                return EntityVisitResult::Recurse;
            };

            offsets_by_decl
                .entry(decl_key)
                .or_default()
                .push(reference_spelling.offset as usize);
            EntityVisitResult::Recurse
        });

        for offsets in offsets_by_decl.values_mut() {
            offsets.sort_unstable();
            offsets.dedup();
        }

        offsets_by_decl
    }

    fn is_main_file_location(location: &Location<'_>) -> bool {
        location.file.is_some() && location.line > 0 && location.column > 0
    }

    fn decl_key_from_entity_cached(
        entity: Entity<'_>,
        path_cache: &mut FxHashMap<std::path::PathBuf, String>,
    ) -> Option<ClangDeclKey> {
        let kind = entity.get_kind();
        if !Self::is_relevant_kind(kind as i32) {
            return None;
        }
        let location = entity.get_location()?.get_spelling_location();
        let file = location.file?;
        let raw_path = file.get_path();
        let path = path_cache
            .entry(raw_path.clone())
            .or_insert_with(|| Self::normalize_path_for_key(&raw_path))
            .clone();
        let line = location.line as usize;
        let column = location.column as usize;
        if line == 0 || column == 0 {
            return None;
        }

        Some(ClangDeclKey::new(path, line, column, kind as i32))
    }

    fn normalize_path_for_key(path: &Path) -> String {
        fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .to_string()
    }

    fn symbol_from_entity(entity: Entity<'_>) -> Option<ClangSymbol> {
        let kind = entity.get_kind();
        if !Self::is_relevant_kind(kind as i32) {
            return None;
        }
        let name = entity
            .get_name()
            .or_else(|| entity.get_display_name())
            .unwrap_or_default();
        if name.is_empty() {
            return None;
        }

        let source_location = entity.get_location()?;
        if !source_location.is_in_main_file() {
            return None;
        }
        let location = source_location.get_spelling_location();
        let line = location.line as usize;
        let column = location.column as usize;
        if line == 0 || column == 0 {
            return None;
        }

        let kind_i32 = kind as i32;
        let needs_storage = matches!(
            kind_i32,
            clang_sys::CXCursor_VarDecl
                | clang_sys::CXCursor_FunctionDecl
                | clang_sys::CXCursor_CXXMethod
                | clang_sys::CXCursor_FunctionTemplate
        );
        let storage_class = if needs_storage {
            entity.get_storage_class().map(|sc| sc as i32)
        } else {
            None
        };

        let needs_type = matches!(
            kind_i32,
            clang_sys::CXCursor_VarDecl
                | clang_sys::CXCursor_FieldDecl
                | clang_sys::CXCursor_ParmDecl
                | clang_sys::CXCursor_FunctionDecl
                | clang_sys::CXCursor_CXXMethod
                | clang_sys::CXCursor_FunctionTemplate
                | clang_sys::CXCursor_Constructor
                | clang_sys::CXCursor_Destructor
        );
        let (is_const, is_volatile, type_kind, type_display, canonical_type_kind, template_name) =
            if needs_type {
                if let Some(ty) = entity.get_type() {
                    let tk = ty.get_kind() as i32;
                    let display = if tk == clang_sys::CXType_Pointer {
                        ty.get_display_name()
                    } else {
                        String::new()
                    };
                    let tmpl = ty.get_declaration()
                        .and_then(|decl| decl.get_name())
                        .filter(|n| !n.is_empty());
                    (
                        ty.is_const_qualified(),
                        ty.is_volatile_qualified(),
                        tk,
                        display,
                        ty.get_canonical_type().get_kind() as i32,
                        tmpl,
                    )
                } else {
                    (false, false, clang_sys::CXType_Unexposed, String::new(), clang_sys::CXType_Unexposed, None)
                }
            } else {
                (false, false, clang_sys::CXType_Unexposed, String::new(), clang_sys::CXType_Unexposed, None)
            };

        Some(ClangSymbol {
            name,
            kind: kind_i32,
            line,
            column,
            usr: entity.get_usr().and_then(|value| {
                let raw = value.0;
                let trimmed = raw.trim();
                (!trimmed.is_empty()).then_some(trimmed.to_string())
            }),
            scope_usr: entity
                .get_semantic_parent()
                .and_then(|parent| parent.get_usr())
                .and_then(|value| {
                    let raw = value.0;
                    let trimmed = raw.trim();
                    (!trimmed.is_empty()).then_some(trimmed.to_string())
                }),
            storage_class,
            is_const,
            is_volatile,
            type_kind,
            type_display,
            canonical_type_kind,
            template_name,
        })
    }

    fn is_relevant_kind(kind: i32) -> bool {
        matches!(
            kind,
            clang_sys::CXCursor_FunctionDecl
                | clang_sys::CXCursor_FunctionTemplate
                | clang_sys::CXCursor_CXXMethod
                | clang_sys::CXCursor_Constructor
                | clang_sys::CXCursor_Destructor
                | clang_sys::CXCursor_VarDecl
                | clang_sys::CXCursor_FieldDecl
                | clang_sys::CXCursor_ParmDecl
                | clang_sys::CXCursor_StructDecl
                | clang_sys::CXCursor_ClassDecl
                | clang_sys::CXCursor_UnionDecl
                | clang_sys::CXCursor_EnumDecl
                | clang_sys::CXCursor_TypedefDecl
                | clang_sys::CXCursor_TypeAliasDecl
                | clang_sys::CXCursor_Namespace
                | clang_sys::CXCursor_MacroDefinition
                | clang_sys::CXCursor_ConversionFunction
                | clang_sys::CXCursor_UsingDeclaration
                | clang_sys::CXCursor_EnumConstantDecl
                | clang_sys::CXCursor_FriendDecl
        )
    }
}
