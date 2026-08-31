use crate::base32::{self, Base32Error};
use crate::crc32::crc32;
use crate::folder_key::{wrap_folder_key, Fmk, FolderKeyError, WRAPPED_LEN};
use crate::identity::{DeviceId, DeviceIdentity};
use chacha20poly1305::{
    aead::{Aead, Payload},
    ChaCha20Poly1305, Nonce,
};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use thiserror::Error;
use zeroize::Zeroizing;

pub const OFFER_MAGIC: [u8; 4] = *b"FRPO";

pub const RESPONSE_MAGIC: [u8; 4] = *b"FRPR";

pub const FORMAT_VERSION: u8 = 1;

pub const OFFER_LEN: usize = 4 + 1 + 16 + 32 + 32 + 8;

pub const RESPONSE_LEN: usize = 4 + 1 + 32 + 32 + 8;

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

#[derive(Debug, Error)]
pub enum GrantError {
    #[error("grant file is malformed: need at least {need} bytes, have {have}")]
    Malformed { need: usize, have: usize },
    #[error("grant failed authentication against this offer")]
    Auth,
    #[error("offer bytes truncated: need {need}, have {have}")]
    OfferTruncated { need: usize, have: usize },

    #[error("internal crypto failure (unreachable for these inputs)")]
    Internal,
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Reader { bytes, pos: 0 }
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N], PairingError> {
        let end = self.pos + N;
        let src = self
            .bytes
            .get(self.pos..end)
            .ok_or(PairingError::Truncated {
                need: end,
                have: self.bytes.len(),
            })?;
        self.pos = end;
        let mut out = [0u8; N];
        out.copy_from_slice(src);
        Ok(out)
    }
}

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
    pub fn create(folder_id: [u8; 16], initiator: &DeviceIdentity, now_sec: i64) -> Self {
        Self::create_with_rng(folder_id, initiator, now_sec, rand::rngs::OsRng)
    }

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

    pub fn one_time_secret(&self) -> &[u8; 32] {
        &self.secret
    }

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
        let mut r = Reader::new(bytes);
        let _magic = r.take::<4>()?;
        let _version = r.take::<1>()?;
        let folder_id = r.take::<16>()?;
        let initiator_pub = r.take::<32>()?;
        let secret = Zeroizing::new(r.take::<32>()?);
        let created_sec = i64::from_le_bytes(r.take::<8>()?);
        Ok(PairingOffer {
            folder_id,
            initiator_pub,
            secret,
            created_sec,
        })
    }

    pub fn short_code(&self, hints: TransportHints) -> String {
        short_code_for(&self.serialize(), hints)
    }

    pub fn qr_content(&self) -> Vec<u8> {
        self.serialize()
    }

    pub fn qr_code(&self) -> Result<qrcode::QrCode, qrcode::types::QrError> {
        qrcode::QrCode::new(self.qr_content())
    }
}

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
    pub fn compute_mac(
        offer_bytes: &[u8],
        one_time_secret: &[u8; 32],
        responder_pub: &DeviceId,
    ) -> [u8; 32] {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(one_time_secret).expect("HMAC accepts any key length");
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
        let mut r = Reader::new(bytes);
        let _magic = r.take::<4>()?;
        let _version = r.take::<1>()?;
        Ok(PairingResponse {
            responder_pub: r.take::<32>()?,
            mac: r.take::<32>()?,
            created_sec: i64::from_le_bytes(r.take::<8>()?),
        })
    }

    pub fn verify(&self, offer: &PairingOffer, offer_bytes: &[u8]) -> Result<(), PairingError> {
        let expect = Self::compute_mac(offer_bytes, offer.one_time_secret(), &self.responder_pub);
        if bool::from(expect.ct_ne(&self.mac)) {
            return Err(PairingError::MacMismatch);
        }

        if offer.serialize() != offer_bytes {
            return Err(PairingError::MacMismatch);
        }
        Ok(())
    }
}

