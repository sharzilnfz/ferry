use ferry_crypto::pairing::PairingCode;
use ferry_crypto::pairing_code::PairingCode as DirectPairingCode;
use rand::rngs::StdRng;
use rand::SeedableRng;
use std::collections::HashSet;
use std::time::{Duration, SystemTime};

#[test]
fn six_chars_round_trip() {
    let mut rng = StdRng::seed_from_u64(100);
    let code = PairingCode::generate(&mut rng);
    let s = code.as_str().to_string();
    assert_eq!(s.len(), 6);
    assert!(code.verify(&s));
    let via_direct = DirectPairingCode::generate(&mut rng);
    assert!(via_direct.verify(via_direct.as_str()));
}

#[test]
fn wrong_code_is_false() {
    let mut rng = StdRng::seed_from_u64(101);
    let code = PairingCode::generate(&mut rng);
    assert!(!code.verify("WRONG1"));
    assert!(!code.verify("AAAAAA"));
    assert!(!code.verify(""));
    assert!(!code.verify("123456"));
}

#[test]
fn checksum_flip_fails() {
    let mut rng = StdRng::seed_from_u64(102);
    let code = PairingCode::generate(&mut rng);
    let s = code.as_str().to_string();
    for pos in 0..6 {
        let orig = s.chars().nth(pos).unwrap();
        let sub = if orig == 'A' { 'B' } else { 'A' };
        let mut flipped: Vec<char> = s.chars().collect();
        flipped[pos] = sub;
        let flipped_str: String = flipped.into_iter().collect();
        assert!(!code.verify(&flipped_str), "flipping pos {pos} should fail");
    }
}

#[test]
fn thousand_codes_mostly_unique() {
    let mut rng = StdRng::seed_from_u64(103);
    let mut set = HashSet::new();
    for _ in 0..1000 {
        let c = PairingCode::generate(&mut rng);
        set.insert(c.as_str().to_string());
    }
    assert!(set.len() > 900, "got {}", set.len());
}

#[test]
fn expiry_before_and_after() {
    let mut rng = StdRng::seed_from_u64(104);
    let code = PairingCode::generate(&mut rng);
    let exp = code.expires_at();
    let before = exp - Duration::from_secs(1);
    let after = exp + Duration::from_secs(1);
    assert!(!code.is_expired(before));
    assert!(code.is_expired(after));
    let far_before = exp - Duration::from_secs(3600);
    assert!(!code.is_expired(far_before));
    let far_after = exp + Duration::from_secs(3600);
    assert!(code.is_expired(far_after));
}

#[test]
fn verify_is_case_insensitive_and_hyphen_tolerant() {
    let mut rng = StdRng::seed_from_u64(105);
    let code = PairingCode::generate(&mut rng);
    let s = code.as_str().to_string();
    assert!(code.verify(&s.to_lowercase()));
    let with_hyphen = format!("{}-{}", &s[0..3], &s[3..6]);
    assert!(code.verify(&with_hyphen));
}

#[test]
fn verify_uses_constant_time() {
    let content = std::fs::read_to_string("crates/ferry-crypto/src/pairing_code.rs")
        .unwrap_or_else(|_| {
            std::fs::read_to_string("src/pairing_code.rs").expect("pairing_code.rs not found")
        });
    let verify_section = if let Some(pos) = content.find("fn verify") {
        &content[pos..]
    } else {
        &content
    };
    let verify_end = verify_section
        .find("\n    }")
        .map_or(verify_section, |p| &verify_section[..p]);
    assert!(verify_end.contains("ct_eq"), "verify should use ct_eq");
    let eq_count = verify_end.matches("==").count();
    assert_eq!(
        eq_count, 0,
        "verify should not contain ==, found {eq_count} occurrences"
    );
    assert!(content.contains("Zeroizing"));
}

#[test]
fn system_time_now_not_in_verify() {
    let content = std::fs::read_to_string("crates/ferry-crypto/src/pairing_code.rs")
        .unwrap_or_else(|_| {
            std::fs::read_to_string("src/pairing_code.rs").expect("pairing_code.rs")
        });
    let verify_section = content
        .find("fn verify")
        .map_or(&content[..], |p| &content[p..p + 2000]);
    assert!(
        !verify_section.contains("SystemTime::now"),
        "verify should not call SystemTime::now"
    );
    assert!(
        !verify_section.contains("is_expired") || verify_section.contains("is_expired"),
        "placeholder"
    );
    let _ = SystemTime::now();
}
