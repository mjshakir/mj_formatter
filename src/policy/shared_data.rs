use std::borrow::Cow;

use crate::parser::file_context::{SemanticFileContext, SemanticScopeKind};
use crate::policy::text_utils;

#[derive(Debug)]
pub struct PolicySharedData<'a> {
    text: &'a str,
    line_ending: &'static str,
    trailing_newline: bool,
    macro_lines: Vec<bool>,
}

impl<'a> PolicySharedData<'a> {
    pub fn new(text: &'a str, semantic: Option<&SemanticFileContext>) -> Self {
        let line_count = memchr::memchr_iter(b'\n', text.as_bytes()).count() + 1;
        let mut macro_lines = vec![false; line_count];
        if let Some(ctx) = semantic {
            for scope in &ctx.scopes {
                if scope.kind == SemanticScopeKind::Preprocessor {
                    let start = scope.start_line.saturating_sub(1);
                    let end = scope.end_line.min(line_count);
                    for line in &mut macro_lines[start..end] {
                        *line = true;
                    }
                }
            }
        }
        Self {
            text,
            line_ending: text_utils::detect_line_ending(text),
            trailing_newline: text.ends_with('\n'),
            macro_lines,
        }
    }

    #[inline]
    pub fn line_ending(&self) -> &'static str {
        self.line_ending
    }

    #[inline]
    pub fn trailing_newline(&self) -> bool {
        self.trailing_newline
    }

    #[inline]
    pub fn is_macro_line(&self, line: usize) -> bool {
        line > 0 && self.macro_lines.get(line - 1).copied().unwrap_or(false)
    }

    pub fn lines_cow(&self) -> Vec<Cow<'a, str>> {
        let (lines, _) = text_utils::split_lines_cow(self.text);
        lines
    }
}
