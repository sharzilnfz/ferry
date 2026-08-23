//! Passphrase-wrapped export/import: the ONE disaster-recovery door, per
//! ADR-0002's consequence list ("key backup becomes a UX problem"). There is
//! deliberately no other: losing all devices AND the export loses the data,
//! loudly and permanently.
//!
//! # Envelope (v1)
//!
//! ```text
//! magic    = "FRRX" (46 52 52 58)
//! version  = 0x01
//! salt     = 16 CSPRNG bytes (Argon2id salt)
//! nonce    = 12 CSPRNG bytes (ChaCha20-Poly1305, NOT the zero nonce used
//!            for device-pub wraps — passphrase keys are low entropy by
//!            human nature, so nonces must never repeat under one KDF output)
//! ct+tag   = ChaCha20-Poly1305_seal(wrap_key, nonce, aad = PURPOSE, pt)
//! wrap_key = Argon2id(passphrase, salt, m = 19456 KiB, t = 2, p = 1, L = 32)
//! ```
//!
//! Plaintext payload is `fmk[32] || device_secret[32]` (64 bytes), so one
//! export restores both folder access AND the device identity on a wiped
//! machine. `aad = "ferry/v1/recovery/export"` binds the purpose: an export
//! blob cannot be replayed as some other protocol object.
//!
//! Argon2id parameters follow the OWASP Password Storage Cheat Sheet's
//! second recommended configuration (m=19 MiB, t=2, p=1) — a deliberate
//! middle ground: strong against offline guessing at interactive latencies
//! on laptops, while staying usable on ARM boards that Ferry targets. The
//! parameters are FIXED by v1 (not stored in the envelope); changing them is
//! a format-version bump.
//!
//! Import refuses to clobber an existing identity file: overwriting live
//! trust roots silently would fork trust exactly like silent regeneration
//! ([`crate::identity`]).

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Key, Nonce,
};
use rand::RngCore;
use thiserror::Error;
use zeroize::Zeroizing;

/// Magic + version for recovery exports.
pub const EXPORT_MAGIC: [u8; 4] = *b"FRRX";
pub const EXPORT_VERSION: u8 = 1;
/// Argon2id salt length.
pub const SALT_LEN: usize = 16;
/// AEAD nonce length.
pub const NONCE_LEN: usize = 12;
/// Total serialized export size.
pub const EXPORT_LEN: usize = 4 + 1 + SALT_LEN + NONCE_LEN + 16 /* tag */ + 64;

const PURPOSE: &[u8] = b"ferry/v1/recovery/export";
/// Fixed v1 KDF cost: OWASP-recommended m=19456 KiB, t=2, p=1.
fn kdf() -> Argon2<'static> {
    // 19456 KiB, 2 iterations, parallelism 1; expect 32-byte output.
    let params = Params::new(19_456, 2, 1, Some(32)).expect("fixed valid params");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

#[derive(Debug, Error)]
pub enum RecoveryError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("bad magic bytes")]
    BadMagic,
    #[error("unsupported export format_version {0}")]
    BadVersion(u8),
    #[error("truncated export: need {need} bytes, have {have}")]
    Truncated { need: usize, have: usize },
    #[error("passphrase incorrect or export corrupted (authentication failed)")]
    AuthFailed,
    #[error("refusing to import: an identity already exists at {}; delete it deliberately first", .0.display())]
    IdentityExists(std::path::PathBuf),
}

/// Everything needed to resurrect a device after total loss.
pub struct RecoveryExport<'a> {
    pub fmk: &'a [u8; 32],
    pub device_secret: &'a [u8; 32],
}

