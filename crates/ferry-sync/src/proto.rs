//! Protocol sketch (internal, throwaway — T-008 owns the durable version).
//!
//! One session on one connection, dialer speaks first:
//!
//! ```text
//! D -> L  HELLO { tag }                 (L replies with its own)
//! D -> L  OFFER { manifest bytes, agreed manifest id }
//! L -> D  OFFER { manifest bytes, agreed manifest id }
//!         [both sides pick donor/puller identically; see engine::pick_donor]
//! P -> W  REQ_META { tree-node ids }    (puller walks the offered tree)
//! W -> P  ITEM* ITEMS_DONE
//! P -> W  REQ_DATA { chunk ids }        (donor resolves to whole packs)
//! W -> P  (ITEM|PACK)* ITEMS_DONE
//! P -> W  AGREED { winning manifest id }   after materializing durably
//! ```
//!
//! `ERROR { text }` may replace any expected message; it aborts the session
//! without touching agreement state.
//!
//! Encodings reuse spec primitives from `docs/store-format.md` via
//! `ferry_store::format` (LE integers, raw 32-byte ids, u32-length-prefixed
//! byte strings). Manifests move byte-for-byte as stored. Encryption is OFF
//! in M0: these frames are plaintext; T-008 inserts AEAD around payloads.

use ferry_store::format::{put_bytes, put_u32, put_u8, BlobId, BlobKind, PackId, Reader};

use crate::transport::Connection;

/// Message tags (first payload byte of every frame).
pub mod tag {
    pub const HELLO: u8 = 0x01;
    pub const OFFER: u8 = 0x02;
    pub const REQ_META: u8 = 0x03;
    pub const REQ_DATA: u8 = 0x04;
    pub const ITEM: u8 = 0x05;
    pub const ITEMS_DONE: u8 = 0x06;
    pub const AGREED: u8 = 0x07;
    pub const ERROR: u8 = 0x08;
}

#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("unexpected message tag 0x{0:02x}")]
    BadTag(u8),
    #[error("malformed message: {0}")]
    Malformed(&'static str),
    #[error("peer reported: {0}")]
    PeerError(String),
}

#[derive(Debug)]
pub struct Hello {
    pub device_tag: String,
}

#[derive(Debug)]
pub struct Offer {
    /// The offered root manifest, serialized exactly as stored.
    pub manifest_bytes: Vec<u8>,
    /// Sender's current last-agreed manifest id (zeros if none yet).
    pub agreed_manifest_id: BlobId,
    /// Root tree id OF THAT AGREED MANIFEST (zeros if none). Lets the peer
    /// tell "unchanged since agreement" from "diverged" without reading
    /// anything — the foundation of clock-free donor selection.
    pub agreed_root_tree_id: BlobId,
}

#[derive(Debug)]
pub enum ItemPayload {
    /// A whole pack file, named by BLAKE3 of its ciphertext.
    Pack { name: PackId, bytes: Vec<u8> },
    /// An individual blob (tree node or manifest in M0), plaintext.
    Blob {
        kind: BlobKind,
        id: BlobId,
        bytes: Vec<u8>,
    },
}

fn send(conn: &mut dyn Connection, msg_tag: u8, body: &[u8]) -> Result<(), ProtoError> {
    let mut frame = Vec::with_capacity(body.len() + 1);
    frame.push(msg_tag);
    frame.extend_from_slice(body);
    conn.send_frame(&frame)?;
    Ok(())
}

fn parse_error_text(body: &[u8]) -> String {
    let mut r = Reader::new(body);
    get_bytes(&mut r)
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .unwrap_or_default()
}

/// Decode a `put_bytes`-encoded string: u32 LE length + raw bytes.
/// Encode a byte string with its u32 LE length (the wire convention for
/// variable-length fields; `format::put_bytes` itself writes raw bytes).
fn put_len_bytes(out: &mut Vec<u8>, v: &[u8]) {
    put_u32(out, v.len() as u32);
    put_bytes(out, v);
}

fn get_bytes<'a>(r: &mut Reader<'a>) -> Result<&'a [u8], ProtoError> {
    let n = r.u32().map_err(|_| ProtoError::Malformed("len"))? as usize;
    r.take(n).map_err(|_| ProtoError::Malformed("bytes"))
}

/// Receive one raw message `(tag, body)`; ERROR frames surface as
/// [`ProtoError::PeerError`]. Shared by typed readers and the donor's
/// multi-message serving loop.
pub fn recv_msg(conn: &mut dyn Connection) -> Result<(u8, Vec<u8>), ProtoError> {
    let frame = conn.recv_frame()?;
    let msg_tag = *frame.first().ok_or(ProtoError::Malformed("empty frame"))?;
    if msg_tag == tag::ERROR {
        return Err(ProtoError::PeerError(parse_error_text(&frame[1..])));
    }
    Ok((msg_tag, frame[1..].to_vec()))
}

