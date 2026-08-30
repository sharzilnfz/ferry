













use ferry_store::format::{put_bytes, put_u16, put_u32, put_u64, put_u8, BlobId, BlobKind, Reader};
use ferry_store::index::IndexEntry;

use crate::error::{ByeReason, ProtoError};
use crate::version::ProtocolVersion;
use crate::WIRE_MAGIC;


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





pub const FLAG_EXTENSION_AWARE: u64 = 1 << 0;


pub const MAX_REQUEST_ITEMS: usize = 512;
pub const MAX_REQUEST_PACKS: usize = 128;
pub const MAX_BATCH_ITEMS: usize = 512;


pub fn is_preauth_type(t: u8) -> bool {
    matches!(
        t,
        MSG_HELLO | MSG_HELLO_ACK | MSG_AUTH_INIT | MSG_AUTH_CONFIRM
    )
}


pub const KNOWN_TYPES: &[u8] = &[
    MSG_HELLO,
    MSG_HELLO_ACK,
    MSG_AUTH_INIT,
    MSG_AUTH_CONFIRM,
    MSG_FOLDER_OFFER,
    MSG_INDEX_ADVERT,
    MSG_REQUEST_ITEMS,
    MSG_REQUEST_PACKS,
    MSG_ITEM_BATCH,
    MSG_PACK_ITEM,
    MSG_BYE,
];






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



fn rd_u16(r: &mut Reader<'_>) -> Result<u16, ProtoError> {
    let b = r
        .take(2)
        .map_err(|_| ProtoError::ProtocolViolation("truncated"))?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}






#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    
    pub version: ProtocolVersion,
    pub flags: u64,
    
    pub eph_pub: [u8; 32],
    
    pub stat_pub: ferry_crypto::identity::DeviceId,
    
    pub nonce: [u8; 32],
}

impl Hello {
    pub const PAYLOAD_LEN: usize = 2 + 8 + 32 + 32 + 32;

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::PAYLOAD_LEN);
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





#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelloAck {
    
    pub version: ProtocolVersion,
    
    pub agreed: ProtocolVersion,
    pub flags: u64,
    pub eph_pub: [u8; 32],
    pub stat_pub: ferry_crypto::identity::DeviceId,
    pub nonce: [u8; 32],
}

impl HelloAck {
    pub const PAYLOAD_LEN: usize = 4 + 8 + 32 + 32 + 32;

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::PAYLOAD_LEN);
        put_u16(&mut out, self.version.to_u16());
        put_u16(&mut out, self.agreed.to_u16());
        put_u64(&mut out, self.flags);
        put_bytes(&mut out, &self.eph_pub);
        put_bytes(&mut out, &self.stat_pub);
        put_bytes(&mut out, &self.nonce);
        out
    }

    pub fn parse(payload: &[u8]) -> Result<Self, ProtoError> {
        let mut r = Reader::new(payload);
        let version = ProtocolVersion::from_u16(rd_u16(&mut r)?);
        let agreed = ProtocolVersion::from_u16(rd_u16(&mut r)?);
        let flags = r.u64().map_err(|_| bad("ack short"))?;
        let eph_pub = r.array::<32>().map_err(|_| bad("ack short"))?;
        let stat_pub = r.array::<32>().map_err(|_| bad("ack short"))?;
        let nonce = r.array::<32>().map_err(|_| bad("ack short"))?;
        r.expect_end().map_err(|_| bad("ack trailing"))?;
        Ok(HelloAck {
            version,
            agreed,
            flags,
            eph_pub,
            stat_pub,
            nonce,
        })
    }
}






#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthProof {
    pub ciphertext: Vec<u8>,
}

impl AuthProof {
    pub const CT_LEN: usize = 48; 

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





#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderOffer {
    pub folder_id: [u8; 16],
    pub manifest_id: BlobId,
    
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








#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexAdvert {
    pub entries: Vec<IndexEntry>,
    pub more: bool,
}

impl IndexAdvert {
    
    pub const MAX_ROWS: usize = 2048;

    pub fn encode(&self) -> Vec<u8> {
        let mut out = ferry_store::index::table_plain(&self.entries);
        out.push(u8::from(self.more));
        out
    }

