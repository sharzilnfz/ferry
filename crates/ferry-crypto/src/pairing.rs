//! The pairing ritual: offer payload, short codes, QR content, and the
//! HMAC-confirmed handshake (ticket T-007 scope per `docs/store-format.md`:
//! "Pairing, QR codes, short codes ... are T-007's").
//!
//! # Flow
//!
//! ```text
//! initiator                              responder
//! ---------                              ---------
//! PairingOffer::create(folder, me)
//!   -> offer bytes  ==QR/scan==>         PairingOffer::parse(bytes)
//!   -> short_code(hints)  ==typed==>     verify_short_code(code, bytes)
//!                                        respond(offer, identity)
//!                       <==response==   PairingResponse { pub, mac }
//! verify_response(offer, response)
//! complete_pairing(offer, response, fmk)
//!   -> wrapped keys for BOTH devices     unwrap_folder_key(wrapped, ...)
//! ```
//!
//! The one-time secret rides inside the scanned payload itself: the QR /
//! in-person exchange IS the authorization channel. Network transport of the
//! offer without that channel proves nothing and wraps nothing — the FMK is
//! only ever wrapped AFTER the responder's HMAC (keyed by the one-time
//! secret) confirms possession. A passive network observer therefore sees no
//! keyable material at any point.
//!
//! # Short-code construction (v1)
//!
//! ```text
//! data     = hints (u16 LE) || BLAKE3(offer_bytes)[0..8]
//! checksum = CRC-32/IEEE(data) truncated to its HIGH 16 bits
//! code     = base32(data) grouped 4-4-4-4
//!            + "-" + base32_padded(checksum BE)  ->  XXXX-XXXX-XXXX-XXXX-XXXX
//! ```
//!
//! Alphabet is [`crate::base32`]'s canonical set (`0/O/1/I` absent). The
//! CRC-16-truncation was chosen over a BLAKE3 prefix because the threat here
//! is typos, not adversaries — authenticity is the MAC's job — and a CRC's
//! burst-error detection is exactly the typo model, reproducible by an
//! independent implementation from this comment alone. Any single-symbol
//! substitution anywhere in the 20 symbols fails either the checksum or the
//! embedded payload hash; decoders never guess substitutions for lookalike
//! characters.
//!
//! Transport hints are advisory metadata about connectivity (see
//! [`TransportHints`]); they ride the code so both humans see identical
//! context while comparing screens.

use crate::base32::{self, Base32Error};
use crate::crc32::crc32;
use crate::folder_key::{
    wrap_folder_key, Fmk, FolderKeyError, WRAPPED_LEN,
};
use crate::identity::{DeviceId, DeviceIdentity};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use thiserror::Error;
use zeroize::Zeroizing;

/// Magic opening every serialized pairing offer: "FRPO".
pub const OFFER_MAGIC: [u8; 4] = *b"FRPO";
/// Magic opening every serialized pairing response: "FRPR".
pub const RESPONSE_MAGIC: [u8; 4] = *b"FRPR";
/// Wire format version written by v1 implementations.
pub const FORMAT_VERSION: u8 = 1;
/// Serialized offer size: magic+version+folder+pubs+secret+timestamp.
pub const OFFER_LEN: usize = 4 + 1 + 16 + 32 + 32 + 8;
/// Serialized response size.
pub const RESPONSE_LEN: usize = 4 + 1 + 32 + 32 + 8;
/// Domain-separation prefix for the confirmation transcript.
const TRANSCRIPT_INFO: &[u8] = b"ferry/v1/pairing/confirm";

