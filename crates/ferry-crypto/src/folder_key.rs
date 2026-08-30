























use crate::identity::{DeviceId, DeviceIdentity};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Nonce,
};
use hkdf::Hkdf;
use rand::{CryptoRng, RngCore};
use sha2::Sha256;
use thiserror::Error;
use zeroize::Zeroizing;


pub const KEYWRAP_INFO: &[u8] = b"ferry/v1/keywrap";

pub const WRAPPED_LEN: usize = 80;

pub type Fmk = [u8; 32];

#[derive(Debug, Error)]
pub enum FolderKeyError {
    #[error("wrapped key envelope failed authentication")]
    Auth,
    #[error("wrapped_len MUST be 80 in v1, got {0}")]
    BadLen(usize),
    #[error("peer public key produced a degenerate shared secret")]
    DegeneratePeerKey,
}


pub fn generate_fmk() -> Fmk {
    let mut fmk: Fmk = [0u8; 32];
    use rand::RngCore;
    rand::rngs::OsRng.fill_bytes(&mut fmk);
    fmk
}

fn hkdf_wrap_key(shared: &[u8], salt: &[u8]) -> Zeroizing<[u8; 32]> {
    let hk = Hkdf::<Sha256>::new(Some(salt), shared);
    let mut okm = Zeroizing::new([0u8; 32]);
    hk.expand(KEYWRAP_INFO, okm.as_mut())
        .expect("32-byte OKM is always valid");
    okm
}




pub fn derive_wrap_key(
    shared: &[u8; 32],
    ephemeral_pub: &DeviceId,
    device_pub: &DeviceId,
) -> Zeroizing<[u8; 32]> {
    let mut salt = Vec::with_capacity(64);
    salt.extend_from_slice(ephemeral_pub);
    salt.extend_from_slice(device_pub);
    hkdf_wrap_key(shared, &salt)
}

fn seal_fmk(
    wrap_key: &[u8; 32],
    folder_id: &[u8; 16],
    fmk: &Fmk,
) -> Result<Vec<u8>, FolderKeyError> {
    let cipher = ChaCha20Poly1305::new(chacha20poly1305::Key::from_slice(wrap_key));
    cipher
        .encrypt(
            Nonce::from_slice(&[0u8; 12]),
            Payload {
                msg: fmk,
                aad: folder_id,
            },
        )
        .map_err(|_| FolderKeyError::Auth)
}

fn open_fmk(
    wrap_key: &[u8; 32],
    folder_id: &[u8; 16],
    ciphertext: &[u8],
) -> Result<Zeroizing<[u8; 32]>, FolderKeyError> {
    let cipher = ChaCha20Poly1305::new(chacha20poly1305::Key::from_slice(wrap_key));
    let pt = cipher
        .decrypt(
            Nonce::from_slice(&[0u8; 12]),
            Payload {
                msg: ciphertext,
                aad: folder_id,
            },
        )
        .map_err(|_| FolderKeyError::Auth)?;
    let mut fmk: Fmk = [0u8; 32];
    fmk.copy_from_slice(&pt);
    Ok(Zeroizing::new(fmk))
}




pub fn wrap_folder_key_with_rng(
    fmk: &Fmk,
    folder_id: &[u8; 16],
    device_pub: &DeviceId,
    mut rng: impl RngCore + CryptoRng,
) -> Result<[u8; WRAPPED_LEN], FolderKeyError> {
    let ephemeral = x25519_dalek::StaticSecret::random_from_rng(&mut rng);
    let mut eph_pub: DeviceId = [0u8; 32];
    eph_pub.copy_from_slice(x25519_dalek::PublicKey::from(&ephemeral).as_bytes());

    let shared = ephemeral.diffie_hellman(&x25519_dalek::PublicKey::from(*device_pub));
    if !shared.was_contributory() {
        return Err(FolderKeyError::DegeneratePeerKey);
    }
    let mut shared_bytes: [u8; 32] = [0u8; 32];
    shared_bytes.copy_from_slice(shared.as_bytes());
    let wrap_key = derive_wrap_key(&shared_bytes, &eph_pub, device_pub);

    let ct = seal_fmk(&wrap_key, folder_id, fmk)?;
    debug_assert_eq!(ct.len(), 48);
    let mut out = [0u8; WRAPPED_LEN];
    out[..32].copy_from_slice(&eph_pub);
    out[32..].copy_from_slice(&ct);
    Ok(out)
}


pub fn wrap_folder_key(
    fmk: &Fmk,
    folder_id: &[u8; 16],
    device_pub: &DeviceId,
) -> Result<[u8; WRAPPED_LEN], FolderKeyError> {
    wrap_folder_key_with_rng(fmk, folder_id, device_pub, rand::rngs::OsRng)
}