    pub fn parse(payload: &[u8]) -> Result<Self, ProtoError> {
        if payload.is_empty() {
            return Err(ProtoError::ProtocolViolation("advert empty"));
        }
        let more_byte = payload[payload.len() - 1];
        let entries = ferry_store::index::table_parse(&payload[..payload.len() - 1])
            .map_err(|_| ProtoError::ProtocolViolation("advert table malformed"))?;
        match more_byte {
            0 => Ok(IndexAdvert {
                entries,
                more: false,
            }),
            1 => Ok(IndexAdvert {
                entries,
                more: true,
            }),
            _ => Err(ProtoError::ProtocolViolation("advert flag invalid")),
        }
    }
}








#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestItems {
    pub folder_id: [u8; 16],
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
        let mut out = Vec::with_capacity(20 + self.items.len() * 33);
        put_bytes(&mut out, &self.folder_id);
        put_u32(&mut out, self.items.len() as u32);
        for (kind, id) in &self.items {
            put_u8(&mut out, kind.to_u8());
            put_bytes(&mut out, id);
        }
        Ok(out)
    }

    pub fn parse(payload: &[u8]) -> Result<Self, ProtoError> {
        let mut r = Reader::new(payload);
        let folder_id = r.array::<16>().map_err(|_| bad("req short"))?;
        let n = r.u32().map_err(|_| bad("req short"))? as usize;
        if n > MAX_REQUEST_ITEMS {
            return Err(ProtoError::ProtocolViolation("request too many items"));
        }
        let mut items = Vec::with_capacity(n);
        for _ in 0..n {
            let kb = r.u8().map_err(|_| bad("req short"))?;
            let kind =
                BlobKind::from_u8(kb).ok_or(ProtoError::ProtocolViolation("unknown blob kind"))?;
            let id = r.array::<32>().map_err(|_| bad("req short"))?;
            items.push((kind, id));
        }
        r.expect_end().map_err(|_| bad("req trailing"))?;
        Ok(RequestItems { folder_id, items })
    }
}


