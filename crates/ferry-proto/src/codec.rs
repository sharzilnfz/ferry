//! The message inventory: exact payload layouts for every wire message.
//!
//! All integers little-endian (the store-format convention); all hashes raw
//! 32-byte values. Payloads here are the bytes AFTER `type || version` in a
//! frame body — see `docs/store-format.md`, "Wire protocol v1", for the full
//! tables this module implements one-to-one.
//!
//! Serializations defined elsewhere are REUSED, never re-encoded:
//! - [`IndexAdvert`] payloads ARE index-table serializations
//!   ([`ferry_store::index::table_plain`]), rows sorted by `(kind, id)`.
//! - Manifests and tree nodes travel inside [`ItemBatch`] exactly as stored.
//! - Packs travel inside [`PackItem`] as whole ciphertext files under their
//!   BLAKE3 names.

use ferry_store::format::{put_bytes, put_u16, put_u32, put_u64, put_u8, BlobId, BlobKind, Reader};
use ferry_store::index::IndexEntry;

use crate::error::{ByeReason, ProtoError};
use crate::version::ProtocolVersion;
use crate::WIRE_MAGIC;

// Message type registry (normative values).
pub const MSG_HELLO: u8 = 0x01;
pub const MSG_HELLO_ACK: u8 = 0x02;
pub const MSG_AUTH_INIT: u8 = 0x03;
pub const MSG_AUTH_CONFIRM: u8 = 0x04;
pub const MSG_FOLDER_OFFER: u8 = 0x05;
pub const MSG_INDEX_ADVERT: u8 = 0x06;
pub const MSG_REQUEST_ITEMS: u8 = 0x07;
pub const MSG_REQUEST_PACKS: u8 = 0x08;
pub const MSG_ITEM_BATCH: u8 = 0x09;
pub const MSG_PACK_ITEM: u8 = 0x0A;
pub const MSG_BYE: u8 = 0x0B;

/// Feature flags advertised in Hello/HelloAck. v1 defines bit 0 only:
/// "I implement the skip-if-flagged rule for unknown higher-version message
/// types". Every conforming v1 engine sets it. Unknown received bits are
/// ignored, never errors.
pub const FLAG_EXTENSION_AWARE: u64 = 1 << 0;

/// Per-frame item caps. Senders MUST split; receivers MUST reject beyond.
pub const MAX_REQUEST_ITEMS: usize = 512;
pub const MAX_REQUEST_PACKS: usize = 128;
pub const MAX_BATCH_ITEMS: usize = 512;

/// True for the four pre-auth handshake types; everything else is sealed.
pub fn is_preauth_type(t: u8) -> bool {
    matches!(
        t,
        MSG_HELLO | MSG_HELLO_ACK | MSG_AUTH_INIT | MSG_AUTH_CONFIRM
    )
}

/// A frame body before sealing: `magic || type || version || payload`.
///
/// The magic rides INSIDE the body (not in the length prefix) so that even
/// unsealed frames self-identify, matching the container-file habit of
/// rejecting foreign bytes early.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameBody {
    pub msg_type: u8,
    pub version: ProtocolVersion,
    pub payload: Vec<u8>,
}

