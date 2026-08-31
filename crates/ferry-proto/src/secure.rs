use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Key, Nonce,
};
use hkdf::Hkdf;
use rand::rngs::OsRng;
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

use ferry_crypto::identity::{DeviceId, DeviceIdentity};
use ferry_store::format::hex;

use crate::codec::{self, AuthProof, Bye, FrameBody, Hello, HelloAck, FLAG_EXTENSION_AWARE};
use crate::engine::Role;
use crate::error::{ByeReason, ProtoError};
use crate::stream::ByteStream;
use crate::version::{negotiate, ProtocolVersion};

pub const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;

pub const INFO_HANDSHAKE: &[u8] = b"ferry/v1/handshake";
pub const INFO_HTK_I2R: &[u8] = b"ferry/v1/htk/i2r";
pub const INFO_HTK_R2I: &[u8] = b"ferry/v1/htk/r2i";
pub const INFO_TK_I2R: &[u8] = b"ferry/v1/tk/i2r";
pub const INFO_TK_R2I: &[u8] = b"ferry/v1/tk/r2i";

const TRAFFIC_NONCE_PREFIX: [u8; 4] = *b"FPN1";

pub fn transcript_hash(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    for p in parts {
        h.update(&(p.len() as u32).to_be_bytes());
        h.update(p);
    }
    *h.finalize().as_bytes()
}

fn expand_from(prk: &[u8], info: &[u8]) -> Zeroizing<[u8; KEY_LEN]> {
    let hk = Hkdf::<Sha256>::from_prk(prk).expect("prk is a valid SHA-256 length");
    let mut okm = Zeroizing::new([0u8; KEY_LEN]);
    hk.expand(info, okm.as_mut())
        .expect("32-byte OKM always valid");
    okm
}

pub type HandshakeKeys = (
    Zeroizing<[u8; KEY_LEN]>,
    Zeroizing<[u8; KEY_LEN]>,
    Box<[u8; KEY_LEN]>,
);

pub fn kdf_handshake(
    transcript: &[u8; 32],
    e1: &[u8; 32],
    m1: &[u8; 32],
    m2: &[u8; 32],
) -> HandshakeKeys {
    let mut ikm = Zeroizing::new([0u8; 96]);
    ikm[..32].copy_from_slice(e1);
    ikm[32..64].copy_from_slice(m1);
    ikm[64..].copy_from_slice(m2);
    let ext = Hkdf::<Sha256>::new(Some(transcript), ikm.as_ref());
    let mut prk_box = Box::new([0u8; KEY_LEN]);
    ext.expand(INFO_HANDSHAKE, prk_box.as_mut())
        .expect("valid prk length");
    let htk_i2r = expand_from(prk_box.as_slice(), INFO_HTK_I2R);
    let htk_r2i = expand_from(prk_box.as_slice(), INFO_HTK_R2I);
    (htk_i2r, htk_r2i, prk_box)
}

#[derive(Debug)]
pub struct SessionKey(Zeroizing<[u8; KEY_LEN]>);

impl SessionKey {
    pub fn cipher(self) -> SessionCipher {
        SessionCipher::new(self)
    }

    pub fn from_bytes(key: [u8; KEY_LEN]) -> Self {
        SessionKey(Zeroizing::new(key))
    }
}

pub fn traffic_keys(prk: &[u8; KEY_LEN], final_transcript: &[u8; 32]) -> (SessionKey, SessionKey) {
    let ext = Hkdf::<Sha256>::new(Some(final_transcript), prk);
    let mut root = Zeroizing::new([0u8; KEY_LEN]);
    ext.expand(b"ferry/v1/traffic", root.as_mut())
        .expect("valid root length");
    (
        SessionKey(expand_from(root.as_slice(), INFO_TK_I2R)),
        SessionKey(expand_from(root.as_slice(), INFO_TK_R2I)),
    )
}