pub fn respond(offer: &PairingOffer, responder: &DeviceIdentity, now_sec: i64) -> PairingResponse {
    let offer_bytes = offer.serialize();
    PairingResponse {
        responder_pub: *responder.public(),
        mac: PairingResponse::compute_mac(
            &offer_bytes,
            offer.one_time_secret(),
            responder.public(),
        ),
        created_sec: now_sec,
    }
}

#[derive(Debug)]
pub struct CompletedPairing {
    pub peer_pub: DeviceId,

    pub wrapped_for_self: [u8; WRAPPED_LEN],

    pub wrapped_for_peer: [u8; WRAPPED_LEN],
}

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

const GRANT_INFO: &[u8] = b"ferry/v1/pair-grant";

const GRANT_SALT: &[u8] = b"ferry/v1/pair-grant-salt";

pub const GRANT_MAGIC: [u8; 4] = *b"FRGR";

pub const GRANT_VERSION: u8 = 1;

const GRANT_NONCE_LEN: usize = 12;

pub fn derive_pair_grant_key(one_time_secret: &[u8]) -> Result<[u8; 32], GrantError> {
    let hk = Hkdf::<Sha256>::new(Some(GRANT_SALT), one_time_secret);
    let mut okm = [0u8; 32];
    hk.expand(GRANT_INFO, &mut okm)
        .map_err(|_| GrantError::Internal)?;
    Ok(okm)
}

fn offer_one_time_secret(offer_bytes: &[u8]) -> Result<&[u8], GrantError> {
    offer_bytes.get(53..85).ok_or(GrantError::OfferTruncated {
        need: 85,
        have: offer_bytes.len(),
    })
}

fn grant_cipher(key: &[u8; 32]) -> ChaCha20Poly1305 {
    use chacha20poly1305::aead::KeyInit;
    ChaCha20Poly1305::new(key.into())
}

pub fn seal_pair_grant(offer_bytes: &[u8], body: &[u8]) -> Result<Vec<u8>, GrantError> {
    let secret = offer_one_time_secret(offer_bytes)?;
    let key = derive_pair_grant_key(secret)?;
    let cipher = grant_cipher(&key);
    let mut nonce_bytes = [0u8; GRANT_NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload {
                msg: body,
                aad: offer_bytes,
            },
        )
        .map_err(|_| GrantError::Internal)?;

    let mut out = Vec::with_capacity(GRANT_MAGIC.len() + 1 + GRANT_NONCE_LEN + ct.len());
    out.extend_from_slice(&GRANT_MAGIC);
    out.push(GRANT_VERSION);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    Ok(out)
}

pub fn open_pair_grant(offer_bytes: &[u8], raw: &[u8]) -> Result<Vec<u8>, GrantError> {
    const HEADER_LEN: usize = 4 + 1 + GRANT_NONCE_LEN;
    if raw.len() < HEADER_LEN || raw[..4] != GRANT_MAGIC || raw[4] != GRANT_VERSION {
        return Err(GrantError::Malformed {
            need: HEADER_LEN,
            have: raw.len(),
        });
    }
    let secret = offer_one_time_secret(offer_bytes)?;
    let key = derive_pair_grant_key(secret)?;
    let cipher = grant_cipher(&key);
    cipher
        .decrypt(
            Nonce::from_slice(&raw[5..5 + GRANT_NONCE_LEN]),
            Payload {
                msg: &raw[HEADER_LEN..],
                aad: offer_bytes,
            },
        )
        .map_err(|_| GrantError::Auth)
}

fn code_payload(offer_bytes: &[u8], hints: TransportHints) -> ([u8; 10], u16) {
    let digest = blake3::hash(offer_bytes);
    let mut data = [0u8; 10];
    data[..2].copy_from_slice(&hints.to_bytes());
    data[2..].copy_from_slice(&digest.as_bytes()[..8]);
    let crc = crc32(&data);
    (data, (crc >> 16) as u16)
}

