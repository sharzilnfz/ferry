//! Protocol v1 session driver: link adaptation, handshake, sealed frames.
//!
//! This is ferry-sync's half of `docs/store-format.md` §"Wire protocol v1"
//! (reference implementation `crates/ferry-proto`). The M0 throwaway message
//! set is replaced by the v1 inventory; the crypto below transcribes the
//! normative handshake byte for byte and is proven compatible with the
//! reference engine by interop tests (`tests/protocol_v1.rs` runs THIS code
//! against `ferry_proto::run_engine` over real TCP, encrypted, in both role
//! assignments).
//!
//! Layers, bottom-up:
//!
//! - [`Link`] — one frame body region per call. [`ConnLink`] rides the
//!   existing `Transport` seam 1:1 (the transport's own length prefix
//!   represents the spec's u32 BE prefix); [`RawLink`] speaks the literal
//!   wire framing over any `Read + Write` stream, which is what reference-
//!   interop and direct-TCP tests use.
//! - [`DirectionCipher`] — ChaCha20-Poly1305 per direction, nonce
//!   `"FPN1" || u64 BE seq`, AAD = u32 BE body length. One failed open
//!   consumes the counter slot; any tag failure kills the session.
//! - [`establish`] — HELLO / HELLO_ACK / AUTH_INIT / AUTH_CONFIRM with
//!   device-key mutual auth (possession proofs, no signatures) and version
//!   negotiation. Peer identity policy: strict pin or trust-on-first-use
//!   ([`ExpectPeer`]); TOFU is a LOCAL policy only — on the wire both modes
//!   are byte-identical, because possession of the claimed static secret is
//!   always proven before any folder state moves.

use std::io::{self, Read, Write};

use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Key, Nonce,
};
use hkdf::Hkdf;
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

use ferry_crypto::identity::{DeviceId, DeviceIdentity};
use ferry_proto::codec::{self, AuthProof, Bye, FrameBody, Hello, HelloAck};
use ferry_proto::error::{ByeReason, ProtoError};
use ferry_proto::secure::{
    transcript_hash, INFO_HANDSHAKE, INFO_HTK_I2R, INFO_HTK_R2I, INFO_TK_I2R, INFO_TK_R2I, KEY_LEN,
};
use ferry_proto::version::{negotiate, ProtocolVersion};

/// Hard cap on one frame's body region (normative v1 limit).
pub const MAX_FRAME_BODY: usize = ferry_proto::frame::MAX_FRAME_BODY;

const NONCE_LEN: usize = 12;
/// Traffic-nonce prefix "FPN1".
const TRAFFIC_NONCE_PREFIX: [u8; 4] = *b"FPN1";

// --- link layer ---------------------------------------------------------------

/// One frame-body region per call: exactly what sits between the spec's
/// length prefix and the next frame — plaintext pre-auth, ciphertext after.
///
/// `Send` because sessions run each side on its own thread (lockstep
/// conversation), and the trait object forms cross thread boundaries in the
/// engine's session handlers.
pub trait Link: Send {
    fn send_body(&mut self, body: &[u8]) -> Result<(), ProtoError>;
    fn recv_body(&mut self) -> Result<Vec<u8>, ProtoError>;
}

/// Ride the existing M0 transport seam. The connection already delivers
/// exact-length frames, so the spec's u32 BE prefix is represented by the
/// transport's own framing rather than duplicated on the wire; the AEAD
/// length binding still holds because sender and receiver derive the same
/// `body_len` from the frame they handle.
pub struct ConnLink<'a>(pub &'a mut dyn crate::transport::Connection);

impl Link for ConnLink<'_> {
    fn send_body(&mut self, body: &[u8]) -> Result<(), ProtoError> {
        self.0.send_frame(body)?;
        Ok(())
    }

    fn recv_body(&mut self) -> Result<Vec<u8>, ProtoError> {
        Ok(self.0.recv_frame()?)
    }
}

/// Literal wire framing over any byte stream (u32 BIG-ENDIAN prefix), used
/// against raw streams — reference-engine interop tests and direct TCP.
pub struct RawLink<S>(pub S);

impl<S: Read + Write + Send> Link for RawLink<S> {
    fn send_body(&mut self, body: &[u8]) -> Result<(), ProtoError> {
        // Assembled and written as ONE buffer: the spec forbids splitting a
        // frame across writes at this layer.
        let mut frame = Vec::with_capacity(4 + body.len());
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.extend_from_slice(body);
        self.0.write_all(&frame)?;
        self.0.flush()?;
        Ok(())
    }

    fn recv_body(&mut self) -> Result<Vec<u8>, ProtoError> {
        let mut prefix = [0u8; 4];
        match self.0.read_exact(&mut prefix) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(ProtoError::Io(e)),
            Err(e) => return Err(ProtoError::Io(e)),
        }
        let len = u32::from_be_bytes(prefix) as usize;
        if len > MAX_FRAME_BODY {
            return Err(ProtoError::FrameTooLarge {
                len,
                max: MAX_FRAME_BODY,
            });
        }
        let mut body = vec![0u8; len];
        self.0.read_exact(&mut body)?;
        Ok(body)
    }
}

