use smallvec::SmallVec;

use crate::parser::simd_classify::find_uppercase_positions_into;
use crate::parser::simd_classify::is_snake_case_bytes;
use crate::parser::simd_classify::is_upper_snake_case_bytes;

use super::NamingConventionsPolicy;

impl NamingConventionsPolicy {
    pub(super) fn is_snake_case(name: &str) -> bool {
        if name.is_empty() {
            return true;
        }
        let bytes = name.as_bytes();
        if !(bytes[0].is_ascii_lowercase() || bytes[0] == b'_') {
            return false;
        }
        is_snake_case_bytes(&bytes[1..])
    }

    pub(super) fn is_upper_snake_case(name: &str) -> bool {
        if name.is_empty() {
            return false;
        }
        let bytes = name.as_bytes();
        if !is_upper_snake_case_bytes(bytes) {
            return false;
        }
        bytes.iter().any(|&b| b.is_ascii_alphabetic())
    }

    pub(super) fn is_constant_like_identifier(name: &str) -> bool {
        if Self::is_upper_snake_case(name) {
            return true;
        }
        if let Some(rest) = name.strip_prefix("C_") {
            return !rest.is_empty() && Self::is_upper_snake_case(rest);
        }
        if let Some(rest) = name.strip_prefix("S_") {
            return !rest.is_empty() && Self::is_upper_snake_case(rest);
        }
        if let Some(rest) = name.strip_prefix("c_") {
            return !rest.is_empty() && Self::is_snake_case(rest);
        }
        if let Some(rest) = name.strip_prefix("s_") {
            return !rest.is_empty() && Self::is_snake_case(rest);
        }
        false
    }

    pub(super) fn to_snake_case_into(value: &str, pos_buf: &mut SmallVec<[usize; 16]>, out: &mut String) {
        out.clear();
        let bytes = value.as_bytes();
        let len = bytes.len();

        pos_buf.clear();
        find_uppercase_positions_into(bytes, pos_buf);

        if pos_buf.is_empty() {
            out.push_str(value);
            return;
        }

        out.reserve(len.saturating_add(4));
        let result = unsafe { out.as_mut_vec() };
        let mut pos_idx = 0;
        for i in 0..len {
            if pos_idx < pos_buf.len() && pos_buf[pos_idx] == i {
                let prev = if i > 0 { Some(bytes[i - 1]) } else { None };
                let next = bytes.get(i + 1).copied();
                let boundary = prev
                    .is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit())
                    || (prev.is_some_and(|p| p.is_ascii_uppercase())
                        && next.is_some_and(|n| n.is_ascii_lowercase()));
                if boundary && result.last() != Some(&b'_') {
                    result.push(b'_');
                }
                result.push(bytes[i].to_ascii_lowercase());
                pos_idx += 1;
            } else {
                result.push(bytes[i]);
            }
        }
    }
}