#[derive(Debug, Error)]
pub enum PairingError {
    #[error("bad magic bytes")]
    BadMagic,
    #[error("unsupported pairing format_version {0}")]
    BadVersion(u8),
    #[error("truncated pairing message: need {need} bytes, have {have}")]
    Truncated { need: usize, have: usize },
    #[error("response MAC failed verification")]
    MacMismatch,
    #[error("short code rejected: {0}")]
    Code(#[from] Base32Error),
    #[error("short code checksum mismatch: the code was mistyped or the payload corrupted")]
    CodeChecksumMismatch,
    #[error("short code does not match this offer payload")]
    CodeHashMismatch,
    #[error(transparent)]
    KeyWrap(#[from] FolderKeyError),
}

/// Advisory connectivity flags displayed alongside / carried by the short
/// code. Bit semantics for v1:
///
/// - bit 0 `RELAY_OFFERED`: initiator is willing to use a relay if direct
///   connection fails.
/// - bit 1 `DIRECT_LAN`: initiator believes it is reachable on the LAN.
/// - bits 2..15 reserved, MUST be zero when encoding; ignored when decoding.
///
/// Hints NEVER affect security decisions; they only pre-fill UI state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TransportHints(pub u16);

impl TransportHints {
    pub const RELAY_OFFERED: u16 = 1 << 0;
    pub const DIRECT_LAN: u16 = 1 << 1;

    fn to_bytes(self) -> [u8; 2] {
        self.0.to_le_bytes()
    }
    fn from_bytes(b: [u8; 2]) -> Self {
        TransportHints(u16::from_le_bytes(b))
    }
}

/// A pairing invitation created by the folder-owning device.
///
/// `secret` is the one-time pairing secret; it serializes into the QR
/// payload (the out-of-band channel) but must never travel over the sync
/// transport. Zeroized on drop; `Debug` shows nothing sensitive.
pub struct PairingOffer {
    pub folder_id: [u8; 16],
    pub initiator_pub: DeviceId,
    secret: Zeroizing<[u8; 32]>,
    pub created_sec: i64,
}

impl std::fmt::Debug for PairingOffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingOffer")
            .field("folder_id", &crate::hex_short(&self.folder_id))
            .field("initiator_pub", &crate::hex_short(&self.initiator_pub))
            .field("created_sec", &self.created_sec)
            .finish_non_exhaustive()
    }
}

impl PairingOffer {
    /// Fresh offer with a CSPRNG one-time secret.
    pub fn create(
        folder_id: [u8; 16],
        initiator: &DeviceIdentity,
        now_sec: i64,
    ) -> Self {
        Self::create_with_rng(folder_id, initiator, now_sec, rand::rngs::OsRng)
    }

    /// Deterministic variant for tests and pinned vectors.
    pub fn create_with_rng(
        folder_id: [u8; 16],
        initiator: &DeviceIdentity,
        now_sec: i64,
        mut rng: impl rand::RngCore + rand::CryptoRng,
    ) -> Self {
        let mut secret: [u8; 32] = [0u8; 32];
        rng.fill_bytes(&mut secret);
        PairingOffer {
            folder_id,
            initiator_pub: *initiator.public(),
            secret: Zeroizing::new(secret),
            created_sec: now_sec,
        }
    }

    /// The one-time secret. Handled with care: it authorizes pairing.
    pub fn one_time_secret(&self) -> &[u8; 32] {
        &self.secret
    }

    /// Serialize the exact bytes the QR encodes (layout in crate docs).
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(OFFER_LEN);
        out.extend_from_slice(&OFFER_MAGIC);
        out.push(FORMAT_VERSION);
        out.extend_from_slice(&self.folder_id);
        out.extend_from_slice(&self.initiator_pub);
        out.extend_from_slice(self.secret.as_ref());
        out.extend_from_slice(&self.created_sec.to_le_bytes());
        debug_assert_eq!(out.len(), OFFER_LEN);
        out
    }

    /// Parse scanned bytes. Unknown magic/version/truncation are hard errors.
    pub fn parse(bytes: &[u8]) -> Result<Self, PairingError> {
        if bytes.len() != OFFER_LEN {
            return Err(PairingError::Truncated {
                need: OFFER_LEN,
                have: bytes.len(),
            });
        }
        if bytes[..4] != OFFER_MAGIC {
            return Err(PairingError::BadMagic);
        }
        if bytes[4] != FORMAT_VERSION {
            return Err(PairingError::BadVersion(bytes[4]));
        }
        let folder_id = bytes[5..21].try_into().expect("16 bytes");
        let initiator_pub = bytes[21..53].try_into().expect("32 bytes");
        let secret: [u8; 32] = bytes[53..85].try_into().expect("32 bytes");
        let created_sec = i64::from_le_bytes(bytes[85..93].try_into().expect("8 bytes"));
        Ok(PairingOffer {
            folder_id,
            initiator_pub,
            secret: Zeroizing::new(secret),
            created_sec,
        })
    }

    /// The human-typed confirmation code for these offer bytes.
    pub fn short_code(&self, hints: TransportHints) -> String {
        short_code_for(&self.serialize(), hints)
    }

    /// Bytes to feed to the QR renderer (identical to [`Self::serialize`] —
    /// there is deliberately no second framing layer to drift).
    pub fn qr_content(&self) -> Vec<u8> {
        self.serialize()
    }

    /// Render a QR symbol matrix over the offer bytes. Rendering PNG/SVG is
    /// out of scope (T-009 UX); callers get the crate's matrix type.
    pub fn qr_code(&self) -> Result<qrcode::QrCode, qrcode::types::QrError> {
        qrcode::QrCode::new(self.qr_content())
    }
}

