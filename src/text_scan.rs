#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TextScanEngine;

pub(crate) static TEXT_SCAN: TextScanEngine = TextScanEngine;

impl TextScanEngine {
    #[inline]
    pub(crate) fn line_starts(&self, text: &str, include_terminal_start: bool) -> Vec<usize> {
        let mut starts = Vec::with_capacity((text.len() / 48).max(4));
        starts.push(0usize);
        let len = text.len();
        self.for_each_byte_match(text.as_bytes(), b'\n', |index| {
            let next = index + 1;
            if include_terminal_start || next < len {
                starts.push(next);
            }
        });
        starts
    }

    #[inline]
    pub(crate) fn split_lines_keepends(
        &self,
        text: &str,
        empty_returns_single_empty_line: bool,
    ) -> Vec<String> {
        if text.is_empty() {
            return if empty_returns_single_empty_line {
                vec![String::new()]
            } else {
                Vec::new()
            };
        }

        let mut lines = Vec::<String>::with_capacity((text.len() / 64).max(1));
        let mut start = 0usize;
        self.for_each_byte_match(text.as_bytes(), b'\n', |index| {
            lines.push(text[start..=index].to_string());
            start = index + 1;
        });
        if start < text.len() {
            lines.push(text[start..].to_string());
        }
        if lines.is_empty() && empty_returns_single_empty_line {
            lines.push(String::new());
        }
        lines
    }

    #[inline]
    pub(crate) fn split_lines_as_slices<'a>(
        &self,
        text: &'a str,
        keep_ends: bool,
    ) -> Vec<&'a str> {
        if text.is_empty() {
            return Vec::new();
        }

        let mut lines = Vec::<&'a str>::with_capacity((text.len() / 48).max(4));
        let mut start = 0usize;
        for pos in memchr::memchr_iter(b'\n', text.as_bytes()) {
            if keep_ends {
                lines.push(&text[start..=pos]);
            } else {
                lines.push(&text[start..pos]);
            }
            start = pos + 1;
        }
        if start < text.len() {
            lines.push(&text[start..]);
        }
        lines
    }

    #[inline]
    pub(crate) fn count_byte(&self, bytes: &[u8], needle: u8) -> usize {
        memchr::memchr_iter(needle, bytes).count()
    }

    #[inline]
    pub(crate) fn has_line_count_changed(&self, before: &str, after: &str) -> bool {
        self.count_byte(before.as_bytes(), b'\n') != self.count_byte(after.as_bytes(), b'\n')
    }

    #[inline]
    pub(crate) fn contains_subslice(&self, haystack: &[u8], needle: &[u8]) -> bool {
        self.find_subslice_from(haystack, needle, 0).is_some()
    }

    #[inline]
    pub(crate) fn find_subslice_from(
        &self,
        haystack: &[u8],
        needle: &[u8],
        from: usize,
    ) -> Option<usize> {
        if needle.is_empty() {
            return Some(from.min(haystack.len()));
        }
        if from >= haystack.len() {
            return None;
        }
        if needle.len() == 1 {
            return self.find_byte_in_range(haystack, needle[0], from, haystack.len());
        }
        if haystack.len().saturating_sub(from) < needle.len() {
            return None;
        }

        let first = needle[0];
        let suffix = &needle[1..];
        let last_start = haystack.len() - needle.len();
        let mut cursor = from;
        while cursor <= last_start {
            let search_end = last_start + 1;
            let candidate = self.find_byte_in_range(haystack, first, cursor, search_end)?;
            let end = candidate + needle.len();
            if haystack[candidate + 1..end] == *suffix {
                return Some(candidate);
            }
            cursor = candidate + 1;
        }
        None
    }

    #[inline]
    pub(crate) fn for_each_byte_match(
        &self,
        bytes: &[u8],
        needle: u8,
        mut on_match: impl FnMut(usize),
    ) {
        for pos in memchr::memchr_iter(needle, bytes) {
            on_match(pos);
        }
    }

    #[inline]
    fn find_byte_in_range(
        &self,
        bytes: &[u8],
        needle: u8,
        from: usize,
        end_exclusive: usize,
    ) -> Option<usize> {
        if from >= end_exclusive || end_exclusive > bytes.len() {
            return None;
        }
        memchr::memchr(needle, &bytes[from..end_exclusive]).map(|pos| pos + from)
    }

    #[inline]
    pub(crate) fn all_bytes_equal(&self, bytes: &[u8], target: u8) -> bool {
        if bytes.is_empty() {
            return true;
        }
        #[cfg(target_arch = "aarch64")]
        {
            all_bytes_equal_neon(bytes, target)
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            all_bytes_equal_scalar(bytes, target)
        }
    }

    #[inline]
    pub(crate) fn leading_whitespace_byte_count(&self, bytes: &[u8]) -> usize {
        #[cfg(target_arch = "aarch64")]
        {
            leading_whitespace_byte_count_neon(bytes)
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            leading_whitespace_byte_count_scalar(bytes)
        }
    }

    #[inline]
    pub(crate) fn strings_equal(&self, a: &str, b: &str) -> bool {
        self.slices_equal(a.as_bytes(), b.as_bytes())
    }

    pub(crate) fn slices_equal(&self, a: &[u8], b: &[u8]) -> bool {
        if a.len() != b.len() {
            return false;
        }
        #[cfg(target_arch = "aarch64")]
        {
            slices_equal_neon(a, b)
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            a == b
        }
    }
}