/// The full wire image of a body region: u32 BE prefix || body. Handshake
/// transcript hashes cover exactly these bytes.
fn wire_image(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(body);
    out
}

// --- AEAD ---------------------------------------------------------------------

/// One direction's sealing state: traffic key + strictly increasing counter.
/// Byte-compatible with `ferry_proto::secure::SessionCipher` (proven by
/// interop tests).
pub struct DirectionCipher {
    key: Zeroizing<[u8; KEY_LEN]>,
    seq: u64,
}

impl DirectionCipher {
    pub fn new(key: [u8; KEY_LEN]) -> Self {
        DirectionCipher {
            key: Zeroizing::new(key),
            seq: 0,
        }
    }

    /// Take the next nonce, consuming the counter slot EVEN IF the operation
    /// later fails — a failed open burns the slot by design (no resync).
    fn next_nonce(&mut self) -> Result<[u8; NONCE_LEN], ProtoError> {
        if self.seq == u64::MAX {
            return Err(ProtoError::CounterExhausted);
        }
        let mut n = [0u8; NONCE_LEN];
        n[..4].copy_from_slice(&TRAFFIC_NONCE_PREFIX);
        n[4..].copy_from_slice(&self.seq.to_be_bytes());
        self.seq += 1;
        Ok(n)
    }

    fn cipher(&self) -> ChaCha20Poly1305 {
        ChaCha20Poly1305::new(Key::from_slice(self.key.as_ref()))
    }

    /// Seal one body region, binding its wire-visible length as AAD.
    pub fn seal(&mut self, len_prefix: u32, body: &[u8]) -> Result<Vec<u8>, ProtoError> {
        let nonce = self.next_nonce()?;
        self.cipher()
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: body,
                    aad: &len_prefix.to_be_bytes(),
                },
            )
            .map_err(|_| ProtoError::ProtocolViolation("frame seal failure"))
    }

    /// Open one sealed body region. Any tamper, reorder, splice, or replay
    /// fails here.
    pub fn open(&mut self, len_prefix: u32, ct: &[u8]) -> Result<Vec<u8>, ProtoError> {
        if ct.len() < 16 {
            return Err(ProtoError::ProtocolViolation("sealed body too short"));
        }
        let nonce = self.next_nonce()?;
        self.cipher()
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: ct,
                    aad: &len_prefix.to_be_bytes(),
                },
            )
            .map_err(|_| ProtoError::Auth("post-auth frame failed tag verification"))
    }
}

// --- handshake key schedule (transcription of the normative section) ----------

fn expand_from(prk: &[u8], info: &[u8]) -> Zeroizing<[u8; KEY_LEN]> {
    let hk = Hkdf::<Sha256>::from_prk(prk).expect("prk is a valid SHA-256 length");
    let mut okm = Zeroizing::new([0u8; KEY_LEN]);
    hk.expand(info, okm.as_mut())
        .expect("32-byte OKM always valid");
    okm
}

/// `(htk_i2r, htk_r2i, prk)` — all zeroizing-on-drop key material.
type HandshakeKeys = (
    Zeroizing<[u8; KEY_LEN]>,
    Zeroizing<[u8; KEY_LEN]>,
    Box<[u8; KEY_LEN]>,
);

/// Stage 1: `(htk_i2r, htk_r2i, prk)` from the three DH terms under the
/// hello transcript hash.
fn kdf_handshake(th: &[u8; 32], e1: &[u8; 32], m1: &[u8; 32], m2: &[u8; 32]) -> HandshakeKeys {
    let mut ikm = Zeroizing::new([0u8; 96]);
    ikm[..32].copy_from_slice(e1);
    ikm[32..64].copy_from_slice(m1);
    ikm[64..].copy_from_slice(m2);
    let ext = Hkdf::<Sha256>::new(Some(th), ikm.as_ref());
    let mut prk = Box::new([0u8; KEY_LEN]);
    ext.expand(INFO_HANDSHAKE, prk.as_mut())
        .expect("valid prk length");
    let htk_i2r = expand_from(prk.as_slice(), INFO_HTK_I2R);
    let htk_r2i = expand_from(prk.as_slice(), INFO_HTK_R2I);
    (htk_i2r, htk_r2i, prk)
}

/// Stage 2: per-direction traffic keys re-rooted on the final transcript.
///
/// NOTE: matches the REFERENCE IMPLEMENTATION (`ferry_proto::secure`), which
/// routes through an intermediate `"ferry/v1/traffic"` expand between the
/// extract and the per-direction labels — one stage more than the doc's
/// sketch. Interop, not prose, is authoritative here.
fn traffic_keys(prk: &[u8; KEY_LEN], th_final: &[u8; 32]) -> (DirectionCipher, DirectionCipher) {
    let ext = Hkdf::<Sha256>::new(Some(th_final), prk);
    let mut root = Zeroizing::new([0u8; KEY_LEN]);
    ext.expand(b"ferry/v1/traffic", root.as_mut())
        .expect("valid root length");
    let tk_i2r = expand_from(root.as_slice(), INFO_TK_I2R);
    let tk_r2i = expand_from(root.as_slice(), INFO_TK_R2I);
    let mut i2r = [0u8; KEY_LEN];
    let mut r2i = [0u8; KEY_LEN];
    i2r.copy_from_slice(tk_i2r.as_ref());
    r2i.copy_from_slice(tk_r2i.as_ref());
    (DirectionCipher::new(i2r), DirectionCipher::new(r2i))
}

