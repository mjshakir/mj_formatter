use std::borrow::Cow;

use crate::parser::text_scan;

pub fn detect_line_ending(text: &str) -> &'static str {
    if text_scan::contains_subslice(text.as_bytes(), b"\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

pub fn split_lines_cow(text: &str) -> (Vec<Cow<'_, str>>, bool) {
    if text.is_empty() {
        return (Vec::new(), false);
    }
    let trailing = text.ends_with('\n');
    let lines = text
        .split('\n')
        .map(|line| {
            if let Some(stripped) = line.strip_suffix('\r') {
                Cow::Owned(stripped.to_string())
            } else {
                Cow::Borrowed(line)
            }
        })
        .collect();
    (lines, trailing)
}

pub fn join_lines_cow(lines: &[Cow<'_, str>], eol: &str, trailing_newline: bool) -> String {
    join_lines_generic(lines.iter().map(|c| c.as_ref()), eol, trailing_newline)
}

fn join_lines_generic<'a>(
    lines: impl ExactSizeIterator<Item = &'a str> + Clone,
    eol: &str,
    trailing_newline: bool,
) -> String {
    let count = lines.len();
    if count == 0 {
        return String::new();
    }

    let content_len: usize = lines.clone().map(str::len).sum();
    let delimiter_len = eol.len().saturating_mul(count.saturating_sub(1));
    let mut text = String::with_capacity(content_len + delimiter_len + eol.len());

    for (index, line) in lines.enumerate() {
        if index > 0 {
            text.push_str(eol);
        }
        text.push_str(line);
    }

    if trailing_newline && !text.ends_with(eol) {
        text.push_str(eol);
    }
    text
}