#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestPacks {
    pub folder_id: [u8; 16],
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
        let mut out = Vec::with_capacity(20 + self.packs.len() * 32);
        put_bytes(&mut out, &self.folder_id);
        put_u32(&mut out, self.packs.len() as u32);
        for p in &self.packs {
            put_bytes(&mut out, p);
        }
        Ok(out)
    }

    pub fn parse(payload: &[u8]) -> Result<Self, ProtoError> {
        let mut r = Reader::new(payload);
        let folder_id = r.array::<16>().map_err(|_| bad("reqp short"))?;
        let n = r.u32().map_err(|_| bad("reqp short"))? as usize;
        if n > MAX_REQUEST_PACKS {
            return Err(ProtoError::ProtocolViolation("request too many packs"));
        }
        let mut packs = Vec::with_capacity(n);
        for _ in 0..n {
            packs.push(r.array::<32>().map_err(|_| bad("reqp short"))?);
        }
        r.expect_end().map_err(|_| bad("reqp trailing"))?;
        Ok(RequestPacks { folder_id, packs })
    }
}






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
            let kind =
                BlobKind::from_u8(kb).ok_or(ProtoError::ProtocolViolation("unknown blob kind"))?;
            let id = r.array::<32>().map_err(|_| bad("batch short"))?;
            let len = r.u64().map_err(|_| bad("batch short"))? as usize;
            let bytes = r.take(len).map_err(|_| bad("batch truncated"))?.to_vec();
            items.push((kind, id, bytes));
        }
        r.expect_end().map_err(|_| bad("batch trailing"))?;
        Ok(ItemBatch { items })
    }
}




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

    fn roundtrip<T>(
        value: T,
        encode: impl Fn(&T) -> Vec<u8>,
        parse: impl Fn(&[u8]) -> Result<T, ProtoError>,
    ) -> T
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
        roundtrip(body, super::FrameBody::encode, FrameBody::parse);

        let mut evil = FrameBody::new(MSG_BYE, ProtocolVersion::V1_0, vec![]).encode();
        evil[0] ^= 0xFF;
        assert!(matches!(
            FrameBody::parse(&evil),
            Err(ProtoError::ProtocolViolation("bad magic"))
        ));
    }

    #[test]
    fn hello_and_ack_layouts_are_pinned_and_round_trip() {
        let h = Hello {
            version: ProtocolVersion::V1_0,
            flags: FLAG_EXTENSION_AWARE,
            eph_pub: [1; 32],
            stat_pub: [2; 32],
            nonce: [3; 32],
        };
        assert_eq!(h.encode().len(), Hello::PAYLOAD_LEN);
        assert_eq!(h.encode().len(), 106);
        
        let bytes = h.encode();
        for cut in 0..bytes.len() {
            assert!(Hello::parse(&bytes[..cut]).is_err(), "cut {cut}");
        }
        
        let mut long = bytes.clone();
        long.push(0);
        assert!(Hello::parse(&long).is_err());
        
        roundtrip(h, super::Hello::encode, Hello::parse);

        let ack = HelloAck {
            version: ProtocolVersion::new(1, 9),
            agreed: ProtocolVersion::V1_0,
            flags: FLAG_EXTENSION_AWARE,
            eph_pub: [4; 32],
            stat_pub: [5; 32],
            nonce: [6; 32],
        };
        assert_eq!(ack.encode().len(), HelloAck::PAYLOAD_LEN);
        assert_eq!(ack.encode().len(), 108);
        let ack_bytes = ack.encode();
        for cut in 0..ack_bytes.len() {
            assert!(HelloAck::parse(&ack_bytes[..cut]).is_err(), "cut {cut}");
        }
        roundtrip(ack, super::HelloAck::encode, HelloAck::parse);
    }

    #[test]
    fn folder_offer_reserved_must_be_zero() {
        let offer = FolderOffer {
            folder_id: [9; 16],
            manifest_id: [7; 32],
            reserved: 0,
        };
        let mut evil = offer.encode();
        evil[48] = 1; 
        assert!(matches!(
            FolderOffer::parse(&evil),
            Err(ProtoError::ProtocolViolation("offer reserved nonzero"))
        ));
        roundtrip(offer, super::FolderOffer::encode, FolderOffer::parse);
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
        let advert = IndexAdvert {
            entries: entries.clone(),
            more: true,
        };
        
        assert_eq!(
            &advert.encode()[..advert.encode().len() - 1],
            ferry_store::index::table_plain(&entries).as_slice()
        );
        let parsed = IndexAdvert::parse(&advert.encode()).unwrap();
        assert_eq!(parsed.entries, entries);
        assert!(parsed.more);
        assert!(
            !IndexAdvert::parse(
                &IndexAdvert {
                    entries,
                    more: false
                }
                .encode()
            )
            .unwrap()
            .more
        );
        
        let mut evil = advert.encode();
        *evil.last_mut().unwrap() = 2;
        assert!(matches!(
            IndexAdvert::parse(&evil),
            Err(ProtoError::ProtocolViolation("advert flag invalid"))
        ));
    }

    #[test]
    fn request_and_batch_caps_are_enforced_on_both_sides() {
        let big_req = RequestItems {
            folder_id: [0; 16],
            items: vec![(BlobKind::DataChunk, [0; 32]); MAX_REQUEST_ITEMS + 1],
        };
        assert!(big_req.encode().is_err());
        
        let mut evil = Vec::new();
        put_bytes(&mut evil, &[0; 16]);
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
            folder_id: [0; 16],
            packs: vec![[0; 32]; MAX_REQUEST_PACKS + 1],
        };
        assert!(big_packs.encode().is_err());
        let mut evil_p = Vec::new();
        put_bytes(&mut evil_p, &[0; 16]);
        put_u32(&mut evil_p, (MAX_REQUEST_PACKS + 1) as u32);
        assert!(RequestPacks::parse(&evil_p).is_err());

        
        let ok = RequestItems {
            folder_id: [7; 16],
            items: vec![(BlobKind::Manifest, [1; 32])],
        };
        roundtrip(ok, |r| r.encode().unwrap(), RequestItems::parse);
        let marker = RequestItems {
            folder_id: [7; 16],
            items: vec![],
        };
        roundtrip(marker, |r| r.encode().unwrap(), RequestItems::parse);
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
            roundtrip(Bye { reason: r }, super::Bye::encode, Bye::parse);
        }
        assert!(Bye::parse(&[99]).is_err());
    }

    #[test]
    fn auth_proof_pins_ciphertext_length() {
        assert!(AuthProof::new(vec![0u8; 47]).is_err());
        assert!(AuthProof::new(vec![0u8; 49]).is_err());
        let ok = AuthProof::new(vec![0xAB; 48]).unwrap();
        assert_eq!(AuthProof::parse(&ok.encode()).unwrap(), ok);
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