/// Seal this side's possession proof: its OWN device_id under its
/// direction's auth key, AAD = hello transcript hash, fixed zero nonce.
fn seal_auth(
    key: &[u8; KEY_LEN],
    th: &[u8; 32],
    device_id: &DeviceId,
) -> Result<AuthProof, ProtoError> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let nonce = [0u8; NONCE_LEN];
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: device_id,
                aad: th,
            },
        )
        .map_err(|_| ProtoError::Auth("seal failure"))?;
    AuthProof::new(ct)
}

/// Open the peer's possession proof. Tag failure == they do not hold the
/// static secret they claim.
fn open_auth(
    key: &[u8; KEY_LEN],
    th: &[u8; 32],
    proof: &AuthProof,
) -> Result<DeviceId, ProtoError> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let nonce = [0u8; NONCE_LEN];
    let pt = cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &proof.ciphertext,
                aad: th,
            },
        )
        .map_err(|_| ProtoError::Auth("auth tag verification failed"))?;
    pt.try_into()
        .map_err(|_| ProtoError::Auth("auth plaintext wrong length"))
}

// --- peer identity policy -------------------------------------------------------

/// Who we accept as the other side. `Pin` fails the session on any other
/// authenticated identity; `TrustOnFirstUse` accepts whichever identity
/// proves possession and reports it so the caller can pin it for next time.
/// On the WIRE the two are indistinguishable — this is local acceptance
/// policy, not protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpectPeer {
    Pin(DeviceId),
    TrustOnFirstUse,
}

fn check_identity(
    got: DeviceId,
    expect: &ExpectPeer,
    seen_first: Option<DeviceId>,
) -> Result<DeviceId, ProtoError> {
    match expect {
        ExpectPeer::Pin(want) => {
            if got == *want {
                Ok(got)
            } else {
                Err(ProtoError::IdentityMismatch {
                    expected: ferry_store::format::hex(want),
                    got: ferry_store::format::hex(&got),
                })
            }
        }
        ExpectPeer::TrustOnFirstUse => match seen_first {
            Some(first) if first != got => Err(ProtoError::IdentityMismatch {
                expected: ferry_store::format::hex(&first),
                got: ferry_store::format::hex(&got),
            }),
            _ => Ok(got),
        },
    }
}

// --- established session ---------------------------------------------------------

/// Everything one successful handshake produced.
pub struct Established<'a> {
    pub io: SessionIo<'a>,
    pub agreed_version: ProtocolVersion,
    /// The authenticated peer identity (possession-proven).
    pub peer: DeviceId,
    pub peer_max: ProtocolVersion,
    pub peer_flags: u64,
    /// Whether post-auth frames are sealed (handshake auth ALWAYS ran).
    pub encrypted: bool,
}

/// Framed, (optionally) sealed message IO over a [`Link`].
pub struct SessionIo<'a> {
    link: &'a mut dyn Link,
    version: ProtocolVersion,
    tx: Option<DirectionCipher>,
    rx: Option<DirectionCipher>,
    peer_max: ProtocolVersion,
    peer_flags: u64,
}

impl<'a> SessionIo<'a> {
    /// Send one message. Sealed iff the handshake negotiated sealing.
    pub fn send_frame(&mut self, msg_type: u8, payload: Vec<u8>) -> Result<(), ProtoError> {
        let body = FrameBody::new(msg_type, self.version, payload).encode();
        match self.tx.as_mut() {
            Some(c) => {
                let len = (body.len() + 16) as u32;
                let ct = c.seal(len, &body)?;
                self.link.send_body(&ct)
            }
            None => self.link.send_body(&body),
        }
    }

    /// Receive one message, enforcing the unknown-message-type rule:
    /// unknown types post-auth are skipped iff the peer advertised a higher
    /// minor within our major AND carries feature flags we do not know;
    /// otherwise the session dies with [`ProtoError::UnknownMessage`].
    pub fn recv_frame(&mut self) -> Result<Option<FrameBody>, ProtoError> {
        loop {
            let raw = self.link.recv_body()?;
            let plain = match self.rx.as_mut() {
                Some(c) => c.open(raw.len() as u32, &raw)?,
                None => raw,
            };
            let fb = FrameBody::parse(&plain)?;
            if !codec::KNOWN_TYPES.contains(&fb.msg_type) {
                let higher = self.peer_max.major() == ProtocolVersion::V1_0.major()
                    && self.peer_max.minor() > ProtocolVersion::V1_0.minor();
                let flagged = (self.peer_flags & !codec::FLAG_EXTENSION_AWARE) != 0;
                if higher && flagged {
                    continue; // skip-if-flagged
                }
                return Err(ProtoError::UnknownMessage {
                    msg_type: fb.msg_type,
                });
            }
            return Ok(Some(fb));
        }
    }

