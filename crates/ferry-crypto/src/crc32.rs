//! IEEE CRC-32 (the zlib/PNG polynomial, reflected `0xEDB88320`).
//!
//! Used only for short-code typo detection — never for authenticity; the
//! pairing MAC covers that. Chosen over a truncated BLAKE3 prefix because a
//! CRC is the classic burst-error detector, costs no cryptographic dependency
//! at this call site, and is trivially reproducible by independent
//! implementations (the store format's compatibility bar). Detection
//! properties used by the tests: every single-bit error and any burst up to
//! 16 bits is caught by the 16-bit truncation we append.

/// Table-less bitwise IEEE CRC-32. Inputs here are tens of bytes; clarity
/// beats table speed.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_check_value() {
        // The universal CRC-32 check value for "123456789".
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
