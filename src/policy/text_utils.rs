use crate::text_scan;

pub fn detect_line_ending(text: &str) -> &'static str {
    if text_scan::contains_subslice(text.as_bytes(), b"\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

pub fn split_lines(text: &str) -> (Vec<String>, bool) {
    if text.is_empty() {
        return (Vec::new(), false);
    }
    let had_trailing_newline = text.ends_with('\n');
    let lines = text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_string())
        .collect();
    (lines, had_trailing_newline)
}

pub fn join_lines(lines: &[String], eol: &str, trailing_newline: bool) -> String {
    if lines.is_empty() {
        return String::new();
    }

    let content_len = lines.iter().map(String::len).sum::<usize>();
    let delimiter_len = eol.len().saturating_mul(lines.len().saturating_sub(1));
    let mut text = String::with_capacity(content_len + delimiter_len + eol.len());

    for (index, line) in lines.iter().enumerate() {
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