// ---------------------------------------------------------------------------
// Scalar fallbacks
// ---------------------------------------------------------------------------

#[inline]
fn all_bytes_equal_scalar(bytes: &[u8], target: u8) -> bool {
    bytes.iter().all(|&b| b == target)
}

#[inline]
fn leading_whitespace_byte_count_scalar(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .take_while(|&&b| b == b' ' || b == b'\t')
        .count()
}

// ---------------------------------------------------------------------------
// NEON implementations (aarch64)
// ---------------------------------------------------------------------------

#[cfg(target_arch = "aarch64")]
#[inline]
fn all_bytes_equal_neon(bytes: &[u8], target: u8) -> bool {
    let len = bytes.len();
    if len < 32 {
        return all_bytes_equal_scalar(bytes, target);
    }

    unsafe {
        use core::arch::aarch64::{vceqq_u8, vdupq_n_u8, vld1q_u8, vminvq_u8};

        let target_vec = vdupq_n_u8(target);
        let mut ptr = bytes.as_ptr();
        let end = ptr.add(len);

        // Process 64 bytes per iteration (4x16B)
        let end64 = bytes.as_ptr().add(len & !63);
        while ptr < end64 {
            let c0 = vceqq_u8(vld1q_u8(ptr), target_vec);
            let c1 = vceqq_u8(vld1q_u8(ptr.add(16)), target_vec);
            let c2 = vceqq_u8(vld1q_u8(ptr.add(32)), target_vec);
            let c3 = vceqq_u8(vld1q_u8(ptr.add(48)), target_vec);
            if vminvq_u8(c0) != 0xFF
                || vminvq_u8(c1) != 0xFF
                || vminvq_u8(c2) != 0xFF
                || vminvq_u8(c3) != 0xFF
            {
                return false;
            }
            ptr = ptr.add(64);
        }

        // Process remaining 16B chunks
        let end16 = bytes.as_ptr().add(len & !15);
        while ptr < end16 {
            let cmp = vceqq_u8(vld1q_u8(ptr), target_vec);
            if vminvq_u8(cmp) != 0xFF {
                return false;
            }
            ptr = ptr.add(16);
        }

        // Scalar tail
        while ptr < end {
            if *ptr != target {
                return false;
            }
            ptr = ptr.add(1);
        }
        true
    }
}