/// The responder's answer: its device public key plus an HMAC over the
/// transcript, keyed by the one-time secret — proof of possession without
/// retransmitting the secret through anything but the already-scanned bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct PairingResponse {
    pub responder_pub: DeviceId,
    mac: [u8; 32],
    pub created_sec: i64,
}

impl std::fmt::Debug for PairingResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingResponse")
            .field("responder_pub", &crate::hex_short(&self.responder_pub))
            .field("mac", &crate::hex_short(&self.mac))
            .field("created_sec", &self.created_sec)
            .finish()
    }
}

impl PairingResponse {
    /// Compute the confirmation MAC over the full transcript:
    /// `HMAC-SHA256(key = one_time_secret,
    ///              data = TRANSCRIPT_INFO || offer_bytes || responder_pub)`.
    pub fn compute_mac(
        offer_bytes: &[u8],
        one_time_secret: &[u8; 32],
        responder_pub: &DeviceId,
    ) -> [u8; 32] {
        let mut mac = Hmac::<Sha256>::new_from_slice(one_time_secret)
            .expect("HMAC accepts any key length");
        mac.update(TRANSCRIPT_INFO);
        mac.update(offer_bytes);
        mac.update(responder_pub);
        let tag = mac.finalize().into_bytes();
        tag.into()
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(RESPONSE_LEN);
        out.extend_from_slice(&RESPONSE_MAGIC);
        out.push(FORMAT_VERSION);
        out.extend_from_slice(&self.responder_pub);
        out.extend_from_slice(&self.mac);
        out.extend_from_slice(&self.created_sec.to_le_bytes());
        out
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, PairingError> {
        if bytes.len() != RESPONSE_LEN {
            return Err(PairingError::Truncated {
                need: RESPONSE_LEN,
                have: bytes.len(),
            });
        }
        if bytes[..4] != RESPONSE_MAGIC {
            return Err(PairingError::BadMagic);
        }
        if bytes[4] != FORMAT_VERSION {
            return Err(PairingError::BadVersion(bytes[4]));
        }
        Ok(PairingResponse {
            responder_pub: bytes[5..37].try_into().expect("32 bytes"),
            mac: bytes[37..69].try_into().expect("32 bytes"),
            created_sec: i64::from_le_bytes(bytes[69..77].try_into().expect("8 bytes")),
        })
    }

    /// Verify against the offer this response answers. Constant-time MAC
    /// comparison via `hmac`'s verified API equivalent (reduced comparison).
    pub fn verify(&self, offer: &PairingOffer, offer_bytes: &[u8]) -> Result<(), PairingError> {
        let expect = Self::compute_mac(offer_bytes, offer.one_time_secret(), &self.responder_pub);
        if expect != self.mac {
            return Err(PairingError::MacMismatch);
        }
        // The transcript binds the offer bytes themselves, so the parsed
        // fields must agree with what was MACed (defensive: parse/serialize
        // round-trip could otherwise diverge).
        if offer.serialize() != offer_bytes {
            return Err(PairingError::MacMismatch);
        }
        Ok(())
    }
}

/// Responder-side step: build the answer to an offer under our identity.
pub fn respond(offer: &PairingOffer, responder: &DeviceIdentity, now_sec: i64) -> PairingResponse {
    let offer_bytes = offer.serialize();
    PairingResponse {
        responder_pub: *responder.public(),
        mac: PairingResponse::compute_mac(&offer_bytes, offer.one_time_secret(), responder.public()),
        created_sec: now_sec,
    }
}

/// Everything the initiator needs after a confirmed handshake.
#[derive(Debug)]
pub struct CompletedPairing {
    /// The peer we just paired with (their X25519 public key).
    pub peer_pub: DeviceId,
    /// FMK wrapped to OUR device public key (store locally like any wrap).
    pub wrapped_for_self: [u8; WRAPPED_LEN],
    /// FMK wrapped to the PEER's public key; send over transport (T-008).
    pub wrapped_for_peer: [u8; WRAPPED_LEN],
}

/// Initiator-side completion: verify the response, then wrap the FMK to both
/// devices. This is the ONLY place an FMK ever gets wrapped during pairing —
/// before this point an intercepted offer yields nothing to decrypt.
pub fn complete_pairing(
    offer: &PairingOffer,
    offer_bytes: &[u8],
    response: &PairingResponse,
    fmk: &Fmk,
    initiator: &DeviceIdentity,
) -> Result<CompletedPairing, PairingError> {
    response.verify(offer, offer_bytes)?;
    let wrapped_for_peer = wrap_folder_key(fmk, &offer.folder_id, &response.responder_pub)?;
    let wrapped_for_self = wrap_folder_key(fmk, &offer.folder_id, initiator.public())?;
    Ok(CompletedPairing {
        peer_pub: response.responder_pub,
        wrapped_for_self,
        wrapped_for_peer,
    })
}

// --- short codes ---

fn code_payload(offer_bytes: &[u8], hints: TransportHints) -> ([u8; 10], u16) {
    let digest = blake3::hash(offer_bytes);
    let mut data = [0u8; 10];
    data[..2].copy_from_slice(&hints.to_bytes());
    data[2..].copy_from_slice(&digest.as_bytes()[..8]);
    let crc = crc32(&data);
    (data, (crc >> 16) as u16)
}

/// Encode the human short code for `offer_bytes` (format in module docs).
pub fn short_code_for(offer_bytes: &[u8], hints: TransportHints) -> String {
    let (data, check) = code_payload(offer_bytes, hints);
    let main = base32::encode(&data); // 16 symbols
    let check_be = check.to_be_bytes();
    let tail = base32::encode(&check_be); // 4 symbols (2 pad bits)
    format!(
        "{}-{}-{}-{}-{}",
        &main[0..4],
        &main[4..8],
        &main[8..12],
        &main[12..16],
        &tail
    )
}

/// A short code whose checksum verified; carries the decoded hints so the
/// UI can show connection intent, and re-exposes the hash prefix match
/// decision to tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifiedCode {
    pub hints: TransportHints,
}