/// Receive one message; ERROR frames become [`ProtoError::PeerError`].
fn expect(conn: &mut dyn Connection, want: &[u8]) -> Result<(u8, Vec<u8>), ProtoError> {
    let (msg_tag, body) = recv_msg(conn)?;
    if !want.contains(&msg_tag) {
        return Err(ProtoError::BadTag(msg_tag));
    }
    Ok((msg_tag, body))
}

pub fn send_hello(conn: &mut dyn Connection, device_tag: &str) -> Result<(), ProtoError> {
    let mut b = Vec::new();
    put_len_bytes(&mut b, device_tag.as_bytes());
    send(conn, tag::HELLO, &b)
}

pub fn recv_hello(conn: &mut dyn Connection) -> Result<Hello, ProtoError> {
    let (_, body) = expect(conn, &[tag::HELLO])?;
    decode_hello(&body)
}

pub fn decode_hello(body: &[u8]) -> Result<Hello, ProtoError> {
    let mut r = Reader::new(body);
    let t = get_bytes(&mut r).map_err(|_| ProtoError::Malformed("hello tag"))?;
    let device_tag =
        String::from_utf8(t.to_vec()).map_err(|_| ProtoError::Malformed("tag utf8"))?;
    validate_tag(&device_tag)?;
    Ok(Hello { device_tag })
}

/// Tags are directory names under the agreement store; keep them boring.
pub fn validate_tag(t: &str) -> Result<(), ProtoError> {
    let ok = !t.is_empty()
        && t.len() <= 64
        && t.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_');
    if ok {
        Ok(())
    } else {
        Err(ProtoError::Malformed(
            "device tag must be 1..=64 chars of [-A-Za-z0-9_]",
        ))
    }
}

pub fn send_offer(conn: &mut dyn Connection, offer: &Offer) -> Result<(), ProtoError> {
    let mut b = Vec::new();
    put_len_bytes(&mut b, &offer.manifest_bytes);
    put_bytes(&mut b, &offer.agreed_manifest_id);
    put_bytes(&mut b, &offer.agreed_root_tree_id);
    send(conn, tag::OFFER, &b)
}

pub fn recv_offer(conn: &mut dyn Connection) -> Result<Offer, ProtoError> {
    let (_, body) = expect(conn, &[tag::OFFER])?;
    decode_offer(&body)
}

pub fn decode_offer(body: &[u8]) -> Result<Offer, ProtoError> {
    let mut r = Reader::new(body);
    let m = get_bytes(&mut r)
        .map_err(|_| ProtoError::Malformed("manifest bytes"))?
        .to_vec();
    let id: BlobId = r.array().map_err(|_| ProtoError::Malformed("agreed id"))?;
    let root: BlobId = r
        .array()
        .map_err(|_| ProtoError::Malformed("agreed root"))?;
    Ok(Offer {
        manifest_bytes: m,
        agreed_manifest_id: id,
        agreed_root_tree_id: root,
    })
}

/// `(blob_kind, id)` pairs wanted as individual meta blobs.
pub fn send_req_meta(
    conn: &mut dyn Connection,
    ids: &[(BlobKind, BlobId)],
) -> Result<(), ProtoError> {
    let mut b = Vec::new();
    put_u32(&mut b, ids.len() as u32);
    for (k, id) in ids {
        put_u8(&mut b, k.to_u8());
        put_bytes(&mut b, id);
    }
    send(conn, tag::REQ_META, &b)
}

pub fn recv_req_meta(conn: &mut dyn Connection) -> Result<Vec<(BlobKind, BlobId)>, ProtoError> {
    let (_, body) = expect(conn, &[tag::REQ_META])?;
    decode_req_meta(&body)
}

pub fn decode_req_meta(body: &[u8]) -> Result<Vec<(BlobKind, BlobId)>, ProtoError> {
    let mut r = Reader::new(body);
    let n = r.u32().map_err(|_| ProtoError::Malformed("count"))? as usize;
    let mut out = Vec::with_capacity(n.min(1 << 20));
    for _ in 0..n {
        let k = BlobKind::from_u8(r.u8().map_err(|_| ProtoError::Malformed("kind"))?)
            .ok_or(ProtoError::Malformed("kind value"))?;
        let id: BlobId = r.array().map_err(|_| ProtoError::Malformed("id"))?;
        out.push((k, id));
    }
    Ok(out)
}