impl FrameBody {
    pub fn new(msg_type: u8, version: ProtocolVersion, payload: Vec<u8>) -> Self {
        FrameBody {
            msg_type,
            version,
            payload,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + 3 + self.payload.len());
        out.extend_from_slice(&WIRE_MAGIC);
        put_u8(&mut out, self.msg_type);
        put_u16(&mut out, self.version.to_u16());
        put_bytes(&mut out, &self.payload);
        out
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, ProtoError> {
        let mut r = Reader::new(bytes);
        if r.take(4).map_err(|_| bad("truncated"))? != WIRE_MAGIC {
            return Err(bad("bad magic"));
        }
        let msg_type = r.u8().map_err(|_| bad("type"))?;
        let version = ProtocolVersion::from_u16(rd_u16(&mut r)?);
        let payload = r.rest().to_vec();
        Ok(FrameBody {
            msg_type,
            version,
            payload,
        })
    }
}

fn bad(why: &'static str) -> ProtoError {
    ProtoError::ProtocolViolation(why)
}

/// ferry-store's Reader has no u16 (the store format never needed one); the
/// wire version field is the first u16, so read it here.
fn rd_u16(r: &mut Reader<'_>) -> Result<u16, ProtoError> {
    let b = r.take(2).map_err(|_| ProtoError::ProtocolViolation("truncated"))?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

// --- Hello / HelloAck ------------------------------------------------------

/// Handshake opener. The static public key doubles as the sender's
/// device_id; the ephemeral key and nonce are fresh per connection and feed
/// the transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    /// Maximum protocol version the sender speaks.
    pub version: ProtocolVersion,
    pub flags: u64,
    /// Fresh X25519 public key for this connection only.
    pub eph_pub: [u8; 32],
    /// Sender's long-lived device key (its identity).
    pub stat_pub: ferry_crypto::identity::DeviceId,
    /// Fresh challenge material bound into the transcript.
    pub nonce: [u8; 32],
}

impl Hello {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(106);
        put_u16(&mut out, self.version.to_u16());
        put_u64(&mut out, self.flags);
        put_bytes(&mut out, &self.eph_pub);
        put_bytes(&mut out, &self.stat_pub);
        put_bytes(&mut out, &self.nonce);
        out
    }

    pub fn parse(payload: &[u8]) -> Result<Self, ProtoError> {
        let mut r = Reader::new(payload);
        let version = ProtocolVersion::from_u16(rd_u16(&mut r)?);
        let flags = r.u64().map_err(|_| bad("hello short"))?;
        let eph_pub = r.array::<32>().map_err(|_| bad("hello short"))?;
        let stat_pub = r.array::<32>().map_err(|_| bad("hello short"))?;
        let nonce = r.array::<32>().map_err(|_| bad("hello short"))?;
        r.expect_end().map_err(|_| bad("hello trailing"))?;
        Ok(Hello {
            version,
            flags,
            eph_pub,
            stat_pub,
            nonce,
        })
    }
}

/// Responder's half of the hello exchange. `version` is the CHOSEN agreed
/// version (min of both maxima), not the responder's own maximum.
pub type HelloAck = Hello;

// --- AUTH_INIT / AUTH_CONFIRM ----------------------------------------------

/// One sealed auth message: ChaCha20-Poly1305 ciphertext (48 bytes) over the
/// sender's device_id, keyed through the handshake secret, AAD = transcript
/// hash. Producing a valid tag requires the sender's static secret term.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthProof {
    pub ciphertext: Vec<u8>,
}

impl AuthProof {
    pub const CT_LEN: usize = 48; // 32-byte plaintext + 16-byte tag

    pub fn new(ciphertext: Vec<u8>) -> Result<Self, ProtoError> {
        if ciphertext.len() != Self::CT_LEN {
            return Err(bad("auth proof length"));
        }
        Ok(AuthProof { ciphertext })
    }

    pub fn encode(&self) -> Vec<u8> {
        self.ciphertext.clone()
    }

    pub fn parse(payload: &[u8]) -> Result<Self, ProtoError> {
        Self::new(payload.to_vec())
    }
}

// --- FOLDER_OFFER ------------------------------------------------------------

/// Announcement of one folder's current root manifest. `manifest_id` zero
/// means "I know nothing about this folder" (fresh device, or not shared).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderOffer {
    pub folder_id: [u8; 16],
    pub manifest_id: BlobId,
    /// Reserved zeros in v1; receivers MUST reject nonzero.
    pub reserved: u32,
}

