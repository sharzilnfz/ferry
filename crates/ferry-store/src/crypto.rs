











#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{hex, write_header};

    
    
    fn hkdf_sha256(salt: &[u8], ikm: &[u8], info: &[u8], len: usize) -> Vec<u8> {
        let hk = Hkdf::<Sha256>::new(Some(salt), ikm);
        let mut okm = vec![0u8; len];
        hk.expand(info, &mut okm).expect("valid okm length");
        okm
    }

    #[test]
    fn rfc5869_test_case_1_vector() {
        
        let ikm = [0x0bu8; 22];
        let salt: [u8; 13] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
        ];
        let info: [u8; 10] = [0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9];
        let okm = hkdf_sha256(&salt, &ikm, &info, 42);
        assert_eq!(
            hex(&okm),
            "3cb25f25faacd57a90434f64d0362f2a\
             2d2d0a90cf1a5a4c5db02d56ecc4c5bf\
             34007208d5b887185865"
        );
    }

    #[test]
    fn rfc5869_test_case_3_vector_zero_salt_and_info() {
        
        let ikm = [0x0bu8; 22];
        let okm = hkdf_sha256(&[], &ikm, &[], 42);
        assert_eq!(
            hex(&okm),
            "8da4e775a563c18f715f802a063c5a31\
             b8a11f5c5ee1879ec3454e5f3c738d2d\
             9d201395faa4b61a96c8"
        );
    }

    
    
    #[test]
    fn pack_key_derivation_known_answer() {
        let fmk: [u8; 32] = core::array::from_fn(|i| i as u8 + 1);
        let salt: [u8; 16] = core::array::from_fn(|i| 0xa0 + i as u8);
        let key = derive_pack_key(&fmk, &salt, ContainerKind::PackData);
        assert_eq!(
            hex(&key),
            "b8bc4034014093dc08f4bfd6ab80b0aef34251a6543447e6b5d08beeaf87b787"
        );
        
        let key_meta = derive_pack_key(&fmk, &salt, ContainerKind::PackMeta);
        assert_ne!(key, key_meta);
        
        let other_salt: [u8; 16] = core::array::from_fn(|i| 0xb0 + i as u8);
        assert_ne!(
            key,
            derive_pack_key(&fmk, &other_salt, ContainerKind::PackData)
        );
    }

    #[test]
    fn index_key_uses_index_info_string() {
        let fmk = [7u8; 32];
        let salt = [9u8; 16];
        
        assert_ne!(
            derive_index_key(&fmk, &salt),
            derive_pack_key(&fmk, &salt, ContainerKind::PackData)
        );
        assert_ne!(
            derive_index_key(&fmk, &salt),
            derive_pack_key(&fmk, &salt, ContainerKind::PackMeta)
        );
        assert_eq!(derive_index_key(&fmk, &salt).len(), 32);
    }

    #[test]
    fn body_nonce_layout_is_eight_zeros_plus_big_endian_word() {
        
        assert_eq!(
            body_nonce(0, 0),
            [0, 0, 0, 0, 0, 0, 0, 0, 0x00, 0x00, 0x00, 0x00]
        );
        
        assert_eq!(
            body_nonce(0, 1),
            [0, 0, 0, 0, 0, 0, 0, 0, 0x00, 0x00, 0x00, 0x01]
        );
        
        
        assert_eq!(
            body_nonce(5, 1),
            [0, 0, 0, 0, 0, 0, 0, 0, 0x00, 0x00, 0x00, 0x0B]
        );
        
        
        assert_eq!(
            body_nonce(0x1234567, 0),
            [0, 0, 0, 0, 0, 0, 0, 0, 0x02, 0x46, 0x8A, 0xCE]
        );
    }

    #[test]
    fn body_nonce_rejects_counter_overflow_and_bad_flags() {
        
        
        assert!(body_nonce_checked(1 << 31, 0).is_err());
        assert!(body_nonce_checked((1 << 31) - 1, 1).is_ok());
        
        assert!(body_nonce_checked(0, 2).is_err());
    }

    #[test]
    fn footer_nonce_is_reserved_counter() {
        assert_eq!(
            FOOTER_NONCE,
            [0, 0, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF]
        );
        
        
        assert_eq!(&FOOTER_NONCE[8..], &0xFFFFFFFFu32.to_be_bytes());
    }

    #[test]
    fn aad_binds_header_kind_and_role() {
        let header = write_header(ContainerKind::PackData);
        
        assert_eq!(
            body_aad(&header, ContainerKind::PackData),
            [&header[..], &[ContainerKind::PackData.to_u8(), 0x00][..]].concat()
        );
        
        assert_eq!(
            footer_aad(&header, ContainerKind::PackMeta),
            [&header[..], &[ContainerKind::PackMeta.to_u8(), 0x01][..]].concat()
        );
        assert_eq!(body_aad(&header, ContainerKind::PackData).len(), 12);
    }

    #[test]
    fn segment_math_matches_spec_formula() {
        assert_eq!(segment_count(0), 0);
        assert_eq!(segment_count(1), 1);
        assert_eq!(segment_count(SEGMENT_PLAIN_LEN as u64 - 1), 1);
        assert_eq!(segment_count(SEGMENT_PLAIN_LEN as u64), 1); 
        assert_eq!(segment_count(SEGMENT_PLAIN_LEN as u64 + 1), 2);
        assert_eq!(segment_count(3 * SEGMENT_PLAIN_LEN as u64), 3);

        
        for len in [
            0u64,
            1,
            100,
            SEGMENT_PLAIN_LEN as u64,
            SEGMENT_PLAIN_LEN as u64 + 17,
        ] {
            let region = body_region_len(len);
            assert_eq!(region, len + TAG_LEN as u64 * segment_count(len));
        }
    }

    #[test]
    fn passthrough_cipher_preserves_framing_shape() {
        let c = PassthroughCipher;
        let key = [1u8; 32];
        let nonce = [2u8; 12];
        let pt = b"hello ferry";
        let ct = c.seal(&key, &nonce, b"aad", pt).unwrap();
        
        assert_eq!(ct.len(), pt.len() + TAG_LEN);
        
        assert_eq!(&ct[..pt.len()], pt);
        assert!(ct[pt.len()..].iter().all(|&b| b == 0));
        
        assert_eq!(c.open(&key, &nonce, b"aad", &ct).unwrap(), pt);
    }

    #[test]
    fn passthrough_open_rejects_short_or_untagged_ciphertext() {
        let c = PassthroughCipher;
        let err = c.open(&[0; 32], &[0; 12], b"", b"short").unwrap_err();
        assert!(matches!(err, CryptoError::MalformedCiphertext));
        
        let mut ct = vec![0u8; 32];
        ct[20] = 1;
        assert!(c.open(&[0; 32], &[0; 12], b"", &ct).is_err());
    }
}