#[cfg(target_arch = "aarch64")]
#[inline]
fn leading_whitespace_byte_count_neon(bytes: &[u8]) -> usize {
    let len = bytes.len();
    if len < 32 {
        return leading_whitespace_byte_count_scalar(bytes);
    }

    unsafe {
        use core::arch::aarch64::{
            vceqq_u8, vdupq_n_u8, vld1q_u8, vminvq_u8, vorrq_u8, vst1q_u8,
        };

        let space_vec = vdupq_n_u8(b' ');
        let tab_vec = vdupq_n_u8(b'\t');

        let mut offset = 0usize;

        // Process 16 bytes at a time
        while offset + 16 <= len {
            let chunk = vld1q_u8(bytes.as_ptr().add(offset));
            let cmp = vorrq_u8(vceqq_u8(chunk, space_vec), vceqq_u8(chunk, tab_vec));
            if vminvq_u8(cmp) == 0xFF {
                offset += 16;
            } else {
                // Partial: find first non-whitespace lane
                let mut lanes = [0u8; 16];
                vst1q_u8(lanes.as_mut_ptr(), cmp);
                for (i, &lane) in lanes.iter().enumerate() {
                    if lane == 0 {
                        return offset + i;
                    }
                }
                return offset + 16;
            }
        }

        // Scalar tail
        while offset < len {
            let b = bytes[offset];
            if b == b' ' || b == b'\t' {
                offset += 1;
            } else {
                break;
            }
        }
        offset
    }
}

#[cfg(target_arch = "aarch64")]
#[inline]
fn slices_equal_neon(a: &[u8], b: &[u8]) -> bool {
    let len = a.len();
    debug_assert_eq!(len, b.len());

    if len < 32 {
        return a == b;
    }

    unsafe {
        use core::arch::aarch64::{vceqq_u8, vld1q_u8, vminvq_u8};

        let mut offset = 0usize;

        // Process 64 bytes per iteration (4x16B)
        let end64 = len & !63;
        while offset < end64 {
            let a0 = vld1q_u8(a.as_ptr().add(offset));
            let b0 = vld1q_u8(b.as_ptr().add(offset));
            let a1 = vld1q_u8(a.as_ptr().add(offset + 16));
            let b1 = vld1q_u8(b.as_ptr().add(offset + 16));
            let a2 = vld1q_u8(a.as_ptr().add(offset + 32));
            let b2 = vld1q_u8(b.as_ptr().add(offset + 32));
            let a3 = vld1q_u8(a.as_ptr().add(offset + 48));
            let b3 = vld1q_u8(b.as_ptr().add(offset + 48));

            if vminvq_u8(vceqq_u8(a0, b0)) != 0xFF
                || vminvq_u8(vceqq_u8(a1, b1)) != 0xFF
                || vminvq_u8(vceqq_u8(a2, b2)) != 0xFF
                || vminvq_u8(vceqq_u8(a3, b3)) != 0xFF
            {
                return false;
            }
            offset += 64;
        }

        // Process remaining 16B chunks
        let end16 = len & !15;
        while offset < end16 {
            let av = vld1q_u8(a.as_ptr().add(offset));
            let bv = vld1q_u8(b.as_ptr().add(offset));
            if vminvq_u8(vceqq_u8(av, bv)) != 0xFF {
                return false;
            }
            offset += 16;
        }

        // Scalar tail
        a[offset..] == b[offset..]
    }
}

// ---------------------------------------------------------------------------
// Free-standing wrappers
// ---------------------------------------------------------------------------

pub(crate) struct SubsliceMatchIter<'a> {
    haystack: &'a [u8],
    needle: &'a [u8],
    cursor: usize,
}

impl<'a> Iterator for SubsliceMatchIter<'a> {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        if self.needle.is_empty() {
            return None;
        }
        let index = TEXT_SCAN.find_subslice_from(self.haystack, self.needle, self.cursor)?;
        self.cursor = index.saturating_add(1);
        Some(index)
    }
}

#[inline]
pub(crate) fn subslice_match_indices<'a>(
    haystack: &'a [u8],
    needle: &'a [u8],
) -> SubsliceMatchIter<'a> {
    SubsliceMatchIter {
        haystack,
        needle,
        cursor: 0,
    }
}

