pub fn hash(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values() {
        assert_eq!(hash(b""), 0x0000_0000);
        assert_eq!(hash(b"123456789"), 0xCBF4_3926);
    }
}