use hkdf::Hkdf;
use sha2::Sha256;
use thiserror::Error;

use crate::format::ContainerKind;


pub const SEGMENT_PLAIN_LEN: usize = 65536;

pub const TAG_LEN: usize = 16;

pub const SALT_LEN: usize = 16;

pub const KEY_LEN: usize = 32;

pub const NONCE_LEN: usize = 12;

pub const INFO_PACK_DATA: &[u8] = b"ferry/v1/pack/data";
pub const INFO_PACK_META: &[u8] = b"ferry/v1/pack/meta";
pub const INFO_INDEX: &[u8] = b"ferry/v1/index";

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("ciphertext too short to contain a tag")]
    MalformedCiphertext,
    #[error("stub tag check failed: ciphertext was not produced by PassthroughCipher")]
    TagMismatch,
}

fn hkdf_expand(info: &[u8], salt: &[u8], ikm: &[u8]) -> [u8; KEY_LEN] {
    let hk = Hkdf::<Sha256>::new(if salt.is_empty() { None } else { Some(salt) }, ikm);
    let mut okm = [0u8; KEY_LEN];
    hk.expand(info, &mut okm)
        .expect("32-byte OKM is always valid");
    okm
}




pub fn derive_pack_key(
    fmk: &[u8; KEY_LEN],
    salt: &[u8; SALT_LEN],
    kind: ContainerKind,
) -> [u8; KEY_LEN] {
    let info = match kind {
        ContainerKind::PackData => INFO_PACK_DATA,
        ContainerKind::PackMeta => INFO_PACK_META,
        _ => panic!("derive_pack_key requires a pack container kind"),
    };
    hkdf_expand(info, salt, fmk)
}