pub(crate) fn seal_auth(
    key: &[u8; KEY_LEN],
    th: &[u8; 32],
    device_id: &[u8; 32],
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

pub(crate) fn open_auth(
    key: &[u8; KEY_LEN],
    th: &[u8; 32],
    proof: &AuthProof,
) -> Result<[u8; 32], ProtoError> {
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

pub struct SessionCipher {
    key: SessionKey,
    seq: u64,
}

impl SessionCipher {
    pub fn new(key: SessionKey) -> Self {
        SessionCipher { key, seq: 0 }
    }

    #[cfg(test)]
    pub(crate) fn at_sequence(key: SessionKey, seq: u64) -> Self {
        SessionCipher { key, seq }
    }

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

    pub fn seal_frame(
        &mut self,
        len_prefix: u32,
        body_region: &[u8],
    ) -> Result<Vec<u8>, ProtoError> {
        let nonce = self.next_nonce()?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(self.key.0.as_ref()));
        cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: body_region,
                    aad: &len_prefix.to_be_bytes(),
                },
            )
            .map_err(|_| ProtoError::ProtocolViolation("frame seal failure"))
    }

    pub fn open_frame(&mut self, len_prefix: u32, ct: &[u8]) -> Result<Vec<u8>, ProtoError> {
        if ct.len() < 16 {
            return Err(ProtoError::ProtocolViolation("sealed body too short"));
        }
        let nonce = self.next_nonce()?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(self.key.0.as_ref()));
        cipher
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

impl core::fmt::Debug for SessionCipher {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SessionCipher")
            .field("seq", &self.seq)
            .finish()
    }
}