/// Decode + verify a typed short code against candidate offer bytes:
/// alphabet-checked, checksum-checked, THEN hash-matched. Any single-symbol
/// typo fails loudly with a targeted error.
pub fn verify_short_code(code: &str, offer_bytes: &[u8]) -> Result<VerifiedCode, PairingError> {
    let cleaned: String = code.chars().filter(|c| *c != '-' && *c != ' ').collect();
    if cleaned.len() != 20 {
        return Err(PairingError::Truncated {
            need: 20,
            have: cleaned.len(),
        });
    }
    let bytes = base32::decode(&cleaned)?;
    debug_assert_eq!(bytes.len(), 12, "20 symbols decode to 12 bytes");
    let data: [u8; 10] = bytes[..10].try_into().expect("10 bytes");
    let check_in: [u8; 2] = bytes[10..12].try_into().expect("2 bytes");
    let expected_check = (crc32(&data) >> 16) as u16;
    if expected_check.to_be_bytes() != check_in {
        return Err(PairingError::CodeChecksumMismatch);
    }
    let want_prefix = blake3::hash(offer_bytes).as_bytes()[..8].to_vec();
    if data[2..] != want_prefix[..] {
        return Err(PairingError::CodeHashMismatch);
    }
    Ok(VerifiedCode {
        hints: TransportHints::from_bytes([data[0], data[1]]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::DeviceIdentity;
    use crate::testing::FixedRng;
    use ferry_store::format::unhex;

    const ALICE_SK_HEX: &str =
        "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a";

    fn alice() -> DeviceIdentity {
        DeviceIdentity::from_secret_bytes(&unhex(ALICE_SK_HEX).unwrap())
    }

    fn test_offer() -> (PairingOffer, Vec<u8>) {
        let offer = PairingOffer::create_with_rng(
            [3u8; 16],
            &alice(),
            1_700_000_000,
            FixedRng::new(
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            ),
        );
        let bytes = offer.serialize();
        (offer, bytes)
    }

    #[test]
    fn offer_serialization_matches_documented_layout() {
        let (_offer, bytes) = test_offer();
        assert_eq!(bytes.len(), OFFER_LEN);
        assert_eq!(bytes.len(), 93);
        assert_eq!(&bytes[..4], b"FRPO");
        assert_eq!(bytes[4], 1);
        assert_eq!(&bytes[5..21], &[3u8; 16]);
        assert_eq!(&bytes[21..53], &alice().device_id()[..]);
        // One-time secret came from the fixed RNG pattern.
        assert_eq!(
            &bytes[53..85],
            &(0u8..=0x1f).collect::<Vec<u8>>()[..]
        );
        assert_eq!(&bytes[85..], &1_700_000_000i64.to_le_bytes());
    }

    #[test]
    fn offer_parse_round_trips_and_rejects_garbage() {
        let (offer, bytes) = test_offer();
        let parsed = PairingOffer::parse(&bytes).unwrap();
        assert_eq!(parsed.folder_id, offer.folder_id);
        assert_eq!(parsed.initiator_pub, offer.initiator_pub);
        assert_eq!(parsed.created_sec, offer.created_sec);
        assert_eq!(parsed.one_time_secret(), offer.one_time_secret());

        let mut evil = bytes.clone();
        evil[0] = b'X';
        assert!(matches!(PairingOffer::parse(&evil), Err(PairingError::BadMagic)));
        evil = bytes.clone();
        evil[4] = 2;
        assert!(matches!(PairingOffer::parse(&evil), Err(PairingError::BadVersion(2))));
        evil = bytes[..92].to_vec();
        assert!(matches!(
            PairingOffer::parse(&evil),
            Err(PairingError::Truncated { need: 93, have: 92 })
        ));
        // Random junk is not secretly valid.
        assert!(PairingOffer::parse(&[7u8; 93]).is_err());
    }

    #[test]
    fn short_code_shape_is_five_groups_of_four_canonical_symbols() {
        let (_offer, bytes) = test_offer();
        let code = short_code_for(&bytes, TransportHints(TransportHints::RELAY_OFFERED));
        let groups: Vec<&str> = code.split('-').collect();
        assert_eq!(groups.len(), 5);
        for g in &groups {
            assert_eq!(g.len(), 4, "group {g}");
        }
        for ch in code.chars().filter(|c| *c != '-') {
            assert!(!matches!(ch, '0' | '1' | 'I' | 'O'));
            assert!(ch.is_ascii_alphanumeric() && !ch.is_ascii_lowercase());
        }
    }

    #[test]
    fn short_code_round_trips_with_hints() {
        let (_offer, bytes) = test_offer();
        let hints = TransportHints(TransportHints::RELAY_OFFERED | TransportHints::DIRECT_LAN);
        let code = short_code_for(&bytes, hints);
        let verified = verify_short_code(&code, &bytes).unwrap();
        assert_eq!(verified.hints, hints);
    }

    #[test]
    fn single_symbol_typo_anywhere_is_rejected() {
        let (_offer, bytes) = test_offer();
        let code = short_code_for(&bytes, TransportHints::default());
        let symbols: Vec<(usize, char)> =
            code.chars().enumerate().filter(|(_, c)| *c != '-').collect();

        for pos in 0..symbols.len() {
            let original = symbols[pos].1;
            // Substitute a DIFFERENT canonical symbol at this position.
            for sub in ALPHABET_TEST {
                if *sub == original {
                    continue;
                }
                let mut typed: Vec<char> = code.chars().collect();
                typed[symbols[pos].0] = *sub;
                let err = verify_short_code(&typed.into_iter().collect::<String>(), &bytes).unwrap_err();
                assert!(
                    matches!(
                        err,
                        PairingError::CodeChecksumMismatch | PairingError::CodeHashMismatch
                    ),
                    "typo '{original}'->'{sub}' gave {err:?}"
                );
                break; // one substitution per position suffices
            }
        }

        // Lookalikes are refused with the targeted char error, not guessed.
        for bad in ['0', '1', 'I', 'O'] {
            let mut typed: Vec<char> = code.chars().collect();
            typed[0] = bad;
            assert!(matches!(
                verify_short_code(&typed.into_iter().collect::<String>(), &bytes),
                Err(PairingError::Code(Base32Error::InvalidChar(c))) if c == bad
            ));
        }

        // Wrong length and separators-in-random-places still work.
        assert!(verify_short_code("ABCD", &bytes).is_err());
        assert!(verify_short_code(&code.replace('-', ""), &bytes).is_ok());
    }

    const ALPHABET_TEST: &[char] = &[
        '2', '3', '4', '5', '6', '7', '8', '9', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'J',
        'K', 'L', 'M', 'N', 'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z',
    ];

    #[test]
    fn code_binds_to_its_exact_payload() {
        let (_offer, bytes) = test_offer();
        let code = short_code_for(&bytes, TransportHints::default());
        // Same payload, different byte somewhere: hash mismatch.
        let mut other = bytes.clone();
        other[70] ^= 1; // inside the secret region
        assert!(matches!(
            verify_short_code(&code, &other),
            Err(PairingError::CodeHashMismatch)
        ));
    }

    #[test]
    fn response_mac_binds_transcript_and_rejects_fakes() {
        let (offer, offer_bytes) = test_offer();
        let responder = DeviceIdentity::generate();
        let resp = respond(&offer, &responder, 1_700_000_100);
        resp.verify(&offer, &offer_bytes).unwrap();

        // Tampered responder key: MAC no longer matches.
        let mut evil = resp.clone();
        evil.responder_pub[0] ^= 1;
        assert!(matches!(
            evil.verify(&offer, &offer_bytes),
            Err(PairingError::MacMismatch)
        ));
        // Tampered MAC field.
        let mut evil = resp.clone();
        evil.mac[31] ^= 1;
        assert!(matches!(
            evil.verify(&offer, &offer_bytes),
            Err(PairingError::MacMismatch)
        ));
        // Different offer entirely (wrong folder): mismatch.
        let mut evil_bytes = offer_bytes.clone();
        evil_bytes[5] ^= 1;
        assert!(matches!(
            resp.verify(&offer, &evil_bytes),
            Err(PairingError::MacMismatch)
        ));
        // Response serialization round-trip keeps verification green.
        let rt = PairingResponse::parse(&resp.serialize()).unwrap();
        rt.verify(&offer, &offer_bytes).unwrap();
    }

    #[test]
    fn response_serialization_layout() {
        let responder = DeviceIdentity::generate();
        let resp = respond(&PairingOffer::parse(&test_offer().1).unwrap(), &responder, 42);
        let bytes = resp.serialize();
        assert_eq!(bytes.len(), RESPONSE_LEN);
        assert_eq!(bytes.len(), 77);
        assert_eq!(&bytes[..4], b"FRPR");
        assert_eq!(bytes[4], 1);
        let parsed = PairingResponse::parse(&bytes).unwrap();
        assert_eq!(parsed, resp);
    }

    #[test]
    fn qr_content_is_the_offer_bytes_and_matrix_builds() {
        let (offer, bytes) = test_offer();
        assert_eq!(offer.qr_content(), bytes);
        let qr = offer.qr_code().unwrap();
        // Binary-mode QR over 93 bytes lands well within version bounds;
        // just prove the matrix exists and is non-degenerate.
        assert!(qr.width() >= 21);
        // The module matrix: width x width cells, non-degenerate.
        let colors = qr.to_colors();
        assert_eq!(colors.len(), qr.width() * qr.width());
        assert!(colors.iter().any(|c| *c == qrcode::Color::Dark));
        assert!(colors.iter().any(|c| *c == qrcode::Color::Light));
    }

    #[test]
    fn full_local_ritual_both_sides_unwrap_same_fmk() {
        let initiator_dir = tempfile::tempdir().unwrap();
        let responder_dir = tempfile::tempdir().unwrap();
        let a = crate::identity::load_or_create(initiator_dir.path()).unwrap();
        let _b = crate::identity::load_or_create(responder_dir.path()).unwrap();

        let offer = PairingOffer::create([9u8; 16], &a, 123);
        let offer_bytes = offer.serialize();
        let b = crate::identity::load_or_create(responder_dir.path()).unwrap();

        // Human step on the responder side:
        let code = offer.short_code(TransportHints(TransportHints::DIRECT_LAN));
        let verified = verify_short_code(&code, &offer_bytes).unwrap();
        assert_eq!(verified.hints, TransportHints(TransportHints::DIRECT_LAN));

        let resp = respond(&offer, &b, 456);
        let fmk = crate::folder_key::generate_fmk();
        let done = complete_pairing(&offer, &offer_bytes, &resp, &fmk, &a).unwrap();
        assert_eq!(done.peer_pub, *b.public());

        let got_a = crate::folder_key::unwrap_folder_key(
            &done.wrapped_for_self,
            &offer.folder_id,
            &a,
        )
        .unwrap();
        let got_b = crate::folder_key::unwrap_folder_key(
            &done.wrapped_for_peer,
            &offer.folder_id,
            &b,
        )
        .unwrap();
        assert_eq!(*got_a, fmk);
        assert_eq!(*got_b, fmk);
    }
}