pub fn derive_index_key(fmk: &[u8; KEY_LEN], salt: &[u8; SALT_LEN]) -> [u8; KEY_LEN] {
    hkdf_expand(INFO_INDEX, salt, fmk)
}



pub fn body_nonce(counter: u32, last_flag: u8) -> [u8; NONCE_LEN] {
    body_nonce_checked(counter, last_flag).expect("valid stream counter")
}


pub fn body_nonce_checked(counter: u32, last_flag: u8) -> Result<[u8; NONCE_LEN], CryptoError> {
    if last_flag > 1 {
        return Err(CryptoError::MalformedCiphertext);
    }
    if counter >= (1 << 31) {
        return Err(CryptoError::MalformedCiphertext);
    }
    let word = (counter << 1) | u32::from(last_flag);
    let mut nonce = [0u8; NONCE_LEN];
    nonce[8..].copy_from_slice(&word.to_be_bytes());
    Ok(nonce)
}



pub const FOOTER_NONCE: [u8; NONCE_LEN] = [0, 0, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF];


pub fn body_aad(header: &[u8; crate::format::HEADER_LEN], kind: ContainerKind) -> Vec<u8> {
    [&header[..], &[kind.to_u8(), 0x00]].concat()
}


pub fn footer_aad(header: &[u8; crate::format::HEADER_LEN], kind: ContainerKind) -> Vec<u8> {
    [&header[..], &[kind.to_u8(), 0x01]].concat()
}



pub fn segment_count(body_plain_len: u64) -> u64 {
    body_plain_len.div_ceil(SEGMENT_PLAIN_LEN as u64)
}



pub fn body_region_len(body_plain_len: u64) -> u64 {
    body_plain_len + TAG_LEN as u64 * segment_count(body_plain_len)
}

#[derive(Debug, Error)]
pub enum SealError {
    #[error("cipher failure: {0}")]
    Cipher(String),
}







pub trait PackCipher: Send + Sync {
    fn seal(
        &self,
        key: &[u8; KEY_LEN],
        nonce: &[u8; NONCE_LEN],
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<Vec<u8>, CryptoError>;

    fn open(
        &self,
        key: &[u8; KEY_LEN],
        nonce: &[u8; NONCE_LEN],
        aad: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, CryptoError>;
}










#[cfg(any(test, feature = "test-util"))]
pub struct PassthroughCipher;

#[cfg(any(test, feature = "test-util"))]
impl PackCipher for PassthroughCipher {
    fn seal(
        &self,
        _key: &[u8; KEY_LEN],
        _nonce: &[u8; NONCE_LEN],
        _aad: &[u8],
        plaintext: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        let mut out = Vec::with_capacity(plaintext.len() + TAG_LEN);
        out.extend_from_slice(plaintext);
        out.extend_from_slice(&[0u8; TAG_LEN]);
        Ok(out)
    }

    fn open(
        &self,
        _key: &[u8; KEY_LEN],
        _nonce: &[u8; NONCE_LEN],
        _aad: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        if ciphertext.len() < TAG_LEN {
            return Err(CryptoError::MalformedCiphertext);
        }
        let split = ciphertext.len() - TAG_LEN;
        if ciphertext[split..].iter().any(|&b| b != 0) {
            
            
            return Err(CryptoError::TagMismatch);
        }
        Ok(ciphertext[..split].to_vec())
    }
}
