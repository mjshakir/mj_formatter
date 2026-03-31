use tree_sitter::StreamingIterator;

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
        memchr::memmem::find(&haystack[from..], needle).map(|pos| pos + from)
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
    pub(crate) fn all_bytes_equal(&self, bytes: &[u8], target: u8) -> bool {
        if bytes.is_empty() {
            return true;
        }
        #[cfg(target_arch = "aarch64")]
        {
            all_bytes_equal_neon(bytes, target)
        }
        #[cfg(target_arch = "x86_64")]
        {
            all_bytes_equal_x86(bytes, target)
        }
        #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
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
        #[cfg(target_arch = "x86_64")]
        {
            leading_whitespace_byte_count_x86(bytes)
        }
        #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
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
        #[cfg(target_arch = "x86_64")]
        {
            slices_equal_x86(a, b)
        }
        #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
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
        use core::arch::aarch64::{vandq_u8, vceqq_u8, vdupq_n_u8, vld1q_u8, vminvq_u8};

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
            let merged = vandq_u8(vandq_u8(c0, c1), vandq_u8(c2, c3));
            if vminvq_u8(merged) != 0xFF {
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
        use core::arch::aarch64::{vandq_u8, vceqq_u8, vld1q_u8, vminvq_u8};

        let mut offset = 0usize;

        // Process 64 bytes per iteration (4x16B)
        let end64 = len & !63;
        while offset < end64 {
            let e0 = vceqq_u8(vld1q_u8(a.as_ptr().add(offset)), vld1q_u8(b.as_ptr().add(offset)));
            let e1 = vceqq_u8(vld1q_u8(a.as_ptr().add(offset + 16)), vld1q_u8(b.as_ptr().add(offset + 16)));
            let e2 = vceqq_u8(vld1q_u8(a.as_ptr().add(offset + 32)), vld1q_u8(b.as_ptr().add(offset + 32)));
            let e3 = vceqq_u8(vld1q_u8(a.as_ptr().add(offset + 48)), vld1q_u8(b.as_ptr().add(offset + 48)));
            let merged = vandq_u8(vandq_u8(e0, e1), vandq_u8(e2, e3));
            if vminvq_u8(merged) != 0xFF {
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
// x86_64 SSE2 implementations
// ---------------------------------------------------------------------------

#[cfg(target_arch = "x86_64")]
#[inline]
fn all_bytes_equal_x86(bytes: &[u8], target: u8) -> bool {
    let len = bytes.len();
    if len < 32 {
        return all_bytes_equal_scalar(bytes, target);
    }

    unsafe {
        use core::arch::x86_64::*;

        let target_vec = _mm_set1_epi8(target as i8);
        let mut offset = 0usize;

        // Process 64 bytes per iteration (4x16B)
        let end64 = len & !63;
        while offset < end64 {
            let c0 = _mm_cmpeq_epi8(_mm_loadu_si128(bytes.as_ptr().add(offset) as *const __m128i), target_vec);
            let c1 = _mm_cmpeq_epi8(_mm_loadu_si128(bytes.as_ptr().add(offset + 16) as *const __m128i), target_vec);
            let c2 = _mm_cmpeq_epi8(_mm_loadu_si128(bytes.as_ptr().add(offset + 32) as *const __m128i), target_vec);
            let c3 = _mm_cmpeq_epi8(_mm_loadu_si128(bytes.as_ptr().add(offset + 48) as *const __m128i), target_vec);
            let merged = _mm_and_si128(_mm_and_si128(c0, c1), _mm_and_si128(c2, c3));
            if _mm_movemask_epi8(merged) != 0xFFFF {
                return false;
            }
            offset += 64;
        }

        // Process remaining 16B chunks
        let end16 = len & !15;
        while offset < end16 {
            let cmp = _mm_cmpeq_epi8(_mm_loadu_si128(bytes.as_ptr().add(offset) as *const __m128i), target_vec);
            if _mm_movemask_epi8(cmp) != 0xFFFF {
                return false;
            }
            offset += 16;
        }

        // Scalar tail
        bytes[offset..].iter().all(|&b| b == target)
    }
}

#[cfg(target_arch = "x86_64")]
#[inline]
fn leading_whitespace_byte_count_x86(bytes: &[u8]) -> usize {
    let len = bytes.len();
    if len < 32 {
        return leading_whitespace_byte_count_scalar(bytes);
    }

    unsafe {
        use core::arch::x86_64::*;

        let space_vec = _mm_set1_epi8(b' ' as i8);
        let tab_vec = _mm_set1_epi8(b'\t' as i8);

        let mut offset = 0usize;

        while offset + 16 <= len {
            let chunk = _mm_loadu_si128(bytes.as_ptr().add(offset) as *const __m128i);
            let cmp = _mm_or_si128(
                _mm_cmpeq_epi8(chunk, space_vec),
                _mm_cmpeq_epi8(chunk, tab_vec),
            );
            let mask = _mm_movemask_epi8(cmp) as u32;
            if mask == 0xFFFF {
                offset += 16;
            } else {
                // Find first non-whitespace byte
                return offset + (!mask).trailing_zeros() as usize;
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

#[cfg(target_arch = "x86_64")]
#[inline]
fn slices_equal_x86(a: &[u8], b: &[u8]) -> bool {
    let len = a.len();
    debug_assert_eq!(len, b.len());

    if len < 32 {
        return a == b;
    }

    unsafe {
        use core::arch::x86_64::*;

        let mut offset = 0usize;

        // Process 64 bytes per iteration (4x16B)
        let end64 = len & !63;
        while offset < end64 {
            let e0 = _mm_cmpeq_epi8(
                _mm_loadu_si128(a.as_ptr().add(offset) as *const __m128i),
                _mm_loadu_si128(b.as_ptr().add(offset) as *const __m128i),
            );
            let e1 = _mm_cmpeq_epi8(
                _mm_loadu_si128(a.as_ptr().add(offset + 16) as *const __m128i),
                _mm_loadu_si128(b.as_ptr().add(offset + 16) as *const __m128i),
            );
            let e2 = _mm_cmpeq_epi8(
                _mm_loadu_si128(a.as_ptr().add(offset + 32) as *const __m128i),
                _mm_loadu_si128(b.as_ptr().add(offset + 32) as *const __m128i),
            );
            let e3 = _mm_cmpeq_epi8(
                _mm_loadu_si128(a.as_ptr().add(offset + 48) as *const __m128i),
                _mm_loadu_si128(b.as_ptr().add(offset + 48) as *const __m128i),
            );
            let merged = _mm_and_si128(_mm_and_si128(e0, e1), _mm_and_si128(e2, e3));
            if _mm_movemask_epi8(merged) != 0xFFFF {
                return false;
            }
            offset += 64;
        }

        // Process remaining 16B chunks
        let end16 = len & !15;
        while offset < end16 {
            let cmp = _mm_cmpeq_epi8(
                _mm_loadu_si128(a.as_ptr().add(offset) as *const __m128i),
                _mm_loadu_si128(b.as_ptr().add(offset) as *const __m128i),
            );
            if _mm_movemask_epi8(cmp) != 0xFFFF {
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
pub(crate) fn is_identifier_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
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

#[cfg(test)]
#[inline]
pub(crate) fn find_subslice_from(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    TEXT_SCAN.find_subslice_from(haystack, needle, from)
}

const NON_CODE_QUERY: &str = r#"[
    (comment) @nc
    (string_literal) @nc
    (raw_string_literal) @nc
    (char_literal) @nc
    (system_lib_string) @nc
    (concatenated_string) @nc
]"#;

pub(crate) fn non_code_ranges(tree: &tree_sitter::Tree) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let language: tree_sitter::Language = tree_sitter_cpp::LANGUAGE.into();
    if let Ok(query) = tree_sitter::Query::new(&language, NON_CODE_QUERY) {
        let mut cursor = tree_sitter::QueryCursor::new();
        let empty: &[u8] = &[];
        let mut matches = cursor.matches(&query, tree.root_node(), empty);
        while let Some(m) = {
            matches.advance();
            matches.get()
        } {
            for capture in m.captures {
                ranges.push((capture.node.start_byte(), capture.node.end_byte()));
            }
        }
    }
    ranges.sort_unstable_by_key(|r| r.0);
    ranges
}

#[cfg(test)]
mod tests {
    use super::{
        contains_subslice, count_byte, find_subslice_from,
        line_starts, split_lines_as_slices, split_lines_keepends, subslice_match_indices,
        TEXT_SCAN,
    };

    #[test]
    fn excludes_terminal_start() {
        assert_eq!(line_starts("a\nb\n", false), vec![0, 2]);
        assert_eq!(line_starts("a\nb", false), vec![0, 2]);
        assert_eq!(line_starts("", false), vec![0]);
    }

    #[test]
    fn includes_terminal_start() {
        assert_eq!(line_starts("a\nb\n", true), vec![0, 2, 4]);
        assert_eq!(line_starts("a\nb", true), vec![0, 2]);
        assert_eq!(line_starts("", true), vec![0]);
    }

    #[test]
    fn keepends_respects_empty() {
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
    fn slices_with_keepends() {
        assert!(split_lines_as_slices("", true).is_empty());
        assert_eq!(split_lines_as_slices("a\nb\n", true), vec!["a\n", "b\n"]);
        assert_eq!(split_lines_as_slices("a\nb", true), vec!["a\n", "b"]);
        assert_eq!(split_lines_as_slices("one", true), vec!["one"]);
    }

    #[test]
    fn slices_no_keepends() {
        assert_eq!(split_lines_as_slices("a\nb\n", false), vec!["a", "b"]);
        assert_eq!(split_lines_as_slices("a\nb", false), vec!["a", "b"]);
    }

    #[test]
    fn count_matches_scalar() {
        assert_eq!(count_byte("".as_bytes(), b'\n'), 0);
        assert_eq!(count_byte("x".as_bytes(), b'\n'), 0);
        assert_eq!(count_byte("x\n\nz".as_bytes(), b'\n'), 2);
    }

    #[test]
    fn count_large_buffer() {
        let buf = "x\n".repeat(500);
        assert_eq!(count_byte(buf.as_bytes(), b'\n'), 500);
    }

    #[test]
    fn subslice_basic_cases() {
        let bytes = b"alpha operator beta operator";
        assert!(contains_subslice(bytes, b"operator"));
        assert_eq!(find_subslice_from(bytes, b"operator", 0), Some(6));
        assert_eq!(find_subslice_from(bytes, b"operator", 7), Some(20));
        assert_eq!(find_subslice_from(bytes, b"missing", 0), None);
    }

    #[test]
    fn subslice_all_matches() {
        let matches = subslice_match_indices(b"aaaa", b"aa").collect::<Vec<_>>();
        assert_eq!(matches, vec![0, 1, 2]);
    }

    #[test]
    fn line_count_detects() {
        assert!(!TEXT_SCAN.has_line_count_changed("a\nb\n", "x\ny\n"));
        assert!(TEXT_SCAN.has_line_count_changed("a\nb\n", "a\nb\nc\n"));
        assert!(!TEXT_SCAN.has_line_count_changed("", ""));
    }

    // -- all_bytes_equal tests --

    #[test]
    fn all_bytes_empty() {
        assert!(TEXT_SCAN.all_bytes_equal(b"", b'-'));
    }

    #[test]
    fn all_bytes_single() {
        assert!(TEXT_SCAN.all_bytes_equal(b"-", b'-'));
        assert!(!TEXT_SCAN.all_bytes_equal(b"x", b'-'));
    }

    #[test]
    fn all_bytes_match() {
        let dashes = "-".repeat(100);
        assert!(TEXT_SCAN.all_bytes_equal(dashes.as_bytes(), b'-'));
    }

    #[test]
    fn mismatch_at_end() {
        let mut data = "-".repeat(99);
        data.push('x');
        assert!(!TEXT_SCAN.all_bytes_equal(data.as_bytes(), b'-'));
    }

    #[test]
    fn mismatch_at_start() {
        let mut data = String::from("x");
        data.push_str(&"-".repeat(99));
        assert!(!TEXT_SCAN.all_bytes_equal(data.as_bytes(), b'-'));
    }

    // -- leading_whitespace_byte_count tests --

    #[test]
    fn leading_ws_empty() {
        assert_eq!(TEXT_SCAN.leading_whitespace_byte_count(b""), 0);
    }

    #[test]
    fn leading_ws_none() {
        assert_eq!(TEXT_SCAN.leading_whitespace_byte_count(b"hello"), 0);
    }

    #[test]
    fn leading_spaces_tabs() {
        assert_eq!(TEXT_SCAN.leading_whitespace_byte_count(b"    int a;"), 4);
        assert_eq!(TEXT_SCAN.leading_whitespace_byte_count(b"\t\tint a;"), 2);
        assert_eq!(TEXT_SCAN.leading_whitespace_byte_count(b"  \tint a;"), 3);
    }

    #[test]
    fn leading_all_whitespace() {
        assert_eq!(TEXT_SCAN.leading_whitespace_byte_count(b"     "), 5);
    }

    #[test]
    fn leading_ws_large() {
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
    fn slices_different_lengths() {
        assert!(!TEXT_SCAN.slices_equal(b"abc", b"abcd"));
    }

    #[test]
    fn slices_equal_identical() {
        let data = "int main() { return 0; }";
        assert!(TEXT_SCAN.slices_equal(data.as_bytes(), data.as_bytes()));
    }

    #[test]
    fn slices_differ_end() {
        assert!(!TEXT_SCAN.slices_equal(
            b"int main() { return 0; }",
            b"int main() { return 1; }"
        ));
    }

    #[test]
    fn slices_large_identical() {
        let data = "x".repeat(4096);
        assert!(TEXT_SCAN.slices_equal(data.as_bytes(), data.as_bytes()));
    }

    #[test]
    fn slices_large_differ() {
        let a = "x".repeat(4096);
        let mut b = "x".repeat(4095);
        b.push('y');
        assert!(!TEXT_SCAN.slices_equal(a.as_bytes(), b.as_bytes()));
    }

}