pub fn short_code_for(offer_bytes: &[u8], hints: TransportHints) -> String {
    let (data, check) = code_payload(offer_bytes, hints);
    let main = base32::encode(&data);
    let check_be = check.to_be_bytes();
    let tail = base32::encode(&check_be);
    format!(
        "{}-{}-{}-{}-{}",
        &main[0..4],
        &main[4..8],
        &main[8..12],
        &main[12..16],
        tail
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifiedCode {
    pub hints: TransportHints,
}

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

    const ALICE_SK_HEX: &str = "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a";

    fn alice() -> DeviceIdentity {
        DeviceIdentity::from_secret_bytes(&unhex(ALICE_SK_HEX).unwrap())
    }

    fn test_offer() -> (PairingOffer, Vec<u8>) {
        let offer = PairingOffer::create_with_rng(
            [3u8; 16],
            &alice(),
            1_700_000_000,
            FixedRng::new("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"),
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

        assert_eq!(&bytes[53..85], &(0u8..=0x1f).collect::<Vec<u8>>()[..]);
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
        assert!(matches!(
            PairingOffer::parse(&evil),
            Err(PairingError::BadMagic)
        ));
        evil = bytes.clone();
        evil[4] = 2;
        assert!(matches!(
            PairingOffer::parse(&evil),
            Err(PairingError::BadVersion(2))
        ));
        evil = bytes[..92].to_vec();
        assert!(matches!(
            PairingOffer::parse(&evil),
            Err(PairingError::Truncated { need: 93, have: 92 })
        ));

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
        let symbols: Vec<(usize, char)> = code
            .chars()
            .enumerate()
            .filter(|(_, c)| *c != '-')
            .collect();

        for pos in 0..symbols.len() {
            let original = symbols[pos].1;

            for sub in ALPHABET_TEST {
                if *sub == original {
                    continue;
                }
                let mut typed: Vec<char> = code.chars().collect();
                typed[symbols[pos].0] = *sub;
                let err =
                    verify_short_code(&typed.into_iter().collect::<String>(), &bytes).unwrap_err();
                assert!(
                    matches!(
                        err,
                        PairingError::CodeChecksumMismatch | PairingError::CodeHashMismatch
                    ),
                    "typo '{original}'->'{sub}' gave {err:?}"
                );
                break;
            }
        }

        for bad in ['0', '1', 'I', 'O'] {
            let mut typed: Vec<char> = code.chars().collect();
            typed[0] = bad;
            assert!(matches!(
                verify_short_code(&typed.into_iter().collect::<String>(), &bytes),
                Err(PairingError::Code(Base32Error::InvalidChar(c))) if c == bad
            ));
        }

        assert!(verify_short_code("ABCD", &bytes).is_err());
        assert!(verify_short_code(&code.replace('-', ""), &bytes).is_ok());
    }

    const ALPHABET_TEST: &[char] = &[
        '2', '3', '4', '5', '6', '7', '8', '9', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'J', 'K',
        'L', 'M', 'N', 'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z',
    ];

    #[test]
    fn code_binds_to_its_exact_payload() {
        let (_offer, bytes) = test_offer();
        let code = short_code_for(&bytes, TransportHints::default());

        let mut other = bytes.clone();
        other[70] ^= 1;
        assert!(matches!(
            verify_short_code(&code, &other),
            Err(PairingError::CodeHashMismatch)
        ));
    }

    #[test]
    fn mac_verify_match_passes_and_single_bit_flip_fails() {
        let (offer, offer_bytes) = test_offer();
        let responder = DeviceIdentity::generate();
        let resp = respond(&offer, &responder, 1_700_000_050);
        resp.verify(&offer, &offer_bytes).unwrap();

        for byte in [0usize, 15, 31] {
            for bit in [0x01u8, 0x80] {
                let mut evil = resp.clone();
                evil.mac[byte] ^= bit;
                assert!(matches!(
                    evil.verify(&offer, &offer_bytes),
                    Err(PairingError::MacMismatch)
                ));
            }
        }
    }

    #[test]
    fn response_mac_binds_transcript_and_rejects_fakes() {
        let (offer, offer_bytes) = test_offer();
        let responder = DeviceIdentity::generate();
        let resp = respond(&offer, &responder, 1_700_000_100);
        resp.verify(&offer, &offer_bytes).unwrap();

        let mut evil = resp.clone();
        evil.responder_pub[0] ^= 1;
        assert!(matches!(
            evil.verify(&offer, &offer_bytes),
            Err(PairingError::MacMismatch)
        ));

        let mut evil = resp.clone();
        evil.mac[31] ^= 1;
        assert!(matches!(
            evil.verify(&offer, &offer_bytes),
            Err(PairingError::MacMismatch)
        ));

        let mut evil_bytes = offer_bytes.clone();
        evil_bytes[5] ^= 1;
        assert!(matches!(
            resp.verify(&offer, &evil_bytes),
            Err(PairingError::MacMismatch)
        ));

        let rt = PairingResponse::parse(&resp.serialize()).unwrap();
        rt.verify(&offer, &offer_bytes).unwrap();
    }

    #[test]
    fn response_serialization_layout() {
        let responder = DeviceIdentity::generate();
        let resp = respond(
            &PairingOffer::parse(&test_offer().1).unwrap(),
            &responder,
            42,
        );
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

        assert!(qr.width() >= 21);

        let colors = qr.to_colors();
        assert_eq!(colors.len(), qr.width() * qr.width());
        assert!(colors.contains(&qrcode::Color::Dark));
        assert!(colors.contains(&qrcode::Color::Light));
    }

    #[test]
    fn grant_seal_open_round_trip_binds_the_offer() {
        let (_offer, offer_bytes) = test_offer();
        let body = br#"{"wrapped_for_peer":"ab","poly":7}"#;

        let sealed = seal_pair_grant(&offer_bytes, body).unwrap();
        assert_eq!(&sealed[..4], &GRANT_MAGIC, "FRGR magic");
        assert_eq!(sealed[4], GRANT_VERSION);

        assert_eq!(sealed.len(), 4 + 1 + 12 + body.len() + 16);
        assert_eq!(
            open_pair_grant(&offer_bytes, &sealed).unwrap(),
            body.as_slice()
        );

        let mut other_bytes = offer_bytes.clone();
        other_bytes[70] ^= 1;
        assert!(matches!(
            open_pair_grant(&other_bytes, &sealed),
            Err(GrantError::Auth)
        ));

        for idx in [5usize, 12, sealed.len() - 1] {
            let mut evil = sealed.clone();
            evil[idx] ^= 0x80;
            assert!(matches!(
                open_pair_grant(&offer_bytes, &evil),
                Err(GrantError::Auth)
            ));
        }

        assert!(matches!(
            open_pair_grant(&offer_bytes, &sealed[..16]),
            Err(GrantError::Malformed { .. })
        ));
        let mut evil_magic = sealed.clone();
        evil_magic[0] = b'X';
        assert!(matches!(
            open_pair_grant(&offer_bytes, &evil_magic),
            Err(GrantError::Malformed { .. })
        ));

        let k1 = derive_pair_grant_key(&offer_bytes[53..85]).unwrap();
        let k2 = derive_pair_grant_key(&offer_bytes[53..85]).unwrap();
        assert_eq!(k1, k2);
        assert_ne!(k1, derive_pair_grant_key(b"a different secret").unwrap());
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

        let code = offer.short_code(TransportHints(TransportHints::DIRECT_LAN));
        let verified = verify_short_code(&code, &offer_bytes).unwrap();
        assert_eq!(verified.hints, TransportHints(TransportHints::DIRECT_LAN));

        let resp = respond(&offer, &b, 456);
        let fmk = crate::folder_key::generate_fmk();
        let done = complete_pairing(&offer, &offer_bytes, &resp, &fmk, &a).unwrap();
        assert_eq!(done.peer_pub, *b.public());

        let got_a =
            crate::folder_key::unwrap_folder_key(&done.wrapped_for_self, &offer.folder_id, &a)
                .unwrap();
        let got_b =
            crate::folder_key::unwrap_folder_key(&done.wrapped_for_peer, &offer.folder_id, &b)
                .unwrap();
        assert_eq!(*got_a, fmk);
        assert_eq!(*got_b, fmk);
    }
}
