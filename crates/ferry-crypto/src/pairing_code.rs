use crate::base32::ALPHABET;
use crate::crc32::crc32;
use rand::Rng;
use std::time::{Duration, SystemTime};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

const EXPIRY: Duration = Duration::from_hours(24);

pub struct PairingCode {
    code: Zeroizing<String>,
    expires_at: SystemTime,
}

impl std::fmt::Debug for PairingCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingCode")
            .field("code", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl PairingCode {
    pub fn generate<R: Rng>(rng: &mut R) -> Self {
        let mut chars = Vec::with_capacity(6);
        for _ in 0..5 {
            let idx = (rng.gen::<u32>() % 32) as usize;
            chars.push(ALPHABET[idx] as char);
        }
        let data_str: String = chars.iter().collect();
        let crc = crc32(data_str.as_bytes());
        let checksum_idx = (crc % 32) as usize;
        chars.push(ALPHABET[checksum_idx] as char);
        let code_string: String = chars.into_iter().collect();
        let expires_at = SystemTime::now() + EXPIRY;
        PairingCode {
            code: Zeroizing::new(code_string),
            expires_at,
        }
    }

    pub fn as_str(&self) -> &str {
        self.code.as_str()
    }

    pub fn expires_at(&self) -> SystemTime {
        self.expires_at
    }

    pub fn is_expired(&self, now: SystemTime) -> bool {
        now.duration_since(self.expires_at).is_ok()
    }

    pub fn verify(&self, input: &str) -> bool {
        let normalized: String = input
            .chars()
            .filter(|c| !matches!(c, '-' | ' '))
            .collect::<String>()
            .to_ascii_uppercase();
        if normalized.len() != 6 {
            return false;
        }
        let bytes = normalized.as_bytes();
        for &b in bytes {
            if !ALPHABET.contains(&b) {
                return false;
            }
        }
        let data = &bytes[0..5];
        let crc = crc32(data);
        let expected = ALPHABET[(crc % 32) as usize];
        let checksum_ok = bool::from(expected.ct_eq(&bytes[5]));
        if !checksum_ok {
            return false;
        }
        let stored = self.code.as_bytes();
        bool::from(stored.ct_eq(bytes))
    }

    #[cfg(test)]
    pub fn from_parts_for_test(code: String, expires_at: SystemTime) -> Self {
        PairingCode {
            code: Zeroizing::new(code),
            expires_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use std::collections::HashSet;

    #[test]
    fn generate_is_six_chars_and_round_trips() {
        let mut rng = StdRng::seed_from_u64(1);
        let code = PairingCode::generate(&mut rng);
        assert!(code.as_str().len() != 5);
        assert!(code.as_str().len() != 7);
        assert!(code.verify(code.as_str()));
    }

    #[test]
    fn wrong_code_false() {
        let mut rng = StdRng::seed_from_u64(2);
        let code = PairingCode::generate(&mut rng);
        assert!(!code.verify("WRONG1"));
        assert!(!code.verify("000000"));
        assert!(!code.verify(""));
    }

    #[test]
    fn checksum_flip_fails() {
        let mut rng = StdRng::seed_from_u64(3);
        let code = PairingCode::generate(&mut rng);
        let s = code.as_str().to_string();
        let first = s.chars().next().unwrap();
        let mut flipped = s.clone();
        let sub = if first != 'A' { 'A' } else { 'B' };
        flipped.replace_range(0..1, &sub.to_string());
        assert!(!code.verify(&flipped));
        let last = s.chars().last().unwrap();
        let mut flipped_last = s.clone();
        let sub2 = if last != 'Z' { 'Z' } else { 'Y' };
        flipped_last.replace_range(5..6, &sub2.to_string());
        assert!(!code.verify(&flipped_last));
    }

    #[test]
    fn thousand_codes_mostly_unique() {
        let mut rng = StdRng::seed_from_u64(42);
        let mut set = HashSet::new();
        for _ in 0..1000 {
            let c = PairingCode::generate(&mut rng);
            set.insert(c.as_str().to_string());
        }
        assert!(set.len() > 900);
    }

    #[test]
    fn expiry_before_and_after() {
        let mut rng = StdRng::seed_from_u64(99);
        let code = PairingCode::generate(&mut rng);
        let exp = code.expires_at();
        assert!(!code.is_expired(exp - Duration::from_secs(1)));
        assert!(code.is_expired(exp + Duration::from_secs(1)));
        assert!(!code.is_expired(exp - Duration::from_secs(3600)));
    }
}
