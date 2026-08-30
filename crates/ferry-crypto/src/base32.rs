












pub const ALPHABET: &[u8; 32] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Base32Error {
    #[error("invalid character {0:?} in code (use digits 2-9 or letters A-Z without I/O)")]
    InvalidChar(char),
    #[error("code is empty")]
    Empty,
}


pub fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len() * 8 / 5 + 2);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for &b in data {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((acc >> bits) & 0x1f) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((acc << (5 - bits)) & 0x1f) as usize] as char);
    }
    out
}





pub fn decode(s: &str) -> Result<Vec<u8>, Base32Error> {
    if s.is_empty() {
        return Err(Base32Error::Empty);
    }
    let mut out = Vec::with_capacity(s.len() * 5 / 8);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for ch in s.chars() {
        let up = ch.to_ascii_uppercase();
        let v = match up {
            '2'..='9' => up as u32 - '2' as u32,
            
            'A'..='H' => 8 + up as u32 - 'A' as u32,
            'J' => 16,
            'K'..='N' => 17 + up as u32 - 'K' as u32,
            'P'..='Z' => 21 + up as u32 - 'P' as u32,
            _ => return Err(Base32Error::InvalidChar(ch)),
        };
        acc = (acc << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_round_trip_and_single_symbol_values() {
        assert_eq!(encode(b""), "");
        assert_eq!(decode(""), Err(Base32Error::Empty));
        
        
        for (i, &sym) in ALPHABET.iter().enumerate() {
            let s = String::from_utf8(vec![sym, ALPHABET[0], ALPHABET[0], ALPHABET[0]]).unwrap();
            let decoded = decode(&s).unwrap();
            assert_eq!((decoded[0] >> 3) as usize, i, "symbol {}", sym as char);
        }
    }

    #[test]
    fn round_trips_arbitrary_bytes() {
        for len in [1usize, 2, 5, 10, 12, 16, 31, 100] {
            let data: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            let enc = encode(&data);
            assert_eq!(decode(&enc).unwrap(), data, "len {len}");
        }
    }

    #[test]
    fn encoded_length_is_ceil_bits_over_five() {
        assert_eq!(encode(&[0u8; 10]).len(), 16); 
        assert_eq!(encode(&[0u8; 12]).len(), 20); 
        assert_eq!(encode(&[0u8; 2]).len(), 4); 
    }

    #[test]
    fn output_never_contains_ambiguous_characters() {
        
        for &sym in ALPHABET {
            assert!(!matches!(sym, b'0' | b'1' | b'I' | b'O'));
        }
        assert_eq!(ALPHABET.len(), 32);
        let mut sorted = ALPHABET.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 32, "alphabet symbols must be distinct");
        
        for len in [1usize, 5, 12] {
            let data: Vec<u8> = (0..len).map(|i| (i * 91 + 3) as u8).collect();
            for ch in encode(&data).chars() {
                assert!(!matches!(ch, '0' | '1' | 'I' | 'O'), "ambiguous {ch}");
            }
        }
    }

    #[test]
    fn decode_accepts_lowercase_and_rejects_lookalikes_and_garbage() {
        let upper = encode(&[0x12, 0x34, 0x56]);
        assert_eq!(
            decode(&upper.to_lowercase()).unwrap(),
            vec![0x12, 0x34, 0x56]
        );
        
        for bad in ['0', '1', 'I', 'O'] {
            let mut tampered = upper.clone();
            tampered.replace_range(0..1, &bad.to_string());
            assert_eq!(decode(&tampered), Err(Base32Error::InvalidChar(bad)));
        }
        assert_eq!(decode("AB-C"), Err(Base32Error::InvalidChar('-')));
    }

    #[test]
    fn known_answer_rfc4648_bit_packing_remapped_to_canonical_alphabet() {
        
        
        
        let remap = |rfc: &str| -> String {
            let rfc_alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
            rfc.bytes()
                .map(|b| {
                    let v = rfc_alphabet
                        .iter()
                        .position(|&x| x == b)
                        .expect("rfc symbol");
                    ALPHABET[v] as char
                })
                .collect()
        };
        assert_eq!(encode(b"f"), remap("MY"));
        assert_eq!(encode(b"fo"), remap("MZXQ"));
        assert_eq!(encode(b"foo"), remap("MZXW6"));
        assert_eq!(encode(b"foobar"), remap("MZXW6YTBOI"));
        
        
        for ch in encode(b"foobar").chars() {
            assert!(!matches!(ch, '0' | '1' | 'I' | 'O'));
        }
    }
}