impl FolderOffer {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(52);
        put_bytes(&mut out, &self.folder_id);
        put_bytes(&mut out, &self.manifest_id);
        put_u32(&mut out, self.reserved);
        out
    }

    pub fn parse(payload: &[u8]) -> Result<Self, ProtoError> {
        let mut r = Reader::new(payload);
        let folder_id = r.array::<16>().map_err(|_| bad("offer short"))?;
        let manifest_id = r.array::<32>().map_err(|_| bad("offer short"))?;
        let reserved = r.u32().map_err(|_| bad("offer short"))?;
        r.expect_end().map_err(|_| bad("offer trailing"))?;
        if reserved != 0 {
            return Err(ProtoError::ProtocolViolation("offer reserved nonzero"));
        }
        Ok(FolderOffer {
            folder_id,
            manifest_id,
            reserved,
        })
    }
}

// --- INDEX_ADVERT ------------------------------------------------------------

/// Advertisement of the sender's blob locations for one folder: EXACTLY the
/// index-table serialization from `docs/store-format.md` (u32 count, rows of
/// `kind || id || pack || off || len`, sorted by `(kind, id)`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexAdvert(pub Vec<IndexEntry>);

impl IndexAdvert {
    pub fn encode(&self) -> Vec<u8> {
        ferry_store::index::table_plain(&self.0)
    }

    pub fn parse(payload: &[u8]) -> Result<Self, ProtoError> {
        let entries = ferry_store::index::table_parse(payload)
            .map_err(|_| ProtoError::ProtocolViolation("advert table malformed"))?;
        Ok(IndexAdvert(entries))
    }
}

// --- REQUEST_ITEMS / REQUEST_PACKS ------------------------------------------

/// Ask the peer for specific blobs by kind + id. Served items arrive in
/// ITEM_BATCH frames; unservable ids are silently omitted (the requester
/// detects gaps after the terminator).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestItems {
    pub items: Vec<(BlobKind, BlobId)>,
}

impl RequestItems {
    pub fn encode(&self) -> Result<Vec<u8>, ProtoError> {
        if self.items.len() > MAX_REQUEST_ITEMS {
            return Err(ProtoError::FrameTooLarge {
                len: self.items.len(),
                max: MAX_REQUEST_ITEMS,
            });
        }
        let mut out = Vec::with_capacity(4 + self.items.len() * 33);
        put_u32(&mut out, self.items.len() as u32);
        for (kind, id) in &self.items {
            put_u8(&mut out, kind.to_u8());
            put_bytes(&mut out, id);
        }
        Ok(out)
    }

    pub fn parse(payload: &[u8]) -> Result<Self, ProtoError> {
        let mut r = Reader::new(payload);
        let n = r.u32().map_err(|_| bad("req short"))? as usize;
        if n > MAX_REQUEST_ITEMS {
            return Err(ProtoError::ProtocolViolation("request too many items"));
        }
        let mut items = Vec::with_capacity(n);
        for _ in 0..n {
            let kb = r.u8().map_err(|_| bad("req short"))?;
            let kind = BlobKind::from_u8(kb)
                .ok_or(ProtoError::ProtocolViolation("unknown blob kind"))?;
            let id = r.array::<32>().map_err(|_| bad("req short"))?;
            items.push((kind, id));
        }
        r.expect_end().map_err(|_| bad("req trailing"))?;
        Ok(RequestItems { items })
    }
}

/// Ask the peer for whole packs by ciphertext name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestPacks {
    pub packs: Vec<BlobId>,
}

impl RequestPacks {
    pub fn encode(&self) -> Result<Vec<u8>, ProtoError> {
        if self.packs.len() > MAX_REQUEST_PACKS {
            return Err(ProtoError::FrameTooLarge {
                len: self.packs.len(),
                max: MAX_REQUEST_PACKS,
            });
        }
        let mut out = Vec::with_capacity(4 + self.packs.len() * 32);
        put_u32(&mut out, self.packs.len() as u32);
        for p in &self.packs {
            put_bytes(&mut out, p);
        }
        Ok(out)
    }