    pub fn expect_frame(&mut self, msg_type: u8) -> Result<FrameBody, ProtoError> {
        match self.recv_frame()? {
            Some(fb) if fb.msg_type == msg_type => Ok(fb),
            Some(_) => Err(ProtoError::ProtocolViolation(
                "unexpected message in this state",
            )),
            None => unreachable!("recv_frame loops until a known type or error"),
        }
    }

    /// Like [`expect_frame`] but accepting any of `msg_types` (e.g. a
    /// PACK_ITEM-or-terminator response sequence).
    pub fn expect_frame_any(&mut self, msg_types: &[u8]) -> Result<FrameBody, ProtoError> {
        match self.recv_frame()? {
            Some(fb) if msg_types.contains(&fb.msg_type) => Ok(fb),
            Some(_) => Err(ProtoError::ProtocolViolation(
                "unexpected message in this state",
            )),
            None => unreachable!("recv_frame loops until a known type or error"),
        }
    }

    /// Receive BYE; Normal closes cleanly, anything else surfaces typed.
    pub fn recv_bye(&mut self) -> Result<(), ProtoError> {
        let fb = self.expect_frame(codec::MSG_BYE)?;
        let bye = Bye::parse(&fb.payload)?;
        match bye.reason {
            ByeReason::Normal => Ok(()),
            other => Err(ProtoError::ByeReceived { reason: other }),
        }
    }

    /// Send BYE with a reason (last frame on errors and clean ends alike).
    pub fn send_bye(&mut self, reason: ByeReason) -> Result<(), ProtoError> {
        self.send_frame(codec::MSG_BYE, Bye { reason }.encode())
    }

    /// Best-effort BYE carrying the coarse reason for a typed error.
    pub fn bye_for_error(&mut self, err: &ProtoError) {
        let reason = match err {
            ProtoError::VersionIncompatible { .. } => ByeReason::VersionIncompatible,
            ProtoError::ProtocolViolation(_) | ProtoError::UnknownMessage { .. } => {
                ByeReason::ProtocolViolation
            }
            ProtoError::Auth(_) | ProtoError::IdentityMismatch { .. } => ByeReason::AuthFailed,
            ProtoError::FrameTooLarge { .. } | ProtoError::CounterExhausted => {
                ByeReason::ResourceLimit
            }
            _ => ByeReason::Internal,
        };
        let _ = self.send_bye(reason);
    }
}

impl core::fmt::Debug for Established<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Established")
            .field("agreed_version", &self.agreed_version)
            .field("peer", &ferry_store::format::hex(&self.peer))
            .field("encrypted", &self.encrypted)
            .finish_non_exhaustive()
    }
}

// --- the handshake -----------------------------------------------------------------

fn random32() -> [u8; 32] {
    let mut b = [0u8; 32];
    OsRng.fill_bytes(&mut b);
    b
}

/// Drive HELLO → HELLO_ACK → AUTH_INIT → AUTH_CONFIRM. Always authenticates
/// both devices, even when `encryption` is false (dev-only plaintext mode):
/// possession proofs are not optional. On any handshake failure the peer
/// receives a best-effort plaintext BYE with the coarse reason before the
/// typed error propagates.
pub fn establish<'a, L: Link>(
    link: &'a mut L,
    role: ferry_proto::Role,
    identity: &DeviceIdentity,
    expect: ExpectPeer,
    encryption: bool,
) -> Result<Established<'a>, ProtoError> {
    // Core returns lifetime-free data so a failed handshake can still use
    // `link` for the goodbye frame.
    match handshake_core(link, role, identity, expect, encryption) {
        Ok(hs) => Ok(Established {
            io: SessionIo {
                link,
                version: hs.agreed,
                tx: hs.tx,
                rx: hs.rx,
                peer_max: hs.peer_max,
                peer_flags: hs.peer_flags,
            },
            agreed_version: hs.agreed,
            peer: hs.peer,
            peer_max: hs.peer_max,
            peer_flags: hs.peer_flags,
            encrypted: encryption,
        }),
        Err(e) => {
            if !matches!(e, ProtoError::ByeReceived { .. } | ProtoError::Io(_)) {
                let reason = match e {
                    ProtoError::VersionIncompatible { .. } => ByeReason::VersionIncompatible,
                    ProtoError::Auth(_) | ProtoError::IdentityMismatch { .. } => {
                        ByeReason::AuthFailed
                    }
                    _ => ByeReason::ProtocolViolation,
                };
                let body = FrameBody::new(
                    codec::MSG_BYE,
                    ProtocolVersion::V1_0,
                    Bye { reason }.encode(),
                )
                .encode();
                let _ = link.send_body(&body);
            }
            Err(e)
        }
    }
}

struct HandshakeData {
    agreed: ProtocolVersion,
    peer: DeviceId,
    peer_max: ProtocolVersion,
    peer_flags: u64,
    tx: Option<DirectionCipher>,
    rx: Option<DirectionCipher>,
}

