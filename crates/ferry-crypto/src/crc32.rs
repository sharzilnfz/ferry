pub fn crc32(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0x0000_0000);
    }

    #[test]
    fn detects_every_single_bit_flip_in_short_payloads() {
        let payload: Vec<u8> = (0..=64u8).collect();
        let base = crc32(&payload);
        for byte in 0..payload.len() {
            for bit in 0..8 {
                let mut p = payload.clone();
                p[byte] ^= 1 << bit;
                assert_ne!(crc32(&p), base, "bit {bit} of byte {byte} undetected");
            }
        }
    }
}