fn full_wire(fb: &FrameBody) -> Vec<u8> {
    let body = fb.encode();
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

fn send_plain<S: ByteStream>(
    io: &mut S,
    msg_type: u8,
    version: ProtocolVersion,
    payload: &[u8],
) -> Result<(), ProtoError> {
    crate::frame::write_body(
        io,
        &full_wire(&FrameBody::new(msg_type, version, payload.to_vec())).as_slice()[4..],
    )
}

fn send_preauth<S: ByteStream>(
    io: &mut S,
    msg_type: u8,
    version: ProtocolVersion,
    payload: &[u8],
) -> Result<Vec<u8>, ProtoError> {
    let fb = FrameBody::new(msg_type, version, payload.to_vec());
    let wire = full_wire(&fb);
    crate::frame::write_body(io, &wire[4..])?;
    Ok(wire)
}

fn recv_preauth<S: ByteStream>(io: &mut S) -> Result<(FrameBody, Vec<u8>), ProtoError> {
    let body = crate::frame::read_body(io)?;
    let wire_len = (body.len() as u32).to_be_bytes();
    let mut wire = Vec::with_capacity(4 + body.len());
    wire.extend_from_slice(&wire_len);
    wire.extend_from_slice(&body);
    let fb = FrameBody::parse(&body)?;
    if fb.msg_type == codec::MSG_BYE {
        let bye = Bye::parse(&fb.payload)?;
        return Err(ProtoError::ByeReceived { reason: bye.reason });
    }
    Ok((fb, wire))
}

fn check_identity(got: DeviceId, expected: DeviceId) -> Result<(), ProtoError> {
    if got == expected {
        Ok(())
    } else {
        Err(ProtoError::IdentityMismatch {
            expected: hex(&expected),
            got: hex(&got),
        })
    }
}

fn unexpected(t: u8) -> ProtoError {
    let _ = t;
    ProtoError::ProtocolViolation("unexpected message in this state")
}

fn random32() -> [u8; 32] {
    use rand::RngCore;
    let mut b = [0u8; 32];
    OsRng.fill_bytes(&mut b);
    b
}

struct AuthWires {
    initiator: Vec<u8>,
    responder: Vec<u8>,
}

enum PeerHello {
    Init(Box<Hello>),
    Ack(Box<HelloAck>),
}

struct HelloWires {
    initiator: Vec<u8>,
    responder: Vec<u8>,
}

pub struct SecureSession<S: ByteStream> {
    io: S,
    version: ProtocolVersion,
    peer_max: ProtocolVersion,
    peer_flags: u64,
    peer_id: DeviceId,
    tx: Option<SessionCipher>,
    rx: Option<SessionCipher>,
}

impl<S: ByteStream> SecureSession<S> {
    pub fn establish(
        mut io: S,
        role: Role,
        identity: &DeviceIdentity,
        expected_peer: DeviceId,
        our_max: ProtocolVersion,
        encryption: bool,
    ) -> Result<Self, ProtoError> {
        let res =
            Self::handshake_internal(&mut io, role, identity, expected_peer, our_max, encryption);
        match res {
            Ok((agreed, peer_max, peer_flags, tx, rx)) => Ok(SecureSession {
                io,
                version: agreed,
                peer_max,
                peer_flags,
                peer_id: expected_peer,
                tx,
                rx,
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
                    let _ = send_plain(&mut io, codec::MSG_BYE, our_max, &[reason as u8]);
                }
                Err(e)
            }
        }
    }

    #[allow(clippy::type_complexity)]
    fn handshake_internal(
        io: &mut S,
        role: Role,
        identity: &DeviceIdentity,
        expected_peer: DeviceId,
        our_max: ProtocolVersion,
        encryption: bool,
    ) -> Result<
        (
            ProtocolVersion,
            ProtocolVersion,
            u64,
            Option<SessionCipher>,
            Option<SessionCipher>,
        ),
        ProtoError,
    > {
        let flags = FLAG_EXTENSION_AWARE;

        let esk = StaticSecret::random_from_rng(OsRng);
        let my_epk = *PublicKey::from(&esk).as_bytes();
        let my_nonce = random32();
        let my_stat = *identity.device_id();

        let my_hello = Hello {
            version: our_max,
            flags,
            eph_pub: my_epk,
            stat_pub: my_stat,
            nonce: my_nonce,
        };

        let (peer_hello, hello_wires) = match role {
            Role::Initiator => {
                let my_wire = send_preauth(io, codec::MSG_HELLO, our_max, &my_hello.encode())?;
                let (fb, _) = recv_preauth(io)?;
                if fb.msg_type != codec::MSG_HELLO_ACK {
                    return Err(ProtoError::ProtocolViolation("expected hello ack"));
                }
                let ack = HelloAck::parse(&fb.payload)?;

                check_identity(ack.stat_pub, expected_peer)?;
                let expected_agreed = negotiate(our_max, ack.version)?;
                if ack.agreed != expected_agreed {
                    return Err(ProtoError::ProtocolViolation(
                        "responder chose an invalid session version",
                    ));
                }
                (
                    PeerHello::Ack(Box::new(ack)),
                    HelloWires {
                        initiator: my_wire,
                        responder: full_wire(&fb),
                    },
                )
            }
            Role::Responder => {
                let (fb, hello_wire) = recv_preauth(io)?;
                if fb.msg_type != codec::MSG_HELLO {
                    return Err(ProtoError::ProtocolViolation("expected hello"));
                }
                let hello = Hello::parse(&fb.payload)?;

                check_identity(hello.stat_pub, expected_peer)?;
                let agreed = negotiate(our_max, hello.version)?;
                let ack = HelloAck {
                    version: our_max,
                    agreed,
                    flags,
                    eph_pub: my_epk,
                    stat_pub: my_stat,
                    nonce: my_nonce,
                };
                let my_wire = send_preauth(io, codec::MSG_HELLO_ACK, agreed, &ack.encode())?;
                (
                    PeerHello::Init(Box::new(hello)),
                    HelloWires {
                        initiator: hello_wire,
                        responder: my_wire,
                    },
                )
            }
        };

        let (agreed, peer_max, peer_flags, peer_eph, peer_stat) = match peer_hello {
            PeerHello::Init(h) => (
                negotiate(our_max, h.version)?,
                h.version,
                h.flags,
                h.eph_pub,
                h.stat_pub,
            ),
            PeerHello::Ack(a) => (a.agreed, a.version, a.flags, a.eph_pub, a.stat_pub),
        };

        let th_hello = transcript_hash(&[&hello_wires.initiator, &hello_wires.responder]);

        fn dh(sk: &StaticSecret, peer: [u8; 32]) -> Result<[u8; 32], ProtoError> {
            let shared = sk.diffie_hellman(&PublicKey::from(peer));
            if !shared.was_contributory() {
                return Err(ProtoError::Auth("degenerate DH output"));
            }
            Ok(*shared.as_bytes())
        }
        let e1 = dh(&esk, peer_eph)?;
        let (m1, m2): ([u8; 32], [u8; 32]) = match role {
            Role::Initiator => (
                *identity
                    .diffie_hellman(&peer_eph)
                    .map_err(|_| ProtoError::Auth("degenerate peer static key"))?,
                dh(&esk, peer_stat)?,
            ),
            Role::Responder => (
                dh(&esk, peer_stat)?,
                *identity
                    .diffie_hellman(&peer_eph)
                    .map_err(|_| ProtoError::Auth("degenerate peer static key"))?,
            ),
        };

        let (htk_i2r, htk_r2i, prk) = kdf_handshake(&th_hello, &e1, &m1, &m2);

        let proof_a: AuthProof = seal_auth(&htk_i2r, &th_hello, identity.device_id())?;
        let proof_b_key = htk_r2i.clone();

        let auth_wires = match role {
            Role::Initiator => {
                let w_init = send_preauth(io, codec::MSG_AUTH_INIT, agreed, &proof_a.encode())?;
                let (fb, _) = recv_preauth(io)?;
                if fb.msg_type != codec::MSG_AUTH_CONFIRM {
                    return Err(ProtoError::ProtocolViolation("expected auth confirm"));
                }
                let proof_r = AuthProof::parse(&fb.payload)?;
                let got = open_auth(&proof_b_key, &th_hello, &proof_r)
                    .map_err(|_| ProtoError::Auth("responder failed its possession proof"))?;
                check_identity(got, expected_peer)?;
                AuthWires {
                    initiator: w_init,
                    responder: full_wire(&fb),
                }
            }
            Role::Responder => {
                let (fb, w_init) = recv_preauth(io)?;
                if fb.msg_type != codec::MSG_AUTH_INIT {
                    return Err(ProtoError::ProtocolViolation("expected auth init"));
                }
                let proof_i = AuthProof::parse(&fb.payload)?;
                let got = open_auth(&htk_i2r, &th_hello, &proof_i)
                    .map_err(|_| ProtoError::Auth("initiator failed its possession proof"))?;
                check_identity(got, expected_peer)?;
                let proof_b = seal_auth(&htk_r2i, &th_hello, identity.device_id())?;
                let w_resp = send_preauth(io, codec::MSG_AUTH_CONFIRM, agreed, &proof_b.encode())?;
                AuthWires {
                    initiator: w_init,
                    responder: w_resp,
                }
            }
        };

        let th_final = transcript_hash(&[
            &hello_wires.initiator,
            &hello_wires.responder,
            &auth_wires.initiator,
            &auth_wires.responder,
        ]);
        let (tk_i2r, tk_r2i) = traffic_keys(&prk, &th_final);

        let (tx, rx) = if encryption {
            let (mine, theirs) = match role {
                Role::Initiator => (tk_i2r, tk_r2i),
                Role::Responder => (tk_r2i, tk_i2r),
            };
            (Some(mine.cipher()), Some(theirs.cipher()))
        } else {
            (None, None)
        };

        Ok((agreed, peer_max, peer_flags, tx, rx))
    }

    pub fn send_frame(&mut self, msg_type: u8, payload: Vec<u8>) -> Result<(), ProtoError> {
        self.send_frame_best_effort(msg_type, payload)
    }

    pub fn send_frame_best_effort(
        &mut self,
        msg_type: u8,
        payload: Vec<u8>,
    ) -> Result<(), ProtoError> {
        let fb = FrameBody::new(msg_type, self.version, payload);
        let body = fb.encode();
        match self.tx.as_mut() {
            Some(cipher) => {
                let ct = cipher.seal_frame(body.len() as u32 + 16, &body)?;
                crate::frame::write_body(&mut self.io, &ct)
            }
            None => crate::frame::write_body(&mut self.io, &body),
        }
    }

    pub fn recv_frame(&mut self) -> Result<Option<FrameBody>, ProtoError> {
        loop {
            let raw = crate::frame::read_body(&mut self.io)?;
            let plain = match self.rx.as_mut() {
                Some(cipher) => cipher.open_frame(raw.len() as u32, &raw)?,
                None => raw,
            };
            let fb = FrameBody::parse(&plain)?;
            if !codec::KNOWN_TYPES.contains(&fb.msg_type) {
                let higher = self.peer_max.major() == ProtocolVersion::V1_0.major()
                    && self.peer_max.minor() > ProtocolVersion::V1_0.minor();
                let flagged = (self.peer_flags & !FLAG_EXTENSION_AWARE) != 0;
                if higher && flagged {
                    continue;
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
            Some(other) => Err(unexpected(other.msg_type)),
            None => unreachable!("recv_frame never returns None without looping"),
        }
    }

    pub fn expect_frame_any(&mut self, types: &[u8]) -> Result<FrameBody, ProtoError> {
        match self.recv_frame()? {
            Some(fb) if types.contains(&fb.msg_type) => Ok(fb),
            Some(other) => Err(unexpected(other.msg_type)),
            None => unreachable!("recv_frame never returns None without looping"),
        }
    }

    pub fn recv_expect_bye(&mut self) -> Result<(), ProtoError> {
        let fb = self.expect_frame(codec::MSG_BYE)?;
        let bye = Bye::parse(&fb.payload)?;
        match bye.reason {
            ByeReason::Normal => Ok(()),
            other => Err(ProtoError::ByeReceived { reason: other }),
        }
    }

    pub fn version(&self) -> ProtocolVersion {
        self.version
    }

    pub fn peer_max(&self) -> ProtocolVersion {
        self.peer_max
    }

    pub fn peer_flags(&self) -> u64 {
        self.peer_flags
    }

    pub fn peer_id(&self) -> DeviceId {
        self.peer_id
    }

    pub fn is_encrypted(&self) -> bool {
        self.tx.is_some()
    }

    pub fn io_mut(&mut self) -> &mut S {
        &mut self.io
    }

    pub fn into_io(self) -> S {
        self.io
    }

    #[cfg(test)]
    pub(crate) fn from_parts(
        io: S,
        version: ProtocolVersion,
        peer_max: ProtocolVersion,
        peer_flags: u64,
        peer_id: DeviceId,
        tx: Option<SessionCipher>,
        rx: Option<SessionCipher>,
    ) -> Self {
        SecureSession {
            io,
            version,
            peer_max,
            peer_flags,
            peer_id,
            tx,
            rx,
        }
    }
}

impl<S: ByteStream> core::fmt::Debug for SecureSession<S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SecureSession")
            .field("version", &self.version)
            .field("peer_max", &self.peer_max)
            .field("peer_flags", &self.peer_flags)
            .field("peer_id", &self.peer_id)
            .field("encrypted", &self.is_encrypted())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::FrameBody;
    use crate::version::ProtocolVersion;

    fn dh_terms() -> ([Box<[u8; 32]>; 3], [u8; 32]) {
        (
            [
                Box::new(core::array::from_fn(|i| i as u8 + 1)),
                Box::new(core::array::from_fn(|i| i as u8 + 41)),
                Box::new(core::array::from_fn(|i| i as u8 + 91)),
            ],
            transcript_hash(&[b"hello", b"ack"]),
        )
    }

    #[test]
    fn schedule_is_deterministic_and_direction_separated() {
        let (terms, th) = dh_terms();
        let (h_a2b, h_b2a, prk) = kdf_handshake(&th, &terms[0], &terms[1], &terms[2]);
        assert_ne!(h_a2b.as_ref(), h_b2a.as_ref());

        let (t_a2b, t_b2a) = traffic_keys(&prk, &th);

        assert_ne!(t_a2b.0.as_ref(), t_b2a.0.as_ref());

        let again = kdf_handshake(&th, &terms[0], &terms[1], &terms[2]);
        assert_eq!(again.0.as_ref(), h_a2b.as_ref());
        let (t2, _) = traffic_keys(&again.2, &th);
        assert_eq!(t2.0.as_ref(), t_a2b.0.as_ref());
    }

    #[test]
    fn any_transcript_or_term_change_changes_every_key() {
        let (terms, th) = dh_terms();
        let base = kdf_handshake(&th, &terms[0], &terms[1], &terms[2]);

        let mut th2 = th;
        th2[0] ^= 1;
        let changed_th = kdf_handshake(&th2, &terms[0], &terms[1], &terms[2]);
        assert_ne!(base.0.as_ref(), changed_th.0.as_ref());

        let mut e2 = terms[0].clone();
        e2[31] ^= 1;
        let changed_e = kdf_handshake(&th, &e2, &terms[1], &terms[2]);
        assert_ne!(base.0.as_ref(), changed_e.0.as_ref());
        assert_ne!(base.2.as_ref(), changed_e.2.as_ref());

        let (ta, _) = traffic_keys(&base.2, &th);
        let (tb, _) = traffic_keys(&base.2, &th2);
        assert_ne!(ta.0.as_ref(), tb.0.as_ref());
    }

    #[test]
    fn auth_proof_round_trips_only_under_the_right_key_and_transcript() {
        let (terms, th) = dh_terms();
        let (h_a2b, _, _) = kdf_handshake(&th, &terms[0], &terms[1], &terms[2]);
        let proof = seal_auth(&h_a2b, &th, &[9; 32]).unwrap();
        assert_eq!(proof.ciphertext.len(), AuthProof::CT_LEN);
        assert_eq!(open_auth(&h_a2b, &th, &proof).unwrap(), [9; 32]);

        let mut th_evil = th;
        th_evil[0] ^= 1;
        assert!(open_auth(&h_a2b, &th_evil, &proof).is_err());
    }

    #[test]
    fn frames_seal_open_with_counters_and_reject_reorder_replay_tamper() {
        let (terms, th) = dh_terms();
        let (_, _, prk) = kdf_handshake(&th, &terms[0], &terms[1], &terms[2]);

        let (ka, _) = traffic_keys(&prk, &th);
        let (kb2, _) = traffic_keys(&prk, &th);
        let mut tx = ka.cipher();
        let mut rx = kb2.cipher();

        let f0 = tx.seal_frame(100, b"first").unwrap();
        let f1 = tx.seal_frame(105, b"second").unwrap();
        assert_ne!(f0, f1, "counter must change the ciphertext");

        assert!(rx.open_frame(105, &f1).is_err());

        let (ka3, _) = traffic_keys(&prk, &th);
        let (ka4, _) = traffic_keys(&prk, &th);
        let mut tx = ka3.cipher();
        let mut rx = ka4.cipher();
        let g0 = tx.seal_frame(100, b"first").unwrap();
        let g1 = tx.seal_frame(105, b"second").unwrap();
        assert_eq!(rx.open_frame(100, &g0).unwrap(), b"first");
        assert_eq!(rx.open_frame(105, &g1).unwrap(), b"second");

        assert!(rx.open_frame(100, &g0).is_err());

        let fresh = tx.seal_frame(10, b"payload").unwrap();
        let mut evil = fresh.clone();
        evil[3] ^= 0x80;
        assert!(rx.open_frame(10, &evil).is_err());

        let bound = tx.seal_frame(77, b"x").unwrap();
        assert!(rx.open_frame(78, &bound).is_err());
    }

    #[test]
    fn exhausted_counter_is_a_hard_error_never_nonce_reuse() {
        let (_, _, prk) = kdf_handshake(&[0u8; 32], &[1u8; 32], &[2u8; 32], &[3u8; 32]);
        let (k, _) = traffic_keys(&prk, &[4u8; 32]);
        let mut c = SessionCipher::at_sequence(k, u64::MAX);
        assert!(matches!(
            c.seal_frame(1, b""),
            Err(ProtoError::CounterExhausted)
        ));
    }

    #[test]
    fn debug_output_never_contains_key_material() {
        let (_, _, prk) = kdf_handshake(&[0u8; 32], &[1; 32], &[2; 32], &[3; 32]);
        let (k, _) = traffic_keys(&prk, &[5u8; 32]);
        let rendered = format!("{:?}\n{:?}", k, SessionCipher::new(SessionKey(k.0.clone())));

        let hexy = hex(&k.0.as_ref()[..4]);
        assert!(!rendered.contains(&hexy));
        assert!(!rendered.contains("secret"));
    }

    #[test]
    fn sealed_frames_carry_the_full_encoded_body_including_magic() {
        use crate::frame::{read_body, write_body};
        use crate::stream::duplex_pair;
        let (_, _, prk) = kdf_handshake(&[0u8; 32], &[1; 32], &[2; 32], &[3; 32]);
        let (k, _) = traffic_keys(&prk, &[5u8; 32]);
        let mut tx = k.cipher();
        let body = FrameBody::new(
            crate::codec::MSG_ITEM_BATCH,
            ProtocolVersion::V1_0,
            vec![7, 7],
        )
        .encode();
        let len_prefix = (body.len() + 16) as u32;
        let ct = tx.seal_frame(len_prefix, &body).unwrap();

        let (mut a, mut b) = duplex_pair();
        write_body(&mut a, &ct).unwrap();
        let got = read_body(&mut b).unwrap();
        assert_eq!(got.len(), body.len() + 16);
        let (k2, _) = traffic_keys(&prk, &[5u8; 32]);
        let plain = k2.cipher().open_frame(len_prefix, &got).unwrap();
        assert_eq!(FrameBody::parse(&plain).unwrap().payload, vec![7, 7]);
    }

    fn test_identity(seed: u8) -> DeviceIdentity {
        let mut sk = [0u8; 32];
        for (i, b) in sk.iter_mut().enumerate() {
            *b = seed.wrapping_mul(131).wrapping_add(i as u8);
        }
        DeviceIdentity::from_secret_bytes(&sk)
    }

    #[test]
    fn handshake_establishes_and_exchanges_sealed_frames() {
        use crate::stream::duplex_pair;
        let id_a = test_identity(1);
        let id_b = test_identity(2);

        let (client_io, server_io) = duplex_pair();
        let srv = std::thread::spawn({
            let id_a_dev = *id_a.device_id();
            let id_b = id_b.clone();
            move || {
                let mut sess = SecureSession::establish(
                    server_io,
                    Role::Responder,
                    &id_b,
                    id_a_dev,
                    ProtocolVersion::V1_0,
                    true,
                )?;
                assert_eq!(sess.version(), ProtocolVersion::V1_0);
                assert_eq!(sess.peer_id(), id_a_dev);
                assert!(sess.is_encrypted());
                let frame = sess.expect_frame(crate::codec::MSG_ITEM_BATCH)?;
                assert_eq!(frame.payload, vec![42, 43]);
                sess.send_frame(crate::codec::MSG_ITEM_BATCH, vec![99])?;
                Ok::<_, ProtoError>(())
            }
        });

        let mut cli = SecureSession::establish(
            client_io,
            Role::Initiator,
            &id_a,
            *id_b.device_id(),
            ProtocolVersion::V1_0,
            true,
        )
        .unwrap();

        assert_eq!(cli.version(), ProtocolVersion::V1_0);
        assert_eq!(cli.peer_id(), *id_b.device_id());
        assert!(cli.is_encrypted());

        cli.send_frame(crate::codec::MSG_ITEM_BATCH, vec![42, 43])
            .unwrap();
        let reply = cli.expect_frame(crate::codec::MSG_ITEM_BATCH).unwrap();
        assert_eq!(reply.payload, vec![99]);

        srv.join().unwrap().unwrap();
    }

    #[test]
    fn handshake_fails_on_identity_mismatch_initiator() {
        use crate::stream::duplex_pair;
        let id_a = test_identity(3);
        let id_b = test_identity(4);
        let id_wrong = test_identity(5);

        let (client_io, server_io) = duplex_pair();
        let srv = std::thread::spawn({
            let id_a_dev = *id_a.device_id();
            let id_b = id_b.clone();
            move || {
                SecureSession::establish(
                    server_io,
                    Role::Responder,
                    &id_b,
                    id_a_dev,
                    ProtocolVersion::V1_0,
                    true,
                )
            }
        });

        let cli_res = SecureSession::establish(
            client_io,
            Role::Initiator,
            &id_a,
            *id_wrong.device_id(),
            ProtocolVersion::V1_0,
            true,
        );
        assert!(matches!(cli_res, Err(ProtoError::IdentityMismatch { .. })));
        let _ = srv.join().unwrap();
    }

    #[test]
    fn handshake_fails_on_identity_mismatch_responder() {
        use crate::stream::duplex_pair;
        let id_a = test_identity(6);
        let id_b = test_identity(7);
        let id_wrong = test_identity(8);

        let (client_io, server_io) = duplex_pair();
        let srv = std::thread::spawn({
            let id_b = id_b.clone();
            let id_wrong_dev = *id_wrong.device_id();
            move || {
                SecureSession::establish(
                    server_io,
                    Role::Responder,
                    &id_b,
                    id_wrong_dev,
                    ProtocolVersion::V1_0,
                    true,
                )
            }
        });

        let cli_res = SecureSession::establish(
            client_io,
            Role::Initiator,
            &id_a,
            *id_b.device_id(),
            ProtocolVersion::V1_0,
            true,
        );
        let srv_res = srv.join().unwrap();
        assert!(matches!(srv_res, Err(ProtoError::IdentityMismatch { .. })));
        assert!(matches!(
            cli_res,
            Err(ProtoError::ByeReceived {
                reason: ByeReason::AuthFailed
            } | ProtoError::Io(_))
        ));
    }

    #[test]
    fn handshake_fails_on_corrupt_auth_init_tag() {
        use crate::stream::duplex_pair;
        let id_a = test_identity(10);
        let id_b = test_identity(11);
        let (client_io, mut pipe_a) = duplex_pair();
        let (mut pipe_b, server_io) = duplex_pair();

        let srv = std::thread::spawn({
            let id_a_dev = *id_a.device_id();
            let id_b = id_b.clone();
            move || {
                SecureSession::establish(
                    server_io,
                    Role::Responder,
                    &id_b,
                    id_a_dev,
                    ProtocolVersion::V1_0,
                    true,
                )
            }
        });

        let cli = std::thread::spawn({
            let id_b_dev = *id_b.device_id();
            move || {
                SecureSession::establish(
                    client_io,
                    Role::Initiator,
                    &id_a,
                    id_b_dev,
                    ProtocolVersion::V1_0,
                    true,
                )
            }
        });

        let hello = crate::frame::read_body(&mut pipe_a).unwrap();
        crate::frame::write_body(&mut pipe_b, &hello).unwrap();

        let ack = crate::frame::read_body(&mut pipe_b).unwrap();
        crate::frame::write_body(&mut pipe_a, &ack).unwrap();

        let mut auth_init = crate::frame::read_body(&mut pipe_a).unwrap();
        let last = auth_init.len() - 1;
        auth_init[last] ^= 0xFF;
        crate::frame::write_body(&mut pipe_b, &auth_init).unwrap();

        let srv_res = srv.join().unwrap();
        assert!(matches!(srv_res, Err(ProtoError::Auth(_))), "{srv_res:?}");

        let bye = crate::frame::read_body(&mut pipe_b).unwrap();
        crate::frame::write_body(&mut pipe_a, &bye).unwrap();

        let cli_res = cli.join().unwrap();
        assert!(
            matches!(
                cli_res,
                Err(ProtoError::ByeReceived {
                    reason: ByeReason::AuthFailed
                } | ProtoError::Auth(_))
            ),
            "{cli_res:?}"
        );
    }

    #[test]
    fn handshake_fails_on_corrupt_auth_confirm_tag() {
        use crate::stream::duplex_pair;
        let id_a = test_identity(12);
        let id_b = test_identity(13);
        let (client_io, mut pipe_a) = duplex_pair();
        let (mut pipe_b, server_io) = duplex_pair();

        let srv = std::thread::spawn({
            let id_a_dev = *id_a.device_id();
            let id_b = id_b.clone();
            move || {
                SecureSession::establish(
                    server_io,
                    Role::Responder,
                    &id_b,
                    id_a_dev,
                    ProtocolVersion::V1_0,
                    true,
                )
            }
        });

        let cli = std::thread::spawn({
            let id_b_dev = *id_b.device_id();
            move || {
                SecureSession::establish(
                    client_io,
                    Role::Initiator,
                    &id_a,
                    id_b_dev,
                    ProtocolVersion::V1_0,
                    true,
                )
            }
        });

        let hello = crate::frame::read_body(&mut pipe_a).unwrap();
        crate::frame::write_body(&mut pipe_b, &hello).unwrap();

        let ack = crate::frame::read_body(&mut pipe_b).unwrap();
        crate::frame::write_body(&mut pipe_a, &ack).unwrap();

        let auth_init = crate::frame::read_body(&mut pipe_a).unwrap();
        crate::frame::write_body(&mut pipe_b, &auth_init).unwrap();

        let mut auth_confirm = crate::frame::read_body(&mut pipe_b).unwrap();
        let last = auth_confirm.len() - 1;
        auth_confirm[last] ^= 0xFF;
        crate::frame::write_body(&mut pipe_a, &auth_confirm).unwrap();

        let cli_res = cli.join().unwrap();
        assert!(matches!(cli_res, Err(ProtoError::Auth(_))), "{cli_res:?}");

        let _ = srv.join().unwrap();
    }

    #[test]
    fn handshake_fails_on_bad_version() {
        use crate::stream::duplex_pair;
        let id_a = test_identity(20);
        let id_b = test_identity(21);

        let (client_io, server_io) = duplex_pair();
        let srv = std::thread::spawn({
            let id_a_dev = *id_a.device_id();
            let id_b = id_b.clone();
            move || {
                SecureSession::establish(
                    server_io,
                    Role::Responder,
                    &id_b,
                    id_a_dev,
                    ProtocolVersion::V1_0,
                    true,
                )
            }
        });

        let cli_res = SecureSession::establish(
            client_io,
            Role::Initiator,
            &id_a,
            *id_b.device_id(),
            ProtocolVersion::new(2, 0),
            true,
        );
        let srv_res = srv.join().unwrap();
        assert!(matches!(
            srv_res,
            Err(ProtoError::VersionIncompatible { .. })
        ));
        assert!(matches!(
            cli_res,
            Err(ProtoError::ByeReceived {
                reason: ByeReason::VersionIncompatible
            } | ProtoError::VersionIncompatible { .. }
                | ProtoError::Io(_))
        ));
    }
}