#[inline]
pub(crate) fn line_starts(text: &str, include_terminal_start: bool) -> Vec<usize> {
    TEXT_SCAN.line_starts(text, include_terminal_start)
}

#[inline]
pub(crate) fn split_lines_keepends(
    text: &str,
    empty_returns_single_empty_line: bool,
) -> Vec<String> {
    TEXT_SCAN.split_lines_keepends(text, empty_returns_single_empty_line)
}

#[inline]
pub(crate) fn split_lines_as_slices(text: &str, keep_ends: bool) -> Vec<&str> {
    TEXT_SCAN.split_lines_as_slices(text, keep_ends)
}

#[inline]
pub(crate) fn count_byte(bytes: &[u8], needle: u8) -> usize {
    TEXT_SCAN.count_byte(bytes, needle)
}

#[inline]
pub(crate) fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    TEXT_SCAN.contains_subslice(haystack, needle)
}

#[inline]
pub(crate) fn find_subslice_from(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    TEXT_SCAN.find_subslice_from(haystack, needle, from)
}

#[inline]
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

pub(crate) fn count_identifier_occurrences_excluding_non_code(
    text: &str,
    name: &str,
    tree: &tree_sitter::Tree,
) -> usize {
    let excluded = collect_non_code_byte_ranges(tree);
    let name_bytes = name.as_bytes();
    let text_bytes = text.as_bytes();
    let len = name_bytes.len();
    if len == 0 || text_bytes.len() < len {
        return 0;
    }
    let mut count = 0usize;
    let last = text_bytes.len() - len;
    let first_byte = name_bytes[0];
    for pos in memchr::memchr_iter(first_byte, &text_bytes[..=last]) {
        if text_bytes[pos..pos + len] == *name_bytes {
            let before_ok = pos == 0 || !is_ident_byte(text_bytes[pos - 1]);
            let after_ok = pos + len >= text_bytes.len() || !is_ident_byte(text_bytes[pos + len]);
            if before_ok && after_ok && !is_in_excluded_range(&excluded, pos) {
                count += 1;
            }
        }
    }
    count
}

fn collect_non_code_byte_ranges(tree: &tree_sitter::Tree) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut cursor = tree.walk();
    collect_non_code_ranges_recursive(&mut cursor, &mut ranges);
    ranges.sort_unstable_by_key(|r| r.0);
    ranges
}