fn handshake_core<L: Link>(
    link: &mut L,
    role: ferry_proto::Role,
    identity: &DeviceIdentity,
    expect: ExpectPeer,
    encryption: bool,
) -> Result<HandshakeData, ProtoError> {
    let our_max = ProtocolVersion::V1_0;
    let flags = codec::FLAG_EXTENSION_AWARE;

    // StaticSecret (not EphemeralSecret): its diffie_hellman BORROWS, so one
    // fresh scalar feeds all three DH terms.
    let esk = StaticSecret::random_from_rng(OsRng);
    let my_epk = *PublicKey::from(&esk).as_bytes();
    let my_stat = *identity.device_id();
    let my_hello = Hello {
        version: our_max,
        flags,
        eph_pub: my_epk,
        stat_pub: my_stat,
        nonce: random32(),
    };

    // --- hellos (initiator speaks first) ---
    let (peer_hello_fb, hello_wires) = match role {
        ferry_proto::Role::Initiator => {
            let body = FrameBody::new(codec::MSG_HELLO, our_max, my_hello.encode()).encode();
            link.send_body(&body)?;
            let ack_body = link.recv_body()?;
            let image = wire_image(&ack_body);
            let fb = FrameBody::parse(&ack_body)?;
            if fb.msg_type != codec::MSG_HELLO_ACK {
                return Err(ProtoError::ProtocolViolation("expected HELLO_ACK"));
            }
            let ack = HelloAck::parse(&fb.payload)?;
            check_identity(ack.stat_pub, &expect, None)?;
            let expected_agreed = negotiate(our_max, ack.version)?;
            if ack.agreed != expected_agreed {
                return Err(ProtoError::ProtocolViolation(
                    "responder chose an invalid session version",
                ));
            }
            (fb, [wire_image(&body), image])
        }
        ferry_proto::Role::Responder => {
            let hello_body = link.recv_body()?;
            let image = wire_image(&hello_body);
            let fb = FrameBody::parse(&hello_body)?;
            if fb.msg_type != codec::MSG_HELLO {
                return Err(ProtoError::ProtocolViolation("expected HELLO"));
            }
            let hello = Hello::parse(&fb.payload)?;
            check_identity(hello.stat_pub, &expect, None)?;
            let agreed = negotiate(our_max, hello.version)?;
            let ack = HelloAck {
                version: our_max,
                agreed,
                flags,
                eph_pub: my_epk,
                stat_pub: my_stat,
                nonce: random32(),
            };
            let body = FrameBody::new(codec::MSG_HELLO_ACK, agreed, ack.encode()).encode();
            link.send_body(&body)?;
            (fb, [image, wire_image(&body)])
        }
    };

    let (peer_max, peer_flags, peer_eph, peer_stat, agreed) = match role {
        ferry_proto::Role::Initiator => {
            let ack = HelloAck::parse(&peer_hello_fb.payload)?;
            (
                ack.version,
                ack.flags,
                ack.eph_pub,
                ack.stat_pub,
                ack.agreed,
            )
        }
        ferry_proto::Role::Responder => {
            let h = Hello::parse(&peer_hello_fb.payload)?;
            (
                h.version,
                h.flags,
                h.eph_pub,
                h.stat_pub,
                negotiate(our_max, h.version)?,
            )
        }
    };

    let th_hello = transcript_hash(&[&hello_wires[0], &hello_wires[1]]);

    // --- three DH terms ---
    fn dh(esk: &StaticSecret, peer: [u8; 32]) -> Result<[u8; 32], ProtoError> {
        let shared = esk.diffie_hellman(&PublicKey::from(peer));
        if !shared.was_contributory() {
            return Err(ProtoError::Auth("degenerate DH output"));
        }
        Ok(*shared.as_bytes())
    }
    let e1 = dh(&esk, peer_eph)?;
    // m1 authenticates the INITIATOR's static key, m2 the RESPONDER's.
    let (m1, m2): ([u8; 32], [u8; 32]) = match role {
        ferry_proto::Role::Initiator => (
            *identity
                .diffie_hellman(&peer_eph)
                .map_err(|_| ProtoError::Auth("degenerate peer static key"))?,
            dh(&esk, peer_stat)?,
        ),
        ferry_proto::Role::Responder => (
            dh(&esk, peer_stat)?,
            *identity
                .diffie_hellman(&peer_eph)
                .map_err(|_| ProtoError::Auth("degenerate peer static key"))?,
        ),
    };

    let (htk_i2r, htk_r2i, prk) = kdf_handshake(&th_hello, &e1, &m1, &m2);

    // --- mutual proofs: initiator first, then responder ---
    let my_proof = match role {
        ferry_proto::Role::Initiator => seal_auth(&htk_i2r, &th_hello, &my_stat)?,
        ferry_proto::Role::Responder => seal_auth(&htk_r2i, &th_hello, &my_stat)?,
    };
    let (proof_wires, peer_proof_id) = match role {
        ferry_proto::Role::Initiator => {
            let init_body =
                FrameBody::new(codec::MSG_AUTH_INIT, agreed, my_proof.encode()).encode();
            link.send_body(&init_body)?;
            let conf_body = link.recv_body()?;
            let conf_image = wire_image(&conf_body);
            let fb = FrameBody::parse(&conf_body)?;
            if fb.msg_type != codec::MSG_AUTH_CONFIRM {
                return Err(ProtoError::ProtocolViolation("expected AUTH_CONFIRM"));
            }
            let proof = AuthProof::parse(&fb.payload)?;
            let got = open_auth(&htk_r2i, &th_hello, &proof)?;
            check_identity(got, &expect, None)?;
            (vec![wire_image(&init_body), conf_image], got)
        }
        ferry_proto::Role::Responder => {
            let init_body = link.recv_body()?;
            let init_image = wire_image(&init_body);
            let fb = FrameBody::parse(&init_body)?;
            if fb.msg_type != codec::MSG_AUTH_INIT {
                return Err(ProtoError::ProtocolViolation("expected AUTH_INIT"));
            }
            let proof = AuthProof::parse(&fb.payload)?;
            let got = open_auth(&htk_i2r, &th_hello, &proof)?;
            check_identity(got, &expect, None)?;
            let conf_body =
                FrameBody::new(codec::MSG_AUTH_CONFIRM, agreed, my_proof.encode()).encode();
            link.send_body(&conf_body)?;
            (vec![init_image, wire_image(&conf_body)], got)
        }
    };

    // Traffic keys re-root from the transcript that includes both proofs.
    let th_final = transcript_hash(&[
        &hello_wires[0],
        &hello_wires[1],
        &proof_wires[0],
        &proof_wires[1],
    ]);
    let (tk_i2r, tk_r2i) = traffic_keys(&prk, &th_final);

    let (tx, rx) = match role {
        ferry_proto::Role::Initiator => (tk_i2r, tk_r2i),
        ferry_proto::Role::Responder => (tk_r2i, tk_i2r),
    };
    let (tx, rx) = if encryption {
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    Ok(HandshakeData {
        agreed,
        peer: peer_proof_id,
        peer_max,
        peer_flags,
        tx,
        rx,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferry_proto::duplex_pair;
    use ferry_proto::stream::DuplexHalf;

    fn identity(seed: u8) -> DeviceIdentity {
        let mut sk = [0u8; 32];
        for (i, b) in sk.iter_mut().enumerate() {
            *b = seed.wrapping_mul(89).wrapping_add(i as u8 ^ 0x5a);
        }
        DeviceIdentity::from_secret_bytes(&sk)
    }

    /// Duplex halves wrapped in the literal wire framing.
    fn raw_pair() -> (RawLink<DuplexHalf>, RawLink<DuplexHalf>) {
        let (a, b) = duplex_pair();
        (RawLink(a), RawLink(b))
    }

    /// Handshake over duplex with strict pins both ways; runs each side on
    /// its own thread because the conversation is lockstep.
    fn pinned_pair<'a, 'b>(
        la: &'a mut RawLink<DuplexHalf>,
        lb: &'b mut RawLink<DuplexHalf>,
        id_a: &DeviceIdentity,
        id_b: &DeviceIdentity,
        encryption: bool,
    ) -> (Established<'a>, Established<'b>) {
        let (ra, rb) = std::thread::scope(|s| {
            let ha = s.spawn(|| {
                establish(
                    la,
                    ferry_proto::Role::Initiator,
                    id_a,
                    ExpectPeer::Pin(*id_b.device_id()),
                    encryption,
                )
            });
            let hb = s.spawn(|| {
                establish(
                    lb,
                    ferry_proto::Role::Responder,
                    id_b,
                    ExpectPeer::Pin(*id_a.device_id()),
                    encryption,
                )
            });
            (ha.join().unwrap(), hb.join().unwrap())
        });
        (ra.expect("handshake"), rb.expect("handshake"))
    }

    #[test]
    fn handshake_succeeds_and_seals_over_duplex() {
        let id_a = identity(10);
        let id_b = identity(20);
        let (mut la, mut lb) = raw_pair();
        let (mut ea, mut eb) = pinned_pair(&mut la, &mut lb, &id_a, &id_b, true);

        assert!(ea.encrypted && eb.encrypted);
        assert_eq!(ea.peer, *id_b.device_id());
        assert_eq!(eb.peer, *id_a.device_id());
        assert_eq!(ea.agreed_version, ProtocolVersion::V1_0);

        // Sealed round-trip: initiator's FOLDER_OFFER opens on responder.
        let offer = codec::FolderOffer {
            folder_id: [7; 16],
            manifest_id: [9; 32],
            reserved: 0,
        };
        ea.io
            .send_frame(codec::MSG_FOLDER_OFFER, offer.encode())
            .unwrap();
        let fb = eb.io.recv_frame().unwrap().unwrap();
        assert_eq!(fb.msg_type, codec::MSG_FOLDER_OFFER);
        let got = codec::FolderOffer::parse(&fb.payload).unwrap();
        assert_eq!(got.manifest_id, [9; 32]);

        // And back the other way on the responder's independent direction.
        eb.io
            .send_frame(
                codec::MSG_BYE,
                Bye {
                    reason: ByeReason::Normal,
                }
                .encode(),
            )
            .unwrap();
        let fb = ea.io.recv_frame().unwrap().unwrap();
        assert_eq!(fb.msg_type, codec::MSG_BYE);
    }

    #[test]
    fn handshake_without_session_sealing_still_authenticates() {
        let id_a = identity(11);
        let id_b = identity(22);
        let (mut la, mut lb) = raw_pair();
        std::thread::scope(|s| {
            // Dev mode (encryption=false): handshake + proofs still run; the
            // post-auth body travels UNSEALED, so magic and type are visible.
            let ha = s.spawn(|| {
                let mut est = establish(
                    &mut la,
                    ferry_proto::Role::Initiator,
                    &id_a,
                    ExpectPeer::Pin(*id_b.device_id()),
                    false,
                )
                .unwrap();
                assert!(!est.encrypted);
                est.io
                    .send_frame(codec::MSG_FOLDER_OFFER, vec![0xAB; 52])
                    .unwrap();
            });
            let hb = s.spawn(|| {
                let mut est = establish(
                    &mut lb,
                    ferry_proto::Role::Responder,
                    &id_b,
                    ExpectPeer::Pin(*id_a.device_id()),
                    false,
                )
                .unwrap();
                assert!(!est.encrypted);
                let raw = est.io.link_recv_raw_for_test();
                // The body region IS magic || type || version || payload
                // (the transport prefix is not part of the body).
                assert_eq!(&raw[..4], b"FRW1");
                assert_eq!(raw[4], codec::MSG_FOLDER_OFFER);
                let fb = FrameBody::parse(&raw).unwrap();
                assert_eq!(fb.payload.len(), 52);
            });
            ha.join().unwrap();
            hb.join().unwrap();
        });
    }

    #[test]
    fn wrong_pinned_peer_fails_cleanly_with_bye3() {
        let id_a = identity(33);
        let id_b = identity(44);
        let stranger = identity(55);
        let (mut la, mut lb) = raw_pair();
        std::thread::scope(|s| {
            let ha = s.spawn(|| {
                establish(
                    &mut la,
                    ferry_proto::Role::Initiator,
                    &id_a,
                    ExpectPeer::Pin(*stranger.device_id()),
                    true,
                )
            });
            let hb = s.spawn(|| {
                establish(
                    &mut lb,
                    ferry_proto::Role::Responder,
                    &id_b,
                    ExpectPeer::Pin(*id_a.device_id()),
                    true,
                )
            });
            let results = (ha.join().unwrap(), hb.join().unwrap());
            // The side whose PIN fails sees IdentityMismatch (after firing a
            // best-effort plaintext BYE(3)). The other side was mid-handshake
            // expecting AUTH_INIT: it consumes that BYE as an unexpected
            // pre-auth frame and dies with a protocol violation — also
            // clean, also immediate.
            let detecting = |err: &ProtoError| {
                matches!(
                    err,
                    ProtoError::IdentityMismatch { .. }
                        | ProtoError::ByeReceived {
                            reason: ByeReason::AuthFailed
                        }
                )
            };
            let surviving = |err: &ProtoError| {
                matches!(
                    err,
                    ProtoError::ProtocolViolation(_)
                        | ProtoError::ByeReceived {
                            reason: ByeReason::AuthFailed
                        }
                        | ProtoError::Io(_)
                )
            };
            let (ra, rb) = (results.0.unwrap_err(), results.1.unwrap_err());
            assert!(
                (detecting(&ra) && surviving(&rb)) || (detecting(&rb) && surviving(&ra)),
                "{ra} / {rb}"
            );
        });
    }

    #[test]
    fn version_major_mismatch_responder_side_fails_cleanly_with_bye1() {
        // A hostile "v2 engine": craft a HELLO advertising major 2 directly.
        let id_a = identity(66);
        let id_b = identity(77);
        let (mut la, mut lb) = raw_pair();
        let evil_hello = Hello {
            version: ProtocolVersion::new(2, 0),
            flags: codec::FLAG_EXTENSION_AWARE,
            eph_pub: [1; 32],
            stat_pub: *id_a.device_id(),
            nonce: [2; 32],
        };
        let body = FrameBody::new(
            codec::MSG_HELLO,
            ProtocolVersion::new(2, 0),
            evil_hello.encode(),
        )
        .encode();
        // Full literal frame: prefix || body.
        la.0.write_all(&wire_image(&body)).unwrap();

        let res = establish(
            &mut lb,
            ferry_proto::Role::Responder,
            &id_b,
            ExpectPeer::Pin(*id_a.device_id()),
            true,
        );
        let err = res.unwrap_err();
        assert!(
            matches!(
                err,
                ProtoError::VersionIncompatible { .. }
                    | ProtoError::ByeReceived {
                        reason: ByeReason::VersionIncompatible
                    }
            ),
            "{err}"
        );

        // The responder sent a plaintext BYE(1) before hanging up — it
        // travels toward the PEER, so read it from la's side. Bounded reads
        // (prefix + exact body): the duplex pipe only signals EOF when the
        // peer half drops, so read_to_end would block forever here.
        let mut prefix = [0u8; 4];
        la.0.read_exact(&mut prefix).unwrap();
        let body_len = u32::from_be_bytes(prefix) as usize;
        assert!(body_len >= 8, "expected a BYE frame, got {body_len} bytes");
        let mut bye = vec![0u8; body_len];
        la.0.read_exact(&mut bye).unwrap();
        assert_eq!(&bye[..4], b"FRW1");
        assert_eq!(bye[4], codec::MSG_BYE);
        assert_eq!(bye[7], ByeReason::VersionIncompatible as u8);
    }

    #[test]
    fn version_major_mismatch_initiator_side_fails_cleanly() {
        let id_a = identity(78);
        let id_b = identity(79);
        let peer_id = *id_b.device_id();
        let (mut la, mut lb) = raw_pair();
        std::thread::scope(|s| {
            // Fake v2 responder: read the HELLO, answer claiming major 2.
            let evil = s.spawn(move || {
                let mut buf = vec![0u8; 65536];
                let _ = la.0.read(&mut buf).unwrap_or(0);
                let ack = HelloAck {
                    version: ProtocolVersion::new(2, 0),
                    agreed: ProtocolVersion::V1_0,
                    flags: codec::FLAG_EXTENSION_AWARE,
                    eph_pub: [3; 32],
                    stat_pub: peer_id,
                    nonce: [4; 32],
                };
                let body = FrameBody::new(
                    codec::MSG_HELLO_ACK,
                    ProtocolVersion::new(2, 0),
                    ack.encode(),
                )
                .encode();
                la.0.write_all(&wire_image(&body)).unwrap();
            });
            let res = establish(
                &mut lb,
                ferry_proto::Role::Initiator,
                &id_a,
                ExpectPeer::Pin(peer_id),
                true,
            );
            let err = res.unwrap_err();
            assert!(
                matches!(
                    err,
                    ProtoError::VersionIncompatible { .. } | ProtoError::ProtocolViolation(_)
                ),
                "{err}"
            );
            evil.join().unwrap();
        });
    }

    #[test]
    fn tofu_accepts_and_reports_the_peer_identity() {
        let id_a = identity(88);
        let id_b = identity(99);
        let (mut la, mut lb) = raw_pair();
        std::thread::scope(|s| {
            let ha = s.spawn(|| {
                establish(
                    &mut la,
                    ferry_proto::Role::Initiator,
                    &id_a,
                    ExpectPeer::TrustOnFirstUse,
                    true,
                )
            });
            let hb = s.spawn(|| {
                establish(
                    &mut lb,
                    ferry_proto::Role::Responder,
                    &id_b,
                    ExpectPeer::TrustOnFirstUse,
                    true,
                )
            });
            let (ea, eb) = (ha.join().unwrap().unwrap(), hb.join().unwrap().unwrap());
            assert_eq!(ea.peer, *id_b.device_id(), "TOFU reports who showed up");
            assert_eq!(eb.peer, *id_a.device_id());
        });
    }

    #[test]
    fn tampered_post_auth_frame_is_fatal_never_silent() {
        let id_a = identity(111);
        let id_b = identity(222);
        let (mut la, mut lb) = raw_pair();
        std::thread::scope(|s| {
            let ha = s.spawn(move || {
                let mut est = establish(
                    &mut la,
                    ferry_proto::Role::Initiator,
                    &id_a,
                    ExpectPeer::TrustOnFirstUse,
                    true,
                )
                .unwrap();
                // Seal an honest frame, flip one ciphertext byte in flight.
                let payload = codec::FolderOffer {
                    folder_id: [1; 16],
                    manifest_id: [2; 32],
                    reserved: 0,
                }
                .encode();
                let body = FrameBody::new(codec::MSG_FOLDER_OFFER, ProtocolVersion::V1_0, payload)
                    .encode();
                let len = (body.len() + 16) as u32;
                let mut ct = est.io.seal_for_test(len, &body).unwrap();
                let last = ct.len() - 3;
                ct[last] ^= 0x01;
                la.0.write_all(&(ct.len() as u32).to_be_bytes()).unwrap();
                la.0.write_all(&ct).unwrap();
            });
            let hb = s.spawn(move || {
                let mut est = establish(
                    &mut lb,
                    ferry_proto::Role::Responder,
                    &id_b,
                    ExpectPeer::TrustOnFirstUse,
                    true,
                )
                .unwrap();
                let err = est.io.recv_frame().unwrap_err();
                assert!(matches!(err, ProtoError::Auth(_)), "{err}");
            });
            ha.join().unwrap();
            hb.join().unwrap();
        });
    }

    impl<'a> SessionIo<'a> {
        /// Test hook: seal without sending.
        fn seal_for_test(&mut self, len: u32, body: &[u8]) -> Result<Vec<u8>, ProtoError> {
            self.tx.as_mut().unwrap().seal(len, body)
        }
        /// Test hook: one raw inbound body region without parsing.
        fn link_recv_raw_for_test(&mut self) -> Vec<u8> {
            self.link.recv_body().unwrap_or_default()
        }
    }
}
