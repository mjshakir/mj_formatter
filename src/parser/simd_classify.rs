use smallvec::SmallVec;

pub fn find_uppercase_positions_into(bytes: &[u8], out: &mut SmallVec<[usize; 16]>) {
    out.clear();
    find_uppercase_positions_impl_into(bytes, out);
}

#[cfg(target_arch = "aarch64")]
fn find_uppercase_positions_impl_into(bytes: &[u8], positions: &mut SmallVec<[usize; 16]>) {
    use core::arch::aarch64::*;
    let chunks = bytes.len() / 16;
    unsafe {
        let a_val = vdupq_n_u8(b'A');
        let range = vdupq_n_u8(25);
        for chunk in 0..chunks {
            let offset = chunk * 16;
            let v = vld1q_u8(bytes.as_ptr().add(offset));
            let shifted = vsubq_u8(v, a_val);
            let is_upper = vcleq_u8(shifted, range);
            let mask_lo = vgetq_lane_u64(vreinterpretq_u64_u8(is_upper), 0);
            let mask_hi = vgetq_lane_u64(vreinterpretq_u64_u8(is_upper), 1);
            if mask_lo != 0 {
                for bit in 0..8 {
                    if (mask_lo >> (bit * 8)) & 0xFF != 0 {
                        positions.push(offset + bit);
                    }
                }
            }
            if mask_hi != 0 {
                for bit in 0..8 {
                    if (mask_hi >> (bit * 8)) & 0xFF != 0 {
                        positions.push(offset + 8 + bit);
                    }
                }
            }
        }
    }
    for (i, &b) in bytes.iter().enumerate().skip(chunks * 16) {
        if b.is_ascii_uppercase() {
            positions.push(i);
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn find_uppercase_positions_impl_into(bytes: &[u8], positions: &mut SmallVec<[usize; 16]>) {
    use core::arch::x86_64::*;
    let chunks = bytes.len() / 16;
    unsafe {
        let a_val = _mm_set1_epi8(b'A' as i8);
        let range = _mm_set1_epi8(25);
        for chunk in 0..chunks {
            let offset = chunk * 16;
            let v = _mm_loadu_si128(bytes.as_ptr().add(offset) as *const __m128i);
            let shifted = _mm_sub_epi8(v, a_val);
            let in_range = _mm_cmpeq_epi8(_mm_min_epu8(shifted, range), shifted);
            let mut mask = _mm_movemask_epi8(in_range) as u32;
            while mask != 0 {
                let bit = mask.trailing_zeros() as usize;
                positions.push(offset + bit);
                mask &= mask - 1;
            }
        }
    }
    for (i, &b) in bytes.iter().enumerate().skip(chunks * 16) {
        if b.is_ascii_uppercase() {
            positions.push(i);
        }
    }
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
fn find_uppercase_positions_impl_into(bytes: &[u8], positions: &mut SmallVec<[usize; 16]>) {
    for (i, &b) in bytes.iter().enumerate() {
        if b.is_ascii_uppercase() {
            positions.push(i);
        }
    }
}

#[cfg(target_arch = "aarch64")]
pub fn is_snake_case_bytes(bytes: &[u8]) -> bool {
    use core::arch::aarch64::*;
    let chunks = bytes.len() / 16;
    unsafe {
        let lower_a = vdupq_n_u8(b'a');
        let lower_range = vdupq_n_u8(25);
        let digit_0 = vdupq_n_u8(b'0');
        let digit_range = vdupq_n_u8(9);
        let underscore = vdupq_n_u8(b'_');
        for chunk in 0..chunks {
            let offset = chunk * 16;
            let v = vld1q_u8(bytes.as_ptr().add(offset));
            let is_lower = vcleq_u8(vsubq_u8(v, lower_a), lower_range);
            let is_digit = vcleq_u8(vsubq_u8(v, digit_0), digit_range);
            let is_under = vceqq_u8(v, underscore);
            let valid = vorrq_u8(vorrq_u8(is_lower, is_digit), is_under);
            let lo = vgetq_lane_u64(vreinterpretq_u64_u8(valid), 0);
            let hi = vgetq_lane_u64(vreinterpretq_u64_u8(valid), 1);
            if lo != u64::MAX || hi != u64::MAX {
                return false;
            }
        }
    }
    bytes.iter().skip(chunks * 16).all(|&b| {
        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'
    })
}

#[cfg(target_arch = "aarch64")]
pub fn is_upper_snake_case_bytes(bytes: &[u8]) -> bool {
    use core::arch::aarch64::*;
    let chunks = bytes.len() / 16;
    unsafe {
        let upper_a = vdupq_n_u8(b'A');
        let upper_range = vdupq_n_u8(25);
        let digit_0 = vdupq_n_u8(b'0');
        let digit_range = vdupq_n_u8(9);
        let underscore = vdupq_n_u8(b'_');
        for chunk in 0..chunks {
            let offset = chunk * 16;
            let v = vld1q_u8(bytes.as_ptr().add(offset));
            let is_upper = vcleq_u8(vsubq_u8(v, upper_a), upper_range);
            let is_digit = vcleq_u8(vsubq_u8(v, digit_0), digit_range);
            let is_under = vceqq_u8(v, underscore);
            let valid = vorrq_u8(vorrq_u8(is_upper, is_digit), is_under);
            let lo = vgetq_lane_u64(vreinterpretq_u64_u8(valid), 0);
            let hi = vgetq_lane_u64(vreinterpretq_u64_u8(valid), 1);
            if lo != u64::MAX || hi != u64::MAX {
                return false;
            }
        }
    }
    bytes.iter().skip(chunks * 16).all(|&b| {
        b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'
    })
}

#[cfg(target_arch = "x86_64")]
pub fn is_snake_case_bytes(bytes: &[u8]) -> bool {
    use core::arch::x86_64::*;
    let chunks = bytes.len() / 16;
    unsafe {
        let lower_a = _mm_set1_epi8(b'a' as i8);
        let lower_range = _mm_set1_epi8(25);
        let digit_0 = _mm_set1_epi8(b'0' as i8);
        let digit_range = _mm_set1_epi8(9);
        let underscore = _mm_set1_epi8(b'_' as i8);
        for chunk in 0..chunks {
            let offset = chunk * 16;
            let v = _mm_loadu_si128(bytes.as_ptr().add(offset) as *const __m128i);
            let shifted_lower = _mm_sub_epi8(v, lower_a);
            let is_lower = _mm_cmpeq_epi8(_mm_min_epu8(shifted_lower, lower_range), shifted_lower);
            let shifted_digit = _mm_sub_epi8(v, digit_0);
            let is_digit = _mm_cmpeq_epi8(_mm_min_epu8(shifted_digit, digit_range), shifted_digit);
            let is_under = _mm_cmpeq_epi8(v, underscore);
            let valid = _mm_or_si128(_mm_or_si128(is_lower, is_digit), is_under);
            if _mm_movemask_epi8(valid) != 0xFFFF {
                return false;
            }
        }
    }
    bytes.iter().skip(chunks * 16).all(|&b| {
        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'
    })
}

#[cfg(target_arch = "x86_64")]
pub fn is_upper_snake_case_bytes(bytes: &[u8]) -> bool {
    use core::arch::x86_64::*;
    let chunks = bytes.len() / 16;
    unsafe {
        let upper_a = _mm_set1_epi8(b'A' as i8);
        let upper_range = _mm_set1_epi8(25);
        let digit_0 = _mm_set1_epi8(b'0' as i8);
        let digit_range = _mm_set1_epi8(9);
        let underscore = _mm_set1_epi8(b'_' as i8);
        for chunk in 0..chunks {
            let offset = chunk * 16;
            let v = _mm_loadu_si128(bytes.as_ptr().add(offset) as *const __m128i);
            let shifted_upper = _mm_sub_epi8(v, upper_a);
            let is_upper = _mm_cmpeq_epi8(_mm_min_epu8(shifted_upper, upper_range), shifted_upper);
            let shifted_digit = _mm_sub_epi8(v, digit_0);
            let is_digit = _mm_cmpeq_epi8(_mm_min_epu8(shifted_digit, digit_range), shifted_digit);
            let is_under = _mm_cmpeq_epi8(v, underscore);
            let valid = _mm_or_si128(_mm_or_si128(is_upper, is_digit), is_under);
            if _mm_movemask_epi8(valid) != 0xFFFF {
                return false;
            }
        }
    }
    bytes.iter().skip(chunks * 16).all(|&b| {
        b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'
    })
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
pub fn is_snake_case_bytes(bytes: &[u8]) -> bool {
    bytes.iter().all(|&b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
pub fn is_upper_snake_case_bytes(bytes: &[u8]) -> bool {
    bytes.iter().all(|&b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}
