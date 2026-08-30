














use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Key, Nonce,
};
use ferry_store::crypto::{CryptoError, PackCipher, TAG_LEN};





#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChaChaCipher;

impl PackCipher for ChaChaCipher {
    fn seal(
        &self,
        key: &[u8; ferry_store::crypto::KEY_LEN],
        nonce: &[u8; ferry_store::crypto::NONCE_LEN],
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
        cipher
            .encrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|_| {
                
                
                CryptoError::TagMismatch
            })
    }

    fn open(
        &self,
        key: &[u8; ferry_store::crypto::KEY_LEN],
        nonce: &[u8; ferry_store::crypto::NONCE_LEN],
        aad: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        if ciphertext.len() < TAG_LEN {
            return Err(CryptoError::MalformedCiphertext);
        }
        let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
        cipher
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: ciphertext,
                    aad,
                },
            )
            .map_err(|_| {
                
                
                CryptoError::TagMismatch
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferry_store::crypto::{
        body_aad, body_nonce, derive_pack_key, footer_aad, segment_count, PassthroughCipher,
        FOOTER_NONCE, SEGMENT_PLAIN_LEN,
    };
    use ferry_store::format::{hex, unhex, write_header, BlobKind, ContainerKind};

    fn cipher() -> ChaChaCipher {
        ChaChaCipher
    }

    #[test]
    fn rfc8439_2_8_2_known_answer_through_the_trait() {
        
        
        
        let key: [u8; 32] = core::array::from_fn(|i| 0x80 + i as u8);
        let nonce: [u8; 12] = unhex("070000004041424344454647").unwrap();
        let aad = unhex::<12>("50515253c0c1c2c3c4c5c6c7").unwrap();
        let pt = b"Ladies and Gentlemen of the class of '99: If I could offer you \
                   only one tip for the future, sunscreen would be it.";
        let ct = cipher().seal(&key, &nonce, &aad, pt).unwrap();
        assert_eq!(
            hex(&ct),
            concat!(
                "d31a8d34648e60db7b86afbc53ef7ec2",
                "a4aded51296e08fea9e2b5a736ee62d6",
                "3dbea45e8ca9671282fafb69da92728b",
                "1a71de0a9e060b2905d6a5b67ecd3b36",
                "92ddbd7f2d778b8c9803aee328091b58",
                "fab324e4fad675945585808b4831d7bc",
                "3ff4def08e4b7a9de576d26586cec64b",
                "6116",
                "1ae10b594f09e26a7e902ecbd0600691", 
            ),
            "seal must reproduce RFC 8439 section 2.8.2 exactly"
        );
        
        assert_eq!(cipher().open(&key, &nonce, &aad, &ct).unwrap(), pt);
    }

    #[test]
    fn round_trip_across_segment_boundaries() {
        let c = cipher();
        let key = derive_pack_key(&[3u8; 32], &[7u8; 16], ContainerKind::PackData);
        let header = write_header(ContainerKind::PackData);
        let aad = body_aad(&header, ContainerKind::PackData);
        for len in [0usize, 1, 100, SEGMENT_PLAIN_LEN - 1, SEGMENT_PLAIN_LEN] {
            let pt: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let nonce = body_nonce(0, 1);
            let ct = c.seal(&key, &nonce, &aad, &pt).unwrap();
            assert_eq!(ct.len(), pt.len() + TAG_LEN);
            assert_eq!(c.open(&key, &nonce, &aad, &ct).unwrap(), pt, "len {len}");
        }
    }

    #[test]
    fn wrong_key_nonce_or_aad_fails_authentication() {
        let c = cipher();
        let key = [1u8; 32];
        let nonce = body_nonce(0, 0);
        let aad = b"header-bound";
        let ct = c.seal(&key, &nonce, aad, b"payload").unwrap();

        let mut bad_key = key;
        bad_key[0] ^= 1;
        assert!(matches!(
            c.open(&bad_key, &nonce, aad, &ct),
            Err(CryptoError::TagMismatch)
        ));

        let bad_nonce = body_nonce(1, 0);
        assert!(matches!(
            c.open(&key, &bad_nonce, aad, &ct),
            Err(CryptoError::TagMismatch)
        ));

        let bad_aad = b"header-bound-tampered";
        assert!(matches!(
            c.open(&key, &nonce, bad_aad, &ct),
            Err(CryptoError::TagMismatch)
        ));
    }

    #[test]
    fn every_ciphertext_byte_flip_is_caught() {
        let c = cipher();
        let key = [9u8; 32];
        let nonce = body_nonce(2, 1);
        let ct = c
            .seal(&key, &nonce, b"aad", vec![42u8; 1000].as_slice())
            .unwrap();
        for idx in [0usize, 500, ct.len() - 17, ct.len() - 1] {
            let mut evil = ct.clone();
            evil[idx] ^= 0x80;
            assert!(
                matches!(
                    c.open(&key, &nonce, b"aad", &evil),
                    Err(CryptoError::TagMismatch)
                ),
                "flip at {idx} undetected"
            );
        }
        
        assert!(matches!(
            c.open(&key, &nonce, b"aad", &ct[..10]),
            Err(CryptoError::MalformedCiphertext)
        ));
    }

    #[test]
    fn nonce_and_aad_layouts_match_the_spec_pinned_hex() {
        
        
        
        let header_data = write_header(ContainerKind::PackData);
        assert_eq!(hex(&header_data), "46455252590101000000");
        assert_eq!(
            hex(&body_aad(&header_data, ContainerKind::PackData)),
            "464552525901010000000100",
            "header || kind(0x01) || role(0x00)"
        );
        let header_meta = write_header(ContainerKind::PackMeta);
        assert_eq!(hex(&header_meta), "46455252590201000000");
        assert_eq!(
            hex(&footer_aad(&header_meta, ContainerKind::PackMeta)),
            "464552525902010000000201",
            "header || kind(0x02) || role(0x01)"
        );
        
        assert_eq!(hex(&FOOTER_NONCE), "0000000000000000ffffffff");
        
        assert_eq!(hex(&body_nonce(0x1234567, 0)), "000000000000000002468ace");
        let _ = segment_count(0);
    }

    
    
    
    #[test]
    #[allow(clippy::large_stack_arrays)] 
    fn passthrough_and_chacha_framing_lengths_are_identical() {
        let passthrough = PassthroughCipher;
        let real = cipher();
        let key = [4u8; 32];
        let nonce = body_nonce(0, 1);

        for len in [0usize, 1, 63, 64, 65, 1000] {
            let pt: Vec<u8> = (0..len).map(|i| (i ^ 0x5a) as u8).collect();
            let a_stub = passthrough.seal(&key, &nonce, b"aad", &pt).unwrap();
            let a_real = real.seal(&key, &nonce, b"aad", &pt).unwrap();
            assert_eq!(a_stub.len(), a_real.len(), "len {len}");
            assert_eq!(a_real.len(), len + TAG_LEN);
        }

        
        
        
        use ferry_store::pack::{footer_plain, seal_pack_bytes, FooterEntry};
        let fmk = [11u8; 32];
        let salt = [22u8; 16];
        let bodies: [&[u8]; 2] = [
            b"one small blob plaintext",
            &[7u8; (SEGMENT_PLAIN_LEN * 2 + 123)],
        ];
        for body in bodies {
            let id = *blake3::hash(body).as_bytes();
            let entries = [FooterEntry {
                kind: BlobKind::DataChunk,
                id,
                plain_off: 0,
                plain_len: body.len() as u64,
            }];

            let stub_pack = seal_pack_bytes(
                ContainerKind::PackData,
                &fmk,
                &salt,
                body,
                &entries,
                &passthrough,
            )
            .unwrap();
            let real_pack =
                seal_pack_bytes(ContainerKind::PackData, &fmk, &salt, body, &entries, &real)
                    .unwrap();

            assert_eq!(stub_pack.len(), real_pack.len(), "pack sizes must match");
            
            let footer_pt_len = footer_plain(&entries, body.len() as u64).len();
            let segs = segment_count(body.len() as u64);
            let want = 26 
                + body.len() + 16 * segs as usize
                + footer_pt_len + 16 
                + 4 ;
            assert_eq!(real_pack.len(), want);
            assert_eq!(stub_pack.len(), want);

            
            let pid = *blake3::hash(&real_pack).as_bytes();
            let got = ferry_store::pack::read_blob(
                &real_pack,
                &pid,
                &fmk,
                &real,
                BlobKind::DataChunk,
                &id,
                None,
            )
            .unwrap();
            assert_eq!(got, body);

            
            
            
            
            let sid = *blake3::hash(&stub_pack).as_bytes();
            assert!(ferry_store::pack::read_blob(
                &stub_pack,
                &sid,
                &fmk,
                &real,
                BlobKind::DataChunk,
                &id,
                None,
            )
            .is_err());
        }
    }
}