impl<'a> RecoveryExport<'a> {
    /// Serialize + encrypt under `passphrase`. Fresh random salt/nonce from
    /// OS randomness on every call; two exports of identical state differ in
    /// ciphertext (no codebook to mine).
    pub fn seal(&self, passphrase: &str) -> Vec<u8> {
        let mut salt = [0u8; SALT_LEN];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        let mut nonce = [0u8; NONCE_LEN];
        rand::rngs::OsRng.fill_bytes(&mut nonce);

        let mut pt = Zeroizing::new([0u8; 64]);
        pt[..32].copy_from_slice(self.fmk);
        pt[32..].copy_from_slice(self.device_secret);

        let wrap_key = derive_wrap_key(passphrase, &salt);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(wrap_key.as_ref()));
        let ct = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload { msg: pt.as_ref(), aad: PURPOSE },
            )
            .expect("AEAD encryption cannot fail for these inputs");

        let mut out = Vec::with_capacity(EXPORT_LEN);
        out.extend_from_slice(&EXPORT_MAGIC);
        out.push(EXPORT_VERSION);
        out.extend_from_slice(&salt);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        debug_assert_eq!(out.len(), EXPORT_LEN);
        out
    }

    /// Decrypt and validate an export under `passphrase`.
    ///
    /// Wrong passphrase, bitrot, and truncation all collapse into
    /// [`RecoveryError::AuthFailed`] or shape errors — never garbage success.
    pub fn open(bytes: &[u8], passphrase: &str) -> Result<(Zeroizing<[u8; 32]>, Zeroizing<[u8; 32]>), RecoveryError> {
        if bytes.len() != EXPORT_LEN {
            return Err(RecoveryError::Truncated {
                need: EXPORT_LEN,
                have: bytes.len(),
            });
        }
        if bytes[..4] != EXPORT_MAGIC {
            return Err(RecoveryError::BadMagic);
        }
        if bytes[4] != EXPORT_VERSION {
            return Err(RecoveryError::BadVersion(bytes[4]));
        }
        let salt: [u8; SALT_LEN] = bytes[5..21].try_into().expect("salt");
        let nonce: [u8; NONCE_LEN] = bytes[21..33].try_into().expect("nonce");
        let ct = &bytes[33..];

        let wrap_key = derive_wrap_key(passphrase, &salt);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(wrap_key.as_ref()));
        let pt = cipher
            .decrypt(Nonce::from_slice(&nonce), Payload { msg: ct, aad: PURPOSE })
            .map_err(|_| RecoveryError::AuthFailed)?;
        let mut fmk: [u8; 32] = [0; 32];
        fmk.copy_from_slice(&pt[..32]);
        let mut sk: [u8; 32] = [0; 32];
        sk.copy_from_slice(&pt[32..]);
        Ok((Zeroizing::new(fmk), Zeroizing::new(sk)))
    }

    /// Full wipe-and-restore drill: write the export image to `dest`, then
    /// import it back into a FRESH identity directory, reconstructing both
    /// the device secret file and returning the FMK. Refuses if `identity_dir`
    /// already holds an identity (callers wiping state should have removed it).
    pub fn round_trip_through_files(
        &self,
        passphrase: &str,
        dest: &std::path::Path,
        identity_dir: &std::path::Path,
    ) -> Result<Zeroizing<[u8; 32]>, RecoveryError> {
        std::fs::write(dest, self.seal(passphrase))?;
        let bytes = std::fs::read(dest)?;
        let (fmk, sk) = Self::open(&bytes, passphrase)?;
        crate::identity::import_identity(identity_dir, &sk)
            .map_err(io_err)?;
        Ok(fmk)
    }
}

fn io_err(e: crate::identity::IdentityError) -> RecoveryError {
    match e {
        crate::identity::IdentityError::Io(io) => RecoveryError::Io(io),
        other => RecoveryError::Io(std::io::Error::other(other.to_string())),
    }
}

