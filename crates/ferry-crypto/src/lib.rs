//! Ferry device identity, pairing, and key wrapping (ticket T-007).
//!
//! The byte-level contracts this crate implements live in
//! `docs/store-format.md` (key-wrap envelope, `CONFIG_HEAD`, pack cipher) and
//! ADR-0002 (E2E by default, explicit pairing, no recovery back door). This
//! crate owns the parts the format spec deliberately leaves to T-007: the
//! pairing handshake, QR payloads, identity persistence, and passphrase-
//! wrapped recovery exports — plus the raw primitives (base32 alphabet,
//! CRC-32) behind the short codes minted by the user-facing ritual in
//! `ferry-folder::pairing`.
//!
//! # Module map
//!
//! - [`base32`]: canonical human alphabet (no `0/O/1/I`) for short codes.
//! - [`crc32`]: IEEE CRC-32 used for short-code checksum characters.
//! - [`identity`]: long-lived X25519 device keypair, load-or-create with a
//!   loud error on corruption.
//! - [`folder_key`]: FMK generation and the normative X25519 wrap envelope
//!   (`wrapped_len == 80`).
//! - [`config_head`]: `CONFIG_HEAD` container serialization, byte-for-byte per
//!   the store format spec.
//! - [`pack_cipher`]: the real ChaCha20-Poly1305 [`PackCipher`] that replaces
//!   ferry-store's pass-through stub at the seam (T-008 does the swap).
//! - [`pairing`]: offer/response payloads, QR content, and the
//!   HMAC-confirmed handshake.
//! - [`recovery`]: Argon2id + ChaCha20-Poly1305 passphrase export/import.
//!
//! # QR content layout (normative for v1)
//!
//! The QR image encodes exactly the serialized pairing-offer bytes; there is
//! no separate framing layer. Byte layout (all integers little-endian):
//!
//! ```text
//! offset size field
//! 0      4    magic "FRPO" (46 52 50 4f)
//! 4      1    version = 1
//! 5      16   folder_id (UUIDv4 raw bytes)
//! 21     32   initiator device X25519 public key (raw)
//! 53     32   one-time pairing secret (CSPRNG)
//! 61     8    created_sec (i64 LE)
//! total  93 bytes
//! ```
//!
//! A scanner camera resolves these bytes; everything downstream (short-code
//! confirmation, response MAC) derives from them. See [`pairing`].
//!
//! # Threat notes
//!
//! - Secret key material ([`identity::DeviceSecret`], FMKs, one-time secrets)
//!   is zeroized on drop; nothing secret appears in `Debug` output.
//! - Identity files are mode 0600 (directories 0700); corruption is an error,
//!   never silent regeneration, because silent new keys silently fork trust.
//! - The one-time pairing secret travels inside the QR payload itself: the QR
//!   exchange is the out-of-band channel, so possession of the scanned bytes
//!   IS the authorization to pair. Intercepting network traffic (T-008)
//!   without the QR gains nothing: pre-completion traffic carries no wrapped
//!   keys, and post-completion wraps authenticate under X25519+HKDF keyed by
//!   each device's static secret.
//! - Losing all devices loses all data. The only escape hatch is a
//!   passphrase-wrapped export ([`recovery`]); there is no server-side reset.

pub mod base32;
pub mod config_head;
pub mod crc32;
pub mod folder_key;
pub mod identity;
pub mod pack_cipher;
pub mod pairing;
pub mod recovery;

/// First 8 bytes of `data` as lowercase hex, for non-secret display in
/// Debug impls and logs.
pub(crate) fn hex_short(data: &[u8]) -> String {
    ferry_store::format::hex(&data[..8.min(data.len())])
}

/// Shared test fixtures. Compiled only for tests within this crate.
#[cfg(test)]
pub(crate) mod testing {
    use rand::{CryptoRng, RngCore};

    /// Deterministic CSPRNG stand-in yielding a repeating 32-byte pattern,
    /// letting tests pin vectors that would otherwise be random.
    pub(crate) struct FixedRng {
        pattern: [u8; 32],
        pos: usize,
    }

    impl FixedRng {
        pub(crate) fn new(hex_pattern: &str) -> Self {
            FixedRng {
                pattern: ferry_store::format::unhex(hex_pattern).expect("valid hex"),
                pos: 0,
            }
        }
    }

    impl RngCore for FixedRng {
        fn next_u32(&mut self) -> u32 {
            let mut b = [0u8; 4];
            self.fill_bytes(&mut b);
            u32::from_le_bytes(b)
        }
        fn next_u64(&mut self) -> u64 {
            (u64::from(self.next_u32()) << 32) | u64::from(self.next_u32())
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            for b in dest {
                *b = self.pattern[self.pos % 32];
                self.pos += 1;
            }
        }
        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }
    impl CryptoRng for FixedRng {}
}