    pub fn parse(payload: &[u8]) -> Result<Self, ProtoError> {
        let mut r = Reader::new(payload);
        let n = r.u32().map_err(|_| bad("reqp short"))? as usize;
        if n > MAX_REQUEST_PACKS {
            return Err(ProtoError::ProtocolViolation("request too many packs"));
        }
        let mut packs = Vec::with_capacity(n);
        for _ in 0..n {
            packs.push(r.array::<32>().map_err(|_| bad("reqp short"))?);
        }
        r.expect_end().map_err(|_| bad("reqp trailing"))?;
        Ok(RequestPacks { packs })
    }
}

// --- ITEM_BATCH / PACK_ITEM ---------------------------------------------------

/// One served blob: verified by the receiver against its OWN requested id
/// after decryption (`BLAKE3(plaintext) == id`) BEFORE anything touches the
/// store. An empty batch terminates every response sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemBatch {
    pub items: Vec<(BlobKind, BlobId, Vec<u8>)>,
}

impl ItemBatch {
    pub const TERMINATOR: ItemBatch = ItemBatch { items: Vec::new() };

    pub fn encode(&self) -> Result<Vec<u8>, ProtoError> {
        if self.items.len() > MAX_BATCH_ITEMS {
            return Err(ProtoError::FrameTooLarge {
                len: self.items.len(),
                max: MAX_BATCH_ITEMS,
            });
        }
        let mut out = Vec::new();
        put_u32(&mut out, self.items.len() as u32);
        for (kind, id, bytes) in &self.items {
            if bytes.is_empty() {
                return Err(ProtoError::ProtocolViolation("empty blob cannot exist"));
            }
            put_u8(&mut out, kind.to_u8());
            put_bytes(&mut out, id);
            put_u64(&mut out, bytes.len() as u64);
            put_bytes(&mut out, bytes);
        }
        Ok(out)
    }

    pub fn parse(payload: &[u8]) -> Result<Self, ProtoError> {
        let mut r = Reader::new(payload);
        let n = r.u32().map_err(|_| bad("batch short"))? as usize;
        if n > MAX_BATCH_ITEMS {
            return Err(ProtoError::ProtocolViolation("batch too many items"));
        }
        let mut items = Vec::with_capacity(n);
        for _ in 0..n {
            let kb = r.u8().map_err(|_| bad("batch short"))?;
            let kind = BlobKind::from_u8(kb)
                .ok_or(ProtoError::ProtocolViolation("unknown blob kind"))?;
            let id = r.array::<32>().map_err(|_| bad("batch short"))?;
            let len = r.u64().map_err(|_| bad("batch short"))? as usize;
            let bytes = r.take(len).map_err(|_| bad("batch truncated"))?.to_vec();
            items.push((kind, id, bytes));
        }
        r.expect_end().map_err(|_| bad("batch trailing"))?;
        Ok(ItemBatch { items })
    }
}

/// One served pack: the ENTIRE ciphertext file under its BLAKE3 name.
/// Receiver verifies `BLAKE3(ciphertext) == pack` BEFORE storing — no
/// decryption, no disk write, on mismatch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackItem {
    pub pack: BlobId,
    pub bytes: Vec<u8>,
}

impl PackItem {
    pub fn encode(&self) -> Result<Vec<u8>, ProtoError> {
        let mut out = Vec::with_capacity(36 + self.bytes.len());
        put_bytes(&mut out, &self.pack);
        put_u32(&mut out, self.bytes.len() as u32);
        put_bytes(&mut out, &self.bytes);
        Ok(out)
    }

    pub fn parse(payload: &[u8]) -> Result<Self, ProtoError> {
        let mut r = Reader::new(payload);
        let pack = r.array::<32>().map_err(|_| bad("packitem short"))?;
        let len = r.u32().map_err(|_| bad("packitem short"))? as usize;
        let bytes = r.take(len).map_err(|_| bad("packitem truncated"))?.to_vec();
        r.expect_end().map_err(|_| bad("packitem trailing"))?;
        if bytes.is_empty() {
            return Err(ProtoError::ProtocolViolation("empty pack"));
        }
        Ok(PackItem { pack, bytes })
    }
}