fn derive_wrap_key(passphrase: &str, salt: &[u8; SALT_LEN]) -> Zeroizing<[u8; 32]> {
    let mut okm = Zeroizing::new([0u8; 32]);
    kdf()
        .hash_password_into(passphrase.as_bytes(), salt, okm.as_mut())
        .expect("argon2 with fixed valid params");
    okm
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{load_or_create, DeviceIdentity, IdentityError};
    use ferry_store::format::unhex;

    const ALICE_SK_HEX: &str =
        "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a";

    fn fixture() -> RecoveryExport<'static> {
        // Leak-free test fixture via Box::leak keeps lifetimes simple; the
        // bytes are fixed RFC vectors, not real secrets.
        let fmk: &'static [u8; 32] = Box::leak(Box::new(core::array::from_fn(|i| i as u8 + 1)));
        let sk: &'static [u8; 32] = Box::leak(Box::new(unhex(ALICE_SK_HEX).unwrap()));
        RecoveryExport { fmk, device_secret: sk }
    }

    #[test]
    fn seal_open_round_trip_restores_exact_material() {
        let exp = fixture();
        let sealed = exp.seal("correct horse battery staple");
        assert_eq!(sealed.len(), EXPORT_LEN);
        // 4 magic + 1 version + 16 salt + 12 nonce + 64 plaintext + 16 tag.
        assert_eq!(sealed.len(), 113);
        assert_eq!(&sealed[..4], b"FRRX");
        assert_eq!(sealed[4], 1);
        let (fmk, sk) = RecoveryExport::open(&sealed, "correct horse battery staple").unwrap();
        assert_eq!(*fmk, *exp.fmk);
        assert_eq!(*sk, *exp.device_secret);
    }

    #[test]
    fn fresh_salt_and_nonce_make_exports_nondeterministic() {
        let exp = fixture();
        let a = exp.seal("same passphrase");
        let b = exp.seal("same passphrase");
        assert_ne!(a, b, "exports must not be reproducible byte-for-byte");
        // Salt occupies bytes 5..21: differing across calls.
        assert_ne!(&a[5..21], &b[5..21]);
    }

    #[test]
    fn wrong_passphrase_fails_cleanly() {
        let exp = fixture();
        let sealed = exp.seal("right");
        match RecoveryExport::open(&sealed, "wrong") {
            Err(RecoveryError::AuthFailed) => {}
            other => panic!("expected AuthFailed, got {other:?}"),
        }
        // Near-miss passphrases too.
        for probe in ["Right", "righ", "right ", "", "richt"] {
            assert!(matches!(
                RecoveryExport::open(&sealed, probe),
                Err(RecoveryError::AuthFailed)
            ));
        }
    }

    #[test]
    fn corrupted_export_fails_loudly_everywhere() {
        let exp = fixture();
        let good = exp.seal("p");

        // Flip one bit at each structural region: magic, version, salt,
        // nonce, ciphertext, tag.
        for idx in [0usize, 4, 10, 25, 40, 80] {
            let mut evil = good.clone();
            evil[idx] ^= 0x01;
            let err = RecoveryExport::open(&evil, "p").unwrap_err();
            assert!(
                matches!(
                    err,
                    RecoveryError::AuthFailed | RecoveryError::BadMagic | RecoveryError::BadVersion(_)
                ),
                "flip at {idx} gave {err:?}"
            );
        }
        // Truncation and padding are shape errors, never decrypt attempts.
        assert!(matches!(
            RecoveryExport::open(&good[..good.len() - 1], "p"),
            Err(RecoveryError::Truncated { .. })
        ));
        let mut padded = good.clone();
        padded.push(0);
        assert!(matches!(
            RecoveryExport::open(&padded, "p"),
            Err(RecoveryError::Truncated { .. })
        ));
    }

    #[test]
    fn exported_key_restores_access_on_a_wiped_device() {
        // The acceptance scenario end-to-end, through real files:
        let dir = tempfile::tempdir().unwrap();
        let export_path = dir.path().join("ferry-backup.ferryexport");
        let original_dir = dir.path().join("identity-original");
        let wiped_dir = dir.path().join("identity-wiped");
        let _original = load_or_create(&original_dir).unwrap();

        let fmk: [u8; 32] = core::array::from_fn(|i| i as u8 + 7);
        let sk: [u8; 32] = unhex(ALICE_SK_HEX).unwrap();
        let exp = RecoveryExport { fmk: &fmk, device_secret: &sk };

        let restored = exp
            .round_trip_through_files("hunter2 but longer", &export_path, &wiped_dir)
            .unwrap();
        assert_eq!(*restored, fmk);

        // The wiped device now HAS its old identity back: loading it yields
        // the same public key the export carried.
        let reborn = load_or_create(&wiped_dir).unwrap();
        assert_eq!(*reborn.public(), *DeviceIdentity::from_secret_bytes(&sk).public());

        // And it can unwrap envelopes addressed to that identity.
        let folder_id = [2u8; 16];
        let wrapped = crate::folder_key::wrap_folder_key(&fmk, &folder_id, reborn.public()).unwrap();
        assert_eq!(
            *crate::folder_key::unwrap_folder_key(&wrapped, &folder_id, &reborn).unwrap(),
            fmk
        );
        let _ = _original;
    }

    #[test]
    fn import_refuses_to_clobber_existing_identity() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("identity");
        load_or_create(&existing).unwrap(); // creates identity

        let sk = unhex(ALICE_SK_HEX).unwrap();
        let err = crate::identity::import_identity(&existing, &sk).unwrap_err();
        assert!(matches!(err, IdentityError::Io(_)), "{err}");
        // The pre-existing identity survived untouched.
        let kept = load_or_create(&existing).unwrap();
        assert_ne!(*kept.public(), *DeviceIdentity::from_secret_bytes(&sk).public());
    }
}