pub fn send_req_data(conn: &mut dyn Connection, chunk_ids: &[BlobId]) -> Result<(), ProtoError> {
    let mut b = Vec::new();
    put_u32(&mut b, chunk_ids.len() as u32);
    for id in chunk_ids {
        put_bytes(&mut b, id);
    }
    send(conn, tag::REQ_DATA, &b)
}

pub fn recv_req_data(conn: &mut dyn Connection) -> Result<Vec<BlobId>, ProtoError> {
    let (_, body) = expect(conn, &[tag::REQ_DATA])?;
    decode_req_data(&body)
}

pub fn decode_req_data(body: &[u8]) -> Result<Vec<BlobId>, ProtoError> {
    let mut r = Reader::new(body);
    let n = r.u32().map_err(|_| ProtoError::Malformed("count"))? as usize;
    let mut out = Vec::with_capacity(n.min(1 << 20));
    for _ in 0..n {
        out.push(r.array().map_err(|_| ProtoError::Malformed("chunk id"))?);
    }
    Ok(out)
}

pub fn send_item(conn: &mut dyn Connection, item: &ItemPayload) -> Result<(), ProtoError> {
    let mut b = Vec::new();
    match item {
        ItemPayload::Pack { name, bytes } => {
            put_u8(&mut b, 1);
            put_bytes(&mut b, name);
            put_len_bytes(&mut b, bytes);
        }
        ItemPayload::Blob { kind, id, bytes } => {
            put_u8(&mut b, 2);
            put_u8(&mut b, kind.to_u8());
            put_bytes(&mut b, id);
            put_len_bytes(&mut b, bytes);
        }
    }
    send(conn, tag::ITEM, &b)
}

pub fn recv_item(conn: &mut dyn Connection) -> Result<ItemPayload, ProtoError> {
    let (_, body) = expect(conn, &[tag::ITEM])?;
    decode_item(&body)
}

pub fn decode_item(body: &[u8]) -> Result<ItemPayload, ProtoError> {
    let mut r = Reader::new(body);
    match r.u8().map_err(|_| ProtoError::Malformed("item kind"))? {
        1 => {
            let name: PackId = r.array().map_err(|_| ProtoError::Malformed("pack name"))?;
            let bytes = get_bytes(&mut r)
                .map_err(|_| ProtoError::Malformed("pack bytes"))?
                .to_vec();
            Ok(ItemPayload::Pack { name, bytes })
        }
        2 => {
            let kind = BlobKind::from_u8(r.u8().map_err(|_| ProtoError::Malformed("kind"))?)
                .ok_or(ProtoError::Malformed("kind value"))?;
            let id: BlobId = r.array().map_err(|_| ProtoError::Malformed("blob id"))?;
            let bytes = get_bytes(&mut r)
                .map_err(|_| ProtoError::Malformed("blob bytes"))?
                .to_vec();
            Ok(ItemPayload::Blob { kind, id, bytes })
        }
        _ => Err(ProtoError::Malformed("item kind value")),
    }
}

/// One step of an item stream: an item, or the terminator.
pub enum ItemStream {
    Item(ItemPayload),
    Done,
}

/// Puller-side stream read: accepts ITEM frames and the ITEMS_DONE
/// terminator; anything else (ERROR included) is a protocol error.
pub fn recv_item_stream(conn: &mut dyn Connection) -> Result<ItemStream, ProtoError> {
    let frame = conn.recv_frame()?;
    let t = *frame.first().ok_or(ProtoError::Malformed("empty frame"))?;
    match t {
        tag::ITEM => Ok(ItemStream::Item(decode_item(&frame[1..])?)),
        tag::ITEMS_DONE => Ok(ItemStream::Done),
        tag::ERROR => Err(ProtoError::PeerError(parse_error_text(&frame[1..]))),
        other => Err(ProtoError::BadTag(other)),
    }
}

pub fn send_items_done(conn: &mut dyn Connection) -> Result<(), ProtoError> {
    send(conn, tag::ITEMS_DONE, &[])
}

pub fn recv_items_done(conn: &mut dyn Connection) -> Result<(), ProtoError> {
    expect(conn, &[tag::ITEMS_DONE])?;
    Ok(())
}

pub fn send_agreed(conn: &mut dyn Connection, manifest_id: BlobId) -> Result<(), ProtoError> {
    let mut b = Vec::new();
    put_bytes(&mut b, &manifest_id);
    send(conn, tag::AGREED, &b)
}

pub fn recv_agreed(conn: &mut dyn Connection) -> Result<BlobId, ProtoError> {
    let (_, body) = expect(conn, &[tag::AGREED])?;
    decode_agreed(&body)
}