pub fn unwrap_folder_key(
    wrapped: &[u8; WRAPPED_LEN],
    folder_id: &[u8; 16],
    identity: &DeviceIdentity,
) -> Result<Zeroizing<[u8; 32]>, FolderKeyError> {
    let eph_pub: DeviceId = wrapped[..32].try_into().expect("split at 32");
    let shared = identity
        .diffie_hellman(&eph_pub)
        .map_err(|_| FolderKeyError::DegeneratePeerKey)?;
    let wrap_key = derive_wrap_key(&shared, &eph_pub, identity.public());
    open_fmk(&wrap_key, folder_id, &wrapped[32..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::DeviceIdentity;
    use crate::testing::FixedRng;
    use ferry_store::format::unhex;

    const ALICE_SK_HEX: &str = "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a";
    const ALICE_PK_HEX: &str = "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a";
    const BOB_SK_HEX: &str = "5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb";
    const BOB_PK_HEX: &str = "de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f";
    const RFC_SHARED_HEX: &str = "4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742";

    #[test]
    fn fmk_generation_is_32_random_distinct_bytes() {
        let a = generate_fmk();
        let b = generate_fmk();
        assert_ne!(a, b, "two draws colliding would mean a broken CSPRNG");
    }

    #[test]
    fn wrap_key_schedule_matches_independent_hkdf_reference() {
        
        
        
        
        
        let shared: [u8; 32] = unhex(RFC_SHARED_HEX).unwrap();
        let eph_pub: DeviceId = unhex(BOB_PK_HEX).unwrap();
        let dev_pub: DeviceId = unhex(ALICE_PK_HEX).unwrap();
        let wk = derive_wrap_key(&shared, &eph_pub, &dev_pub);
        assert_eq!(
            ferry_store::format::hex(wk.as_ref()),
            "e3c1787d10dcaadf06c5d907bd5796b2a260d057f471cea8a54ad30a4dfe71b4"
        );
        
        let swapped = derive_wrap_key(&shared, &dev_pub, &eph_pub);
        assert_ne!(wk, swapped);
    }

    #[test]
    fn wrap_output_is_exactly_80_bytes_ephemeral_pub_first() {
        let alice = DeviceIdentity::from_secret_bytes(&unhex(ALICE_SK_HEX).unwrap());
        let fmk = generate_fmk();
        let folder_id = [7u8; 16];
        let rng = FixedRng::new(BOB_SK_HEX);
        let wrapped = wrap_folder_key_with_rng(&fmk, &folder_id, alice.public(), rng).unwrap();
        assert_eq!(wrapped.len(), WRAPPED_LEN);
        assert_eq!(wrapped.len(), 80, "spec: wrapped_len MUST be 80");
        
        
        assert_eq!(&wrapped[..32], &unhex::<32>(BOB_PK_HEX).unwrap());
    }

    #[test]
    fn recipient_unwraps_the_original_fmk() {
        let alice = DeviceIdentity::from_secret_bytes(&unhex(ALICE_SK_HEX).unwrap());
        let fmk = generate_fmk();
        let folder_id = [0x42u8; 16];
        let wrapped = wrap_folder_key(&fmk, &folder_id, alice.public()).unwrap();
        let got = unwrap_folder_key(&wrapped, &folder_id, &alice).unwrap();
        assert_eq!(*got, fmk);
    }

    #[test]
    fn a_different_device_cannot_unwrap() {
        let alice = DeviceIdentity::from_secret_bytes(&unhex(ALICE_SK_HEX).unwrap());
        let mallory = DeviceIdentity::generate();
        let fmk = generate_fmk();
        let folder_id = [1u8; 16];
        let wrapped = wrap_folder_key(&fmk, &folder_id, alice.public()).unwrap();
        match unwrap_folder_key(&wrapped, &folder_id, &mallory) {
            Err(FolderKeyError::Auth) => {}
            other => panic!("expected auth failure, got {other:?}"),
        }
    }

    #[test]
    fn every_tampered_byte_region_fails_authentication() {
        let alice = DeviceIdentity::from_secret_bytes(&unhex(ALICE_SK_HEX).unwrap());
        let fmk = generate_fmk();
        let folder_id = [2u8; 16];
        let good = wrap_folder_key(&fmk, &folder_id, alice.public()).unwrap();

        
        
        for idx in [0usize, 40, 79] {
            let mut evil = good;
            evil[idx] ^= 0x01;
            assert!(
                matches!(
                    unwrap_folder_key(&evil, &folder_id, &alice),
                    Err(FolderKeyError::Auth)
                ),
                "tamper at {idx} must fail"
            );
        }
    }

    #[test]
    fn unwrapping_under_wrong_folder_id_fails() {
        let alice = DeviceIdentity::from_secret_bytes(&unhex(ALICE_SK_HEX).unwrap());
        let fmk = generate_fmk();
        let wrapped = wrap_folder_key(&fmk, &[9u8; 16], alice.public()).unwrap();
        assert!(matches!(
            unwrap_folder_key(&wrapped, &[8u8; 16], &alice),
            Err(FolderKeyError::Auth)
        ));
    }

    #[test]
    fn deterministic_rng_pins_full_envelope_bytes() {
        
        
        
        
        let alice = DeviceIdentity::from_secret_bytes(&unhex(ALICE_SK_HEX).unwrap());
        let fmk: Fmk = core::array::from_fn(|i| i as u8 + 1);
        let folder_id: [u8; 16] = core::array::from_fn(|i| i as u8);

        let wrapped =
            wrap_folder_key_with_rng(&fmk, &folder_id, alice.public(), FixedRng::new(BOB_SK_HEX))
                .unwrap();
        assert_eq!(wrapped.len(), 80);
        
        
        
        assert_eq!(
            ferry_store::format::hex(&wrapped),
            "de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f\
             f59b0b9d840ca51536831c1af980f10ca51ac5030c2a56bab74061b5f68749\
             e835e64c6ec363b6ff0f670500b7cb59be",
            "envelope bytes drifted from the pinned vector"
        );
        
        assert_eq!(
            *unwrap_folder_key(&wrapped, &folder_id, &alice).unwrap(),
            fmk
        );
    }
}