// --- BYE ---------------------------------------------------------------------

/// Graceful close with a reason code. Sent best-effort before disconnecting
/// on errors; always sent on clean completion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bye {
    pub reason: ByeReason,
}

impl Bye {
    pub fn encode(&self) -> Vec<u8> {
        vec![self.reason as u8]
    }

    pub fn parse(payload: &[u8]) -> Result<Self, ProtoError> {
        let mut r = Reader::new(payload);
        let code = r.u8().map_err(|_| bad("bye short"))?;
        r.expect_end().map_err(|_| bad("bye trailing"))?;
        match ByeReason::from_u8(code) {
            Some(reason) => Ok(Bye { reason }),
            None => Err(ProtoError::ProtocolViolation("unknown bye code")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferry_store::format::hex;

    fn roundtrip<T>(value: T, encode: impl Fn(&T) -> Vec<u8>, parse: impl Fn(&[u8]) -> Result<T, ProtoError>) -> T
    where
        T: PartialEq + core::fmt::Debug,
    {
        let parsed = parse(&encode(&value)).expect("parse own encoding");
        assert_eq!(parsed, value);
        parsed
    }

    #[test]
    fn frame_body_round_trips_and_rejects_foreign_magic() {
        let body = FrameBody::new(MSG_HELLO, ProtocolVersion::V1_0, vec![1, 2, 3]);
        roundtrip(body, |b| b.encode(), FrameBody::parse);

        let mut evil = FrameBody::new(MSG_BYE, ProtocolVersion::V1_0, vec![]).encode();
        evil[0] ^= 0xFF;
        assert!(matches!(
            FrameBody::parse(&evil),
            Err(ProtoError::ProtocolViolation("bad magic"))
        ));
    }

    #[test]
    fn hello_layout_is_106_bytes_and_round_trips() {
        let h = Hello {
            version: ProtocolVersion::V1_0,
            flags: FLAG_EXTENSION_AWARE,
            eph_pub: [1; 32],
            stat_pub: [2; 32],
            nonce: [3; 32],
        };
        assert_eq!(h.encode().len(), 2 + 8 + 32 + 32 + 32);
        // Truncation at every prefix length fails loudly, never guesses.
        let bytes = h.encode();
        for cut in 0..bytes.len() {
            assert!(Hello::parse(&bytes[..cut]).is_err(), "cut {cut}");
        }
        // Trailing garbage rejected.
        let mut long = bytes.clone();
        long.push(0);
        assert!(Hello::parse(&long).is_err());
        // Full round trip last (consumes h).
        roundtrip(h, |x| x.encode(), Hello::parse);
    }

    #[test]
    fn folder_offer_reserved_must_be_zero() {
        let offer = FolderOffer {
            folder_id: [9; 16],
            manifest_id: [7; 32],
            reserved: 0,
        };
        let mut evil = offer.encode();
        evil[48] = 1; // first reserved byte
        assert!(matches!(
            FolderOffer::parse(&evil),
            Err(ProtoError::ProtocolViolation("offer reserved nonzero"))
        ));
        roundtrip(offer, |o| o.encode(), FolderOffer::parse);
    }

    #[test]
    fn advert_reuses_the_index_table_serialization_exactly() {
        let entry = |k: BlobKind, idb: u8| IndexEntry {
            kind: k,
            id: [idb; 32],
            pack: [idb.wrapping_add(100); 32],
            plain_off: 42,
            plain_len: 7,
        };
        let entries = vec![
            entry(BlobKind::DataChunk, 1),
            entry(BlobKind::TreeNode, 2),
            entry(BlobKind::Manifest, 3),
        ];
        let advert = IndexAdvert(entries);
        assert_eq!(advert.encode(), ferry_store::index::table_plain(&advert.0));
        roundtrip(advert, |a| a.encode(), IndexAdvert::parse);
    }

    #[test]
    fn request_and_batch_caps_are_enforced_on_both_sides() {
        let big_req = RequestItems {
            items: vec![(BlobKind::DataChunk, [0; 32]); MAX_REQUEST_ITEMS + 1],
        };
        assert!(big_req.encode().is_err());
        // Hand-serialize an oversized count to attack the parser directly.
        let mut evil = Vec::new();
        put_u32(&mut evil, (MAX_REQUEST_ITEMS + 1) as u32);
        assert!(matches!(
            RequestItems::parse(&evil),
            Err(ProtoError::ProtocolViolation("request too many items"))
        ));

        let big_batch = ItemBatch {
            items: vec![(BlobKind::DataChunk, [0; 32], vec![1u8]); MAX_BATCH_ITEMS + 1],
        };
        assert!(big_batch.encode().is_err());
        let mut evil_b = Vec::new();
        put_u32(&mut evil_b, (MAX_BATCH_ITEMS + 1) as u32);
        assert!(ItemBatch::parse(&evil_b).is_err());

        let big_packs = RequestPacks {
            packs: vec![[0; 32]; MAX_REQUEST_PACKS + 1],
        };
        assert!(big_packs.encode().is_err());
    }

    #[test]
    fn item_batch_round_trips_multi_kilo_payloads() {
        let batch = ItemBatch {
            items: vec![
                (BlobKind::Manifest, [1; 32], vec![9u8; 200]),
                (BlobKind::TreeNode, [2; 32], vec![8u8; 64]),
                (BlobKind::DataChunk, [3; 32], vec![7u8; 4096]),
            ],
        };
        roundtrip(batch, |b| b.encode().unwrap(), ItemBatch::parse);

        // Empty blobs cannot exist; encoder refuses them outright.
        let empty = ItemBatch {
            items: vec![(BlobKind::DataChunk, [4; 32], vec![])],
        };
        assert!(matches!(
            empty.encode(),
            Err(ProtoError::ProtocolViolation("empty blob cannot exist"))
        ));
    }

    #[test]
    fn pack_item_rejects_empty_and_round_trips() {
        let p = PackItem {
            pack: [5; 32],
            bytes: vec![1u8; 1024],
        };
        roundtrip(p, |x| x.encode().unwrap(), PackItem::parse);
        assert!(PackItem::parse(&{
            let mut v = Vec::new();
            put_bytes(&mut v, &[5; 32]);
            put_u32(&mut v, 0);
            v
        })
        .is_err());
    }

    #[test]
    fn bye_codes_round_trip_and_unknown_code_is_violation() {
        for r in [
            ByeReason::Normal,
            ByeReason::VersionIncompatible,
            ByeReason::ProtocolViolation,
            ByeReason::AuthFailed,
            ByeReason::ResourceLimit,
            ByeReason::Internal,
        ] {
            roundtrip(Bye { reason: r }, |b| b.encode(), Bye::parse);
        }
        assert!(Bye::parse(&[99]).is_err());
    }

    #[test]
    fn auth_proof_pins_ciphertext_length() {
        assert!(AuthProof::new(vec![0u8; 47]).is_err());
        assert!(AuthProof::new(vec![0u8; 49]).is_err());
        let ok = AuthProof::new(vec![0xAB; 48]).unwrap();
        assert_eq!(
            AuthProof::parse(&ok.encode()).unwrap(),
            ok
        );
        assert_eq!(hex(&ok.encode()[..3]), "ababab");
    }

    #[test]
    fn preauth_types_cover_exactly_the_handshake() {
        for t in [MSG_HELLO, MSG_HELLO_ACK, MSG_AUTH_INIT, MSG_AUTH_CONFIRM] {
            assert!(is_preauth_type(t));
        }
        for t in [
            MSG_FOLDER_OFFER,
            MSG_INDEX_ADVERT,
            MSG_REQUEST_ITEMS,
            MSG_REQUEST_PACKS,
            MSG_ITEM_BATCH,
            MSG_PACK_ITEM,
            MSG_BYE,
        ] {
            assert!(!is_preauth_type(t), "{t:#04x}");
        }
    }
}