pub fn decode_agreed(body: &[u8]) -> Result<BlobId, ProtoError> {
    let mut r = Reader::new(body);
    r.array().map_err(|_| ProtoError::Malformed("agreed id"))
}

pub fn send_error(conn: &mut dyn Connection, text: &str) {
    let mut b = Vec::new();
    put_len_bytes(&mut b, text.as_bytes());
    // Best effort: the failing peer may already be gone.
    let _ = send(conn, tag::ERROR, &b);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{TcpTransport, Transport};

    /// Loopback pipe over real TCP for codec round-trips.
    fn pipe() -> (Box<dyn Connection>, Box<dyn Connection>) {
        let lst = TcpTransport.listen("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = lst.local_addr().unwrap();
        let server = std::thread::spawn(move || lst.accept().unwrap());
        let client = TcpTransport.dial(addr).unwrap();
        (client, server.join().unwrap())
    }

    #[test]
    fn hello_offer_round_trip() {
        let (mut a, mut b) = pipe();
        send_hello(&mut a, "node-x").unwrap();
        assert_eq!(recv_hello(&mut b).unwrap().device_tag, "node-x");

        send_offer(
            &mut a,
            &Offer {
                manifest_bytes: vec![1, 2, 3, 4],
                agreed_manifest_id: [7; 32],
                agreed_root_tree_id: [9; 32],
            },
        )
        .unwrap();
        let off = recv_offer(&mut b).unwrap();
        assert_eq!(off.manifest_bytes, vec![1, 2, 3, 4]);
        assert_eq!(off.agreed_manifest_id, [7; 32]);
        assert_eq!(off.agreed_root_tree_id, [9; 32]);
    }

    #[test]
    fn requests_items_and_agreement_round_trip() {
        let (mut w, mut p) = pipe();

        send_req_meta(
            &mut p,
            &[(BlobKind::TreeNode, [1; 32]), (BlobKind::Manifest, [2; 32])],
        )
        .unwrap();
        let got = recv_req_meta(&mut w).unwrap();
        assert_eq!(
            got,
            vec![(BlobKind::TreeNode, [1; 32]), (BlobKind::Manifest, [2; 32])]
        );

        send_item(
            &mut w,
            &ItemPayload::Pack {
                name: [3; 32],
                bytes: vec![9; 100],
            },
        )
        .unwrap();
        send_item(
            &mut w,
            &ItemPayload::Blob {
                kind: BlobKind::TreeNode,
                id: [1; 32],
                bytes: vec![5],
            },
        )
        .unwrap();
        send_items_done(&mut w).unwrap();

        match recv_item(&mut p).unwrap() {
            ItemPayload::Pack { name, bytes } => {
                assert_eq!(name, [3; 32]);
                assert_eq!(bytes.len(), 100);
            }
            other => panic!("{other:?}"),
        }
        match recv_item(&mut p).unwrap() {
            ItemPayload::Blob { kind, id, bytes } => {
                assert_eq!(kind, BlobKind::TreeNode);
                assert_eq!(id, [1; 32]);
                assert_eq!(bytes, vec![5]);
            }
            other => panic!("{other:?}"),
        }
        recv_items_done(&mut p).unwrap();

        send_agreed(&mut p, [42; 32]).unwrap();
        assert_eq!(recv_agreed(&mut w).unwrap(), [42; 32]);
    }

    #[test]
    fn wrong_tag_is_a_protocol_error_not_a_hang() {
        let (mut a, mut b) = pipe();
        send_hello(&mut a, "t").unwrap();
        let err = recv_offer(&mut b).unwrap_err();
        assert!(matches!(err, ProtoError::BadTag(tag::HELLO)));
    }

    #[test]
    fn error_frame_surfaces_peer_text() {
        let (mut a, mut b) = pipe();
        send_error(&mut a, "boom on purpose");
        let err = recv_hello(&mut b).unwrap_err();
        assert!(
            matches!(err, ProtoError::PeerError(ref s) if s == "boom on purpose"),
            "{err}"
        );
    }

    #[test]
    fn tags_are_constrained() {
        assert!(validate_tag("dev-01").is_ok());
        assert!(validate_tag("").is_err());
        assert!(validate_tag("../etc").is_err());
        assert!(validate_tag(&"x".repeat(65)).is_err());
    }

    #[test]
    fn empty_data_request_round_trips() {
        let (mut a, mut b) = pipe();
        send_req_data(&mut a, &[]).unwrap();
        assert!(recv_req_data(&mut b).unwrap().is_empty());
    }
}