fn collect_non_code_ranges_recursive(
    cursor: &mut tree_sitter::TreeCursor,
    ranges: &mut Vec<(usize, usize)>,
) {
    loop {
        let node = cursor.node();
        let kind = node.kind();
        if kind == "comment" || kind == "string_literal" || kind == "raw_string_literal" {
            ranges.push((node.start_byte(), node.end_byte()));
        } else if cursor.goto_first_child() {
            collect_non_code_ranges_recursive(cursor, ranges);
            cursor.goto_parent();
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

fn is_in_excluded_range(ranges: &[(usize, usize)], pos: usize) -> bool {
    ranges
        .binary_search_by(|&(start, end)| {
            if pos < start {
                std::cmp::Ordering::Greater
            } else if pos >= end {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

pub(crate) fn count_identifier_occurrences(text: &str, name: &str) -> usize {
    let name_bytes = name.as_bytes();
    let text_bytes = text.as_bytes();
    let len = name_bytes.len();
    if len == 0 || text_bytes.len() < len {
        return 0;
    }
    let mut count = 0usize;
    let last = text_bytes.len() - len;
    let first_byte = name_bytes[0];
    for pos in memchr::memchr_iter(first_byte, &text_bytes[..=last]) {
        if text_bytes[pos..pos + len] == *name_bytes {
            let before_ok = pos == 0 || !is_ident_byte(text_bytes[pos - 1]);
            let after_ok = pos + len >= text_bytes.len() || !is_ident_byte(text_bytes[pos + len]);
            if before_ok && after_ok {
                count += 1;
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::{
        contains_subslice, count_byte, count_identifier_occurrences, find_subslice_from,
        line_starts, split_lines_as_slices, split_lines_keepends, subslice_match_indices,
        TEXT_SCAN,
    };

    #[test]
    fn line_starts_excludes_terminal_start_when_requested() {
        assert_eq!(line_starts("a\nb\n", false), vec![0, 2]);
        assert_eq!(line_starts("a\nb", false), vec![0, 2]);
        assert_eq!(line_starts("", false), vec![0]);
    }

    #[test]
    fn line_starts_includes_terminal_start_when_requested() {
        assert_eq!(line_starts("a\nb\n", true), vec![0, 2, 4]);
        assert_eq!(line_starts("a\nb", true), vec![0, 2]);
        assert_eq!(line_starts("", true), vec![0]);
    }

    #[test]
    fn split_lines_keepends_respects_empty_mode() {
        assert_eq!(split_lines_keepends("", false), Vec::<String>::new());
        assert_eq!(split_lines_keepends("", true), vec![String::new()]);
        assert_eq!(
            split_lines_keepends("a\nb\n", false),
            vec!["a\n".to_string(), "b\n".to_string()]
        );
        assert_eq!(
            split_lines_keepends("a\nb", true),
            vec!["a\n".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn split_lines_as_slices_keepends() {
        assert!(split_lines_as_slices("", true).is_empty());
        assert_eq!(split_lines_as_slices("a\nb\n", true), vec!["a\n", "b\n"]);
        assert_eq!(split_lines_as_slices("a\nb", true), vec!["a\n", "b"]);
        assert_eq!(split_lines_as_slices("one", true), vec!["one"]);
    }

    #[test]
    fn split_lines_as_slices_no_keepends() {
        assert_eq!(split_lines_as_slices("a\nb\n", false), vec!["a", "b"]);
        assert_eq!(split_lines_as_slices("a\nb", false), vec!["a", "b"]);
    }

    #[test]
    fn count_byte_matches_scalar_expectations() {
        assert_eq!(count_byte("".as_bytes(), b'\n'), 0);
        assert_eq!(count_byte("x".as_bytes(), b'\n'), 0);
        assert_eq!(count_byte("x\n\nz".as_bytes(), b'\n'), 2);
    }

    #[test]
    fn count_byte_large_buffer() {
        let buf = "x\n".repeat(500);
        assert_eq!(count_byte(buf.as_bytes(), b'\n'), 500);
    }

    #[test]
    fn subslice_search_handles_basic_cases() {
        let bytes = b"alpha operator beta operator";
        assert!(contains_subslice(bytes, b"operator"));
        assert_eq!(find_subslice_from(bytes, b"operator", 0), Some(6));
        assert_eq!(find_subslice_from(bytes, b"operator", 7), Some(20));
        assert_eq!(find_subslice_from(bytes, b"missing", 0), None);
    }

    #[test]
    fn subslice_match_iterator_reports_all_matches() {
        let matches = subslice_match_indices(b"aaaa", b"aa").collect::<Vec<_>>();
        assert_eq!(matches, vec![0, 1, 2]);
    }

    #[test]
    fn has_line_count_changed_detects_difference() {
        assert!(!TEXT_SCAN.has_line_count_changed("a\nb\n", "x\ny\n"));
        assert!(TEXT_SCAN.has_line_count_changed("a\nb\n", "a\nb\nc\n"));
        assert!(!TEXT_SCAN.has_line_count_changed("", ""));
    }

    // -- all_bytes_equal tests --

    #[test]
    fn all_bytes_equal_empty() {
        assert!(TEXT_SCAN.all_bytes_equal(b"", b'-'));
    }

    #[test]
    fn all_bytes_equal_single() {
        assert!(TEXT_SCAN.all_bytes_equal(b"-", b'-'));
        assert!(!TEXT_SCAN.all_bytes_equal(b"x", b'-'));
    }

    #[test]
    fn all_bytes_equal_all_match() {
        let dashes = "-".repeat(100);
        assert!(TEXT_SCAN.all_bytes_equal(dashes.as_bytes(), b'-'));
    }

    #[test]
    fn all_bytes_equal_mismatch_at_end() {
        let mut data = "-".repeat(99);
        data.push('x');
        assert!(!TEXT_SCAN.all_bytes_equal(data.as_bytes(), b'-'));
    }

    #[test]
    fn all_bytes_equal_mismatch_at_start() {
        let mut data = String::from("x");
        data.push_str(&"-".repeat(99));
        assert!(!TEXT_SCAN.all_bytes_equal(data.as_bytes(), b'-'));
    }

    // -- leading_whitespace_byte_count tests --

    #[test]
    fn leading_whitespace_byte_count_empty() {
        assert_eq!(TEXT_SCAN.leading_whitespace_byte_count(b""), 0);
    }

    #[test]
    fn leading_whitespace_byte_count_none() {
        assert_eq!(TEXT_SCAN.leading_whitespace_byte_count(b"hello"), 0);
    }

    #[test]
    fn leading_whitespace_byte_count_spaces_and_tabs() {
        assert_eq!(TEXT_SCAN.leading_whitespace_byte_count(b"    int a;"), 4);
        assert_eq!(TEXT_SCAN.leading_whitespace_byte_count(b"\t\tint a;"), 2);
        assert_eq!(TEXT_SCAN.leading_whitespace_byte_count(b"  \tint a;"), 3);
    }

    #[test]
    fn leading_whitespace_byte_count_all_whitespace() {
        assert_eq!(TEXT_SCAN.leading_whitespace_byte_count(b"     "), 5);
    }

    #[test]
    fn leading_whitespace_byte_count_large() {
        let input = format!("{}content", " ".repeat(200));
        assert_eq!(
            TEXT_SCAN.leading_whitespace_byte_count(input.as_bytes()),
            200
        );
    }

    // -- slices_equal tests --

    #[test]
    fn slices_equal_empty() {
        assert!(TEXT_SCAN.slices_equal(b"", b""));
    }

    #[test]
    fn slices_equal_different_lengths() {
        assert!(!TEXT_SCAN.slices_equal(b"abc", b"abcd"));
    }

    #[test]
    fn slices_equal_identical() {
        let data = "int main() { return 0; }";
        assert!(TEXT_SCAN.slices_equal(data.as_bytes(), data.as_bytes()));
    }

    #[test]
    fn slices_equal_differ_at_end() {
        assert!(!TEXT_SCAN.slices_equal(
            b"int main() { return 0; }",
            b"int main() { return 1; }"
        ));
    }

    #[test]
    fn slices_equal_large_identical() {
        let data = "x".repeat(4096);
        assert!(TEXT_SCAN.slices_equal(data.as_bytes(), data.as_bytes()));
    }

    #[test]
    fn slices_equal_large_differ() {
        let a = "x".repeat(4096);
        let mut b = "x".repeat(4095);
        b.push('y');
        assert!(!TEXT_SCAN.slices_equal(a.as_bytes(), b.as_bytes()));
    }

    // -- count_identifier_occurrences tests --

    #[test]
    fn count_identifier_occurrences_basic() {
        let text = "Initialization(value); other Initialization; last";
        assert_eq!(count_identifier_occurrences(text, "Initialization"), 2);
    }

    #[test]
    fn count_identifier_occurrences_boundary_check() {
        let text = "myInitialization foo Initialization InitializationX";
        assert_eq!(count_identifier_occurrences(text, "Initialization"), 1);
    }

    #[test]
    fn count_identifier_occurrences_empty() {
        assert_eq!(count_identifier_occurrences("hello", ""), 0);
        assert_eq!(count_identifier_occurrences("", "foo"), 0);
    }

    #[test]
    fn count_identifier_occurrences_at_boundaries() {
        let text = "foo bar foo";
        assert_eq!(count_identifier_occurrences(text, "foo"), 2);
    }

    #[test]
    fn count_identifier_occurrences_underscore_boundary() {
        let text = "my_foo foo_bar foo";
        assert_eq!(count_identifier_occurrences(text, "foo"), 1);
    }
}
