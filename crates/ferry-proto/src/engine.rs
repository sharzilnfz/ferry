//! The conversation driver: hello → authenticate → offers → pull →
//! re-offer → agree → bye.
//!
//! Both peers run THIS SAME function; [`Role`] decides who speaks first.
//! The conversation is strict lockstep (one side writes while the other
//! reads), so it cannot deadlock over bounded TCP buffers at harness scale.
//! Documented limitation: v1 assumes each side's advert sequence for one
//! folder fits within socket buffers per turn; chunked/streamed adverts are
//! future-minor work (see `docs/store-format.md`, "Wire protocol v1").
//!
//! Pull flow per folder (`P` = puller, `S` = server):
//!
//! 1. P fetches S's root manifest BY ID, verifies `BLAKE3(pt) == id`, stores.
//! 2. P walks S's tree breadth-first: requests missing TREE NODES by id,
//!    verifying and storing each, accumulating every referenced CHUNK id.
//! 3. Missing chunks are grouped through S's ADVERTISED index entries:
//!    whole packs by ciphertext name where the granularity policy says so,
//!    individual blobs otherwise. Every received item is verified AFTER
//!    decryption (`BLAKE3(plaintext) == id`; packs `BLAKE3(ct) == name`)
//!    BEFORE it touches the store; rejects are surfaced as typed errors
//!    and re-requested up to the retry budget.
//! 4. A second offer round lets both sides observe post-pull equality;
//!    equal root manifest ids record the last-agreed pointer locally.
//!
//! An empty REQUEST_ITEMS is the end-of-pull marker: the server answers
//! with a single empty ITEM_BATCH terminator and returns to listening.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use rand::rngs::OsRng;
use x25519_dalek::{PublicKey, StaticSecret};

use ferry_crypto::identity::{DeviceId, DeviceIdentity};
use ferry_store::format::{hex, BlobId, BlobKind};
use ferry_store::index::IndexEntry;
use ferry_store::manifest::{parse_manifest, parse_tree_node, EntryPayload};
use ferry_store::store::Store;

use crate::agreement::{AgreementLedger, AgreementRecord};
use crate::codec::{
    self, AuthProof, Bye, FolderOffer, FrameBody, Hello, HelloAck, IndexAdvert, ItemBatch,
    PackItem, RequestItems, RequestPacks,
};
use crate::error::{ByeReason, ProtoError};
use crate::secure::{kdf_handshake, open_auth, seal_auth, traffic_keys, transcript_hash, SessionCipher};
use crate::stream::ByteStream;
use crate::version::negotiate;
use crate::version::ProtocolVersion;
use crate::codec::FLAG_EXTENSION_AWARE;

/// Zero folder_id marks "end of announcement list" in offer rounds.
const FOLDER_SENTINEL: [u8; 16] = [0; 16];

/// Which side of the conversation this engine is. The Initiator sends the
/// first Hello; over TCP this is the dialing peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Initiator,
    Responder,
}

/// How the puller chooses between pack-granular and item-granular fetches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Granularity {
    /// Whole pack when ≥ 2 wanted chunks share one pack, items otherwise.
    Auto,
    /// Never request packs; always item-level.
    ItemsOnly,
    /// Always prefer whole packs for data chunks.
    PacksOnly,
}

/// One synced folder this engine serves/wants.
pub struct FolderState {
    pub folder_id: [u8; 16],
    pub store: Arc<Store>,
    /// Our current root manifest id (`None` = fresh device for this folder).
    pub current_manifest: Option<BlobId>,
}

/// Engine configuration. Consumed by [`run_engine`]; final state comes back
/// in the [`SessionReport`].
pub struct EngineConfig {
    pub identity: DeviceIdentity,
    /// The ONLY peer we accept (ADR-0003: peers are their public keys).
    pub expected_peer: DeviceId,
    pub folders: Vec<FolderState>,
    /// Seal post-auth frames under session keys. Handshake authentication is
    /// ALWAYS active regardless. `false` is a development/testing mode only;
    /// production engines must leave this at the default (true).
    pub encryption: bool,
    pub granularity: Granularity,
    /// Re-request budget for corrupt/missing items before failing cleanly.
    pub max_retries: u32,
}

impl EngineConfig {
    /// Production defaults: encryption ON, auto granularity, 3 retries.
    pub fn new(
        identity: DeviceIdentity,
        expected_peer: DeviceId,
        folders: Vec<FolderState>,
    ) -> Self {
        EngineConfig {
            identity,
            expected_peer,
            folders,
            encryption: true,
            granularity: Granularity::Auto,
            max_retries: 3,
        }
    }
}

/// Per-folder result of one session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderOutcome {
    pub folder_id: [u8; 16],
    /// Our manifest pointer after the session (adopted the remote's if we
    /// started empty).
    pub local_manifest_after: Option<BlobId>,
    pub remote_manifest: Option<BlobId>,
    /// Set when both sides held equal manifests and the last-agreed pointer
    /// was recorded.
    pub agreement_recorded: Option<BlobId>,
    /// Received items that failed verification and were rejected
    /// (re-requested afterwards).
    pub rejections: usize,
}

/// Everything one successful session produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionReport {
    pub peer: DeviceId,
    pub agreed_version: ProtocolVersion,
    pub encrypted: bool,
    pub folders: Vec<FolderOutcome>,
}

/// Run one full session over `io`.
pub fn run_engine<S: ByteStream>(
    mut io: S,
    role: Role,
    cfg: EngineConfig,
) -> Result<SessionReport, ProtoError> {
    let our_max = ProtocolVersion::V1_0;
    let hs = match handshake(&mut io, role, &cfg, our_max) {
        Ok(h) => h,
        // No session ciphers exist yet: the clean disconnect is a plaintext
        // BYE carrying the typed reason.
        Err(e) => {
            if !matches!(e, ProtoError::ByeReceived { .. } | ProtoError::Io(_)) {
                let reason = match e {
                    ProtoError::VersionIncompatible { .. } => ByeReason::VersionIncompatible,
                    ProtoError::Auth(_) | ProtoError::IdentityMismatch { .. } => ByeReason::AuthFailed,
                    _ => ByeReason::ProtocolViolation,
                };
                let _ = send_plain(&mut io, codec::MSG_BYE, our_max, &[reason as u8]);
            }
            return Err(e);
        }
    };

    let encrypted = hs.tx.is_some();
    let mut sess = Session {
        io: &mut io,
        version: hs.agreed,
        peer_max: hs.peer_max,
        peer_flags: hs.peer_flags,
        peer_id: cfg.expected_peer,
        tx: hs.tx,
        rx: hs.rx,
    };

    let mut outcomes: Vec<FolderOutcome> = cfg
        .folders
        .iter()
        .map(|f| FolderOutcome {
            folder_id: f.folder_id,
            local_manifest_after: f.current_manifest,
            remote_manifest: None,
            agreement_recorded: None,
            rejections: 0,
        })
        .collect();

    if let Err(e) = folder_phases(&mut sess, role, &cfg, &mut outcomes) {
        return abort(&mut sess, e);
    }

    // Clean shutdown: initiator sends BYE first, responder mirrors it.
    let bye_result = match role {
        Role::Initiator => sess
            .send_frame(codec::MSG_BYE, Bye { reason: ByeReason::Normal }.encode())
            .and_then(|()| sess.recv_expect_bye()),
        Role::Responder => sess.recv_expect_bye().and_then(|()| {
            sess.send_frame(codec::MSG_BYE, Bye { reason: ByeReason::Normal }.encode())
        }),
    };
    if let Err(e) = bye_result {
        return abort(&mut sess, e);
    }

    Ok(SessionReport {
        peer: cfg.expected_peer,
        agreed_version: hs.agreed,
        encrypted,
        folders: outcomes,
    })
}

/// Best-effort BYE (post-auth ciphers when present) then propagate the
/// original error untouched.
fn abort<S: ByteStream>(
    sess: &mut Session<'_, S>,
    err: ProtoError,
) -> Result<SessionReport, ProtoError> {
    if !matches!(err, ProtoError::ByeReceived { .. } | ProtoError::Io(_)) {
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
        let _ = sess.send_frame_best_effort(codec::MSG_BYE, Bye { reason }.encode());
    }
    Err(err)
}

// --- low-level frame io ------------------------------------------------------

/// Full wire bytes of a pre-auth frame: length prefix || magic || type ||
/// version || payload. The handshake hashes exactly these bytes.
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
    crate::frame::write_body(io, &full_wire(&FrameBody::new(msg_type, version, payload.to_vec())).as_slice()[4..])
}

/// Send one pre-auth frame, returning its exact wire image.
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
    Ok((FrameBody::parse(&body)?, wire))
}

// --- session state ------------------------------------------------------------

pub(crate) struct Session<'a, S: ByteStream> {
    pub(crate) io: &'a mut S,
    pub(crate) version: ProtocolVersion,
    pub(crate) peer_max: ProtocolVersion,
    pub(crate) peer_flags: u64,
    /// The authenticated peer identity (verified during handshake).
    pub(crate) peer_id: DeviceId,
    pub(crate) tx: Option<SessionCipher>,
    pub(crate) rx: Option<SessionCipher>,
}

impl<S: ByteStream> Session<'_, S> {
    fn send_frame(&mut self, msg_type: u8, payload: Vec<u8>) -> Result<(), ProtoError> {
        self.send_frame_best_effort(msg_type, payload)
    }

    fn send_frame_best_effort(&mut self, msg_type: u8, payload: Vec<u8>) -> Result<(), ProtoError> {
        let fb = FrameBody::new(msg_type, self.version, payload);
        let body = fb.encode();
        match self.tx.as_mut() {
            Some(cipher) => {
                let ct = cipher.seal_frame(body.len() as u32 + 16, &body)?;
                crate::frame::write_body(self.io, &ct)
            }
            None => crate::frame::write_body(self.io, &body),
        }
    }

    /// Receive one frame, applying the unknown-message-type rule:
    ///
    /// - Unknown types before auth completes are protocol violations (the
    ///   pre-auth surface is frozen at four message types).
    /// - Post-auth: an unknown type is SKIPPED silently iff the peer
    ///   advertised a higher minor than ours AND carries feature flags we
    ///   do not know (the ignore-if-flagged rule). Otherwise it is a
    ///   protocol violation → clean disconnect.
    ///
    /// Returns `Ok(None)` for skipped frames so callers can loop.
    pub(crate) fn recv_frame(&mut self) -> Result<Option<FrameBody>, ProtoError> {
        loop {
            let raw = crate::frame::read_body(self.io)?;
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
                    continue; // skip-if-flagged
                }
                return Err(ProtoError::UnknownMessage {
                    msg_type: fb.msg_type,
                });
            }
            return Ok(Some(fb));
        }
    }

    fn expect_frame(&mut self, msg_type: u8) -> Result<FrameBody, ProtoError> {
        match self.recv_frame()? {
            Some(fb) if fb.msg_type == msg_type => Ok(fb),
            Some(other) => Err(unexpected(other.msg_type)),
            None => unreachable!("recv_frame never returns None without looping"),
        }
    }

    fn expect_frame_any(&mut self, types: &[u8]) -> Result<FrameBody, ProtoError> {
        match self.recv_frame()? {
            Some(fb) if types.contains(&fb.msg_type) => Ok(fb),
            Some(other) => Err(unexpected(other.msg_type)),
            None => unreachable!("recv_frame never returns None without looping"),
        }
    }

    fn recv_expect_bye(&mut self) -> Result<(), ProtoError> {
        let fb = self.expect_frame(codec::MSG_BYE)?;
        let bye = Bye::parse(&fb.payload)?;
        match bye.reason {
            ByeReason::Normal => Ok(()),
            other => Err(ProtoError::ByeReceived { reason: other }),
        }
    }
}

fn unexpected(t: u8) -> ProtoError {
    let _ = t;
    ProtoError::ProtocolViolation("unexpected message in this state")
}

fn store_err(e: ferry_store::store::StoreError) -> ProtoError {
    ProtoError::Io(std::io::Error::new(
        std::io::ErrorKind::Other,
        e.to_string(),
    ))
}

// --- handshake -----------------------------------------------------------------

struct HandshakeResult {
    agreed: ProtocolVersion,
    peer_max: ProtocolVersion,
    peer_flags: u64,
    tx: Option<SessionCipher>,
    rx: Option<SessionCipher>,
}

fn random32() -> [u8; 32] {
    use rand::RngCore;
    let mut b = [0u8; 32];
    OsRng.fill_bytes(&mut b);
    b
}

/// Drive hello + mutual authentication. Always active, even when session
/// sealing is disabled — possession proofs are not optional.
fn handshake<S: ByteStream>(
    io: &mut S,
    role: Role,
    cfg: &EngineConfig,
    our_max: ProtocolVersion,
) -> Result<HandshakeResult, ProtoError> {
    let flags = FLAG_EXTENSION_AWARE;
    // StaticSecret (not EphemeralSecret) because its diffie_hellman BORROWS,
    // letting one fresh scalar feed all three DH terms.
    let esk = StaticSecret::random_from_rng(OsRng);
    let my_epk = *PublicKey::from(&esk).as_bytes();
    let my_nonce = random32();
    let my_stat = *cfg.identity.device_id();

    let my_hello = Hello {
        version: our_max,
        flags,
        eph_pub: my_epk,
        stat_pub: my_stat,
        nonce: my_nonce,
    };

    // --- exchange hellos ---
    let (peer_hello, hello_wires) = match role {
        Role::Initiator => {
            let my_wire =
                send_preauth(io, codec::MSG_HELLO, our_max, &my_hello.encode())?;
            let (fb, _) = recv_preauth(io)?;
            let ack = HelloAck::parse(&fb.payload)?;
            check_identity(ack.stat_pub, cfg.expected_peer)?;
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
            let hello = Hello::parse(&fb.payload)?;
            check_identity(hello.stat_pub, cfg.expected_peer)?;
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

    // --- three DH terms ---
    // EphemeralSecret::diffie_hellman is infallible (a fresh random scalar
    // is never degenerate); identity.diffie_hellman checks contribution.
    fn dh(sk: &StaticSecret, peer: [u8; 32]) -> Result<[u8; 32], ProtoError> {
        let shared = sk.diffie_hellman(&PublicKey::from(peer));
        if !shared.was_contributory() {
            return Err(ProtoError::Auth("degenerate DH output"));
        }
        Ok(*shared.as_bytes())
    }
    let e1 = dh(&esk, peer_eph)?;
    // m1 authenticates the INITIATOR's static key, m2 the RESPONDER's.
    let (m1, m2): ([u8; 32], [u8; 32]) = match role {
        Role::Initiator => (
            *cfg.identity.diffie_hellman(&peer_eph).map_err(|_| ProtoError::Auth("degenerate peer static key"))?, // A.stat × B.eph
            dh(&esk, peer_stat)?,                     // A.eph × B.stat
        ),
        Role::Responder => (
            dh(&esk, peer_stat)?,                                     // B.eph × A.stat == A.stat × B.eph
            *cfg.identity.diffie_hellman(&peer_eph).map_err(|_| ProtoError::Auth("degenerate peer static key"))?, // B.stat × A.eph
        ),
    };

    let (htk_a2b, htk_b2a, prk) = kdf_handshake(&th_hello, &e1, &m1, &m2);

    // --- mutual proofs: initiator first, then responder ---
    let proof_a: AuthProof = seal_auth(&htk_a2b, &th_hello, cfg.identity.device_id())?;
    let proof_b_key = htk_b2a.clone();

    let auth_wires = match role {
        Role::Initiator => {
            let w_init = send_preauth(io, codec::MSG_AUTH_INIT, agreed, &proof_a.encode())?;
            let (fb, _) = recv_preauth(io)?;
            let proof_r = AuthProof::parse(&fb.payload)?;
            let got = open_auth(&proof_b_key, &th_hello, &proof_r)
                .map_err(|_| ProtoError::Auth("responder failed its possession proof"))?;
            check_identity(got, cfg.expected_peer)?;
            AuthWires {
                initiator: w_init,
                responder: full_wire(&fb),
            }
        }
        Role::Responder => {
            let (fb, w_init) = recv_preauth(io)?;
            let proof_i = AuthProof::parse(&fb.payload)?;
            let got = open_auth(&htk_a2b, &th_hello, &proof_i)
                .map_err(|_| ProtoError::Auth("initiator failed its possession proof"))?;
            check_identity(got, cfg.expected_peer)?;
            let proof_b = seal_auth(&htk_b2a, &th_hello, cfg.identity.device_id())?;
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
    let (tk_a2b, tk_b2a) = traffic_keys(&prk, &th_final);

    let (tx, rx) = if cfg.encryption {
        let (mine, theirs) = match role {
            Role::Initiator => (tk_a2b, tk_b2a),
            Role::Responder => (tk_b2a, tk_a2b),
        };
        (Some(mine.cipher()), Some(theirs.cipher()))
    } else {
        (None, None)
    };

    Ok(HandshakeResult {
        agreed,
        peer_max,
        peer_flags,
        tx,
        rx,
    })
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




// --- folder phases ---------------------------------------------------------------

/// Peer view of one folder, learned from offers.
#[derive(Clone, Copy, Debug)]
struct PeerFolder {
    manifest: Option<BlobId>,
}

type AdvertMap = BTreeMap<BlobId, IndexEntry>;

/// Payload flush threshold for ITEM_BATCH frames (8 MiB).
const BATCH_FLUSH_BYTES: usize = 8 * 1024 * 1024;
/// BFS round guard for remote tree walks.
const MAX_BFS_ROUNDS: usize = 64;

fn now_secs_nsecs() -> (i64, u32) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (now.as_secs() as i64, now.subsec_nanos())
}

fn folder_phases<S: ByteStream>(
    sess: &mut Session<'_, S>,
    role: Role,
    cfg: &EngineConfig,
    outcomes: &mut [FolderOutcome],
) -> Result<(), ProtoError> {
    // Round 1: full announcements with adverts.
    let (peer_folders, peer_adverts) = exchange_offers(sess, role, cfg, true)?;
    for out in outcomes.iter_mut() {
        out.remote_manifest = peer_folders.get(&out.folder_id).and_then(|p| p.manifest);
    }

    // Pull stages are strictly serialized by ROLE so both sides agree on
    // who writes and who serves without further negotiation. Both sides
    // compute the same stage conditions from round-1 offer state.
    let initiator_stage = stage_needed(cfg, &peer_folders, Role::Initiator, role);
    let responder_stage = stage_needed(cfg, &peer_folders, Role::Responder, role);

    match role {
        Role::Initiator => {
            if !initiator_stage.is_empty() {
                run_stage(sess, cfg, &initiator_stage, &peer_folders, &peer_adverts, outcomes)?;
            }
            if !responder_stage.is_empty() {
                serve_stage(sess, cfg)?;
            }
        }
        Role::Responder => {
            if !initiator_stage.is_empty() {
                serve_stage(sess, cfg)?;
            }
            if !responder_stage.is_empty() {
                run_stage(sess, cfg, &responder_stage, &peer_folders, &peer_adverts, outcomes)?;
            }
        }
    }

    // Round 2 + agreement recording.
    finish_after_sync(sess, role, cfg, outcomes)
}

/// Folders the given side would pull, as indices into `cfg.folders`.
///
/// `whose == my_role`: that side's current manifests are my own
/// `current_manifest`s and its counterpart's come from the peer offers.
/// Otherwise reversed. Both peers run this with identical inputs, so both
/// agree on which stages exist without extra messages.
fn stage_needed(
    cfg: &EngineConfig,
    peer_folders: &BTreeMap<[u8; 16], PeerFolder>,
    whose: Role,
    my_role: Role,
) -> Vec<usize> {
    let mut out = Vec::new();
    for (idx, f) in cfg.folders.iter().enumerate() {
        let Some(pf) = peer_folders.get(&f.folder_id) else {
            continue; // peer does not share this folder
        };
        let (that_side, counterpart) = if whose == my_role {
            (f.current_manifest, pf.manifest)
        } else {
            (pf.manifest, f.current_manifest)
        };
        if pull_needed(that_side, counterpart) {
            out.push(idx);
        }
    }
    out
}

fn pull_needed(mine: Option<BlobId>, theirs: Option<BlobId>) -> bool {
    matches!(theirs, Some(t) if mine != Some(t))
}

/// One pull stage: fetch every listed folder, then send the empty
/// REQUEST_ITEMS end-of-stage marker.
fn run_stage<S: ByteStream>(
    sess: &mut Session<'_, S>,
    cfg: &EngineConfig,
    idxs: &[usize],
    peer_folders: &BTreeMap<[u8; 16], PeerFolder>,
    peer_adverts: &BTreeMap<[u8; 16], AdvertMap>,
    outcomes: &mut [FolderOutcome],
) -> Result<(), ProtoError> {
    for &idx in idxs {
        let f = &cfg.folders[idx];
        let Some(pf) = peer_folders.get(&f.folder_id) else {
            continue;
        };
        let Some(target) = pf.manifest else { continue };
        let empty = AdvertMap::new();
        let adverts = peer_adverts.get(&f.folder_id).unwrap_or(&empty);
        pull_folder(
            sess,
            f.folder_id,
            &f.store,
            target,
            f.current_manifest,
            adverts,
            cfg.granularity,
            cfg.max_retries,
            &mut outcomes[idx].rejections,
        )?;
        if f.current_manifest.is_none() {
            outcomes[idx].local_manifest_after = Some(target); // adopted
        }
    }
    sess.send_frame(
        codec::MSG_REQUEST_ITEMS,
        RequestItems {
            folder_id: [0; 16],
            items: vec![],
        }
        .encode()?,
    )
}

/// Serve the peer's pull stage until its end-of-stage marker arrives.
fn serve_stage<S: ByteStream>(sess: &mut Session<'_, S>, cfg: &EngineConfig) -> Result<(), ProtoError> {
    loop {
        let fb = match sess.recv_frame()? {
            Some(fb) => fb,
            None => continue,
        };
        match fb.msg_type {
            codec::MSG_REQUEST_ITEMS => {
                let r = RequestItems::parse(&fb.payload)?;
                if r.items.is_empty() {
                    return Ok(());
                }
                serve_items(sess, cfg, r)?;
            }
            codec::MSG_REQUEST_PACKS => serve_packs(sess, cfg, RequestPacks::parse(&fb.payload)?)?,
            other => return Err(unexpected(other)),
        }
    }
}

fn find_store(cfg: &EngineConfig, folder_id: [u8; 16]) -> Result<&Arc<Store>, ProtoError> {
    cfg.folders
        .iter()
        .find(|f| f.folder_id == folder_id)
        .map(|f| &f.store)
        .ok_or_else(|| ProtoError::FolderUnknown {
            folder: hex(&folder_id),
        })
}

fn serve_items<S: ByteStream>(
    sess: &mut Session<'_, S>,
    cfg: &EngineConfig,
    r: RequestItems,
) -> Result<(), ProtoError> {
    let store = find_store(cfg, r.folder_id)?;
    let mut acc: Vec<(BlobKind, BlobId, Vec<u8>)> = Vec::new();
    let mut size = 0usize;
    for (kind, id) in r.items {
        if let Ok(bytes) = store.get(kind, &id) {
            size += bytes.len();
            acc.push((kind, id, bytes));
        }
        // Unserved ids are omitted; the requester detects the gap.
        if acc.len() >= codec::MAX_BATCH_ITEMS || size >= BATCH_FLUSH_BYTES {
            let batch = std::mem::take(&mut acc);
            size = 0;
            sess.send_frame(codec::MSG_ITEM_BATCH, ItemBatch { items: batch }.encode()?)?;
        }
    }
    // Exactly one trailing batch; empty when nothing remains — the
    // response terminator.
    sess.send_frame(codec::MSG_ITEM_BATCH, ItemBatch { items: acc }.encode()?)
}

fn serve_packs<S: ByteStream>(
    sess: &mut Session<'_, S>,
    cfg: &EngineConfig,
    r: RequestPacks,
) -> Result<(), ProtoError> {
    let store = find_store(cfg, r.folder_id)?;
    let packs_dir = store.store_dir().join("packs");
    for name in r.packs {
        let path = packs_dir.join(format!("{}.pack", hex(&name)));
        if let Ok(bytes) = std::fs::read(&path) {
            // Serve only bytes that verify against their own name.
            if *blake3::hash(&bytes).as_bytes() == name {
                sess.send_frame(codec::MSG_PACK_ITEM, PackItem { pack: name, bytes }.encode()?)?;
            }
        }
    }
    sess.send_frame(codec::MSG_ITEM_BATCH, ItemBatch::TERMINATOR.encode()?)
}

// --- offer / advert exchange -------------------------------------------------

/// Announce + mirror offers. With `with_adverts` each offer is followed by
/// the announcer's index-advert sequence for that folder. A ZERO folder_id
/// offer ends an announcement list. Round 2 (`with_adverts = false`)
/// re-announces post-pull state so equality is observable on both sides.
#[allow(clippy::type_complexity)]
fn exchange_offers<S: ByteStream>(
    sess: &mut Session<'_, S>,
    role: Role,
    cfg: &EngineConfig,
    with_adverts: bool,
) -> Result<(
    BTreeMap<[u8; 16], PeerFolder>,
    BTreeMap<[u8; 16], AdvertMap>,
), ProtoError> {
    let mut peer_folders: BTreeMap<[u8; 16], PeerFolder> = BTreeMap::new();
    let mut peer_adverts: BTreeMap<[u8; 16], AdvertMap> = BTreeMap::new();

    let announce_one = |sess: &mut Session<'_, S>, f: &FolderState| -> Result<(), ProtoError> {
        sess.send_frame(codec::MSG_FOLDER_OFFER, FolderOffer {
            folder_id: f.folder_id,
            manifest_id: f.current_manifest.unwrap_or([0; 32]),
            reserved: 0,
        }.encode())?;
        if with_adverts {
            send_my_adverts(sess, Some(&f.store))?;
        }
        Ok(())
    };

    let echo_as_mirror = |sess: &mut Session<'_, S>,
                          cfg: &EngineConfig,
                          folder_id: [u8; 16]|
     -> Result<(), ProtoError> {
        let mine = cfg.folders.iter().find(|f| f.folder_id == folder_id);
        sess.send_frame(codec::MSG_FOLDER_OFFER, FolderOffer {
            folder_id,
            manifest_id: mine.and_then(|f| f.current_manifest).unwrap_or([0; 32]),
            reserved: 0,
        }.encode())?;
        if with_adverts {
            match mine {
                Some(f) => send_my_adverts(sess, Some(&f.store))?,
                None => send_my_adverts(sess, None)?,
            }
        }
        Ok(())
    };

    match role {
        Role::Initiator => {
            // Announce my folders; read the mirror's reply per folder.
            for f in &cfg.folders {
                announce_one(sess, f)?;
                record_offer(sess, with_adverts, &mut peer_folders, &mut peer_adverts)?;
            }
            sess.send_frame(codec::MSG_FOLDER_OFFER, FolderOffer {
                folder_id: FOLDER_SENTINEL,
                manifest_id: [0; 32],
                reserved: 0,
            }.encode())?;
            // Mirror the peer's own extras.
            loop {
                let po = expect_offer(sess)?;
                if po.folder_id == FOLDER_SENTINEL {
                    break;
                }
                record_offer(sess, with_adverts, &mut peer_folders, &mut peer_adverts)?;
                echo_as_mirror(sess, cfg, po.folder_id)?;
            }
        }
        Role::Responder => {
            // Mirror the initiator's list.
            loop {
                let po = expect_offer(sess)?;
                if po.folder_id == FOLDER_SENTINEL {
                    break;
                }
                record_offer(sess, with_adverts, &mut peer_folders, &mut peer_adverts)?;
                echo_as_mirror(sess, cfg, po.folder_id)?;
            }
            // Announce MY extras (folders the initiator did not mention).
            for f in &cfg.folders {
                if peer_folders.contains_key(&f.folder_id) {
                    continue;
                }
                announce_one(sess, f)?;
                record_offer(sess, with_adverts, &mut peer_folders, &mut peer_adverts)?;
            }
            sess.send_frame(codec::MSG_FOLDER_OFFER, FolderOffer {
                folder_id: FOLDER_SENTINEL,
                manifest_id: [0; 32],
                reserved: 0,
            }.encode())?;
        }
    }

    Ok((peer_folders, peer_adverts))
}

fn expect_offer<S: ByteStream>(sess: &mut Session<'_, S>) -> Result<FolderOffer, ProtoError> {
    let fb = sess.expect_frame(codec::MSG_FOLDER_OFFER)?;
    FolderOffer::parse(&fb.payload)
}

fn record_offer<S: ByteStream>(
    sess: &mut Session<'_, S>,
    with_adverts: bool,
    peer_folders: &mut BTreeMap<[u8; 16], PeerFolder>,
    peer_adverts: &mut BTreeMap<[u8; 16], AdvertMap>,
) -> Result<(), ProtoError> {
    let po = expect_offer(sess)?;
    if with_adverts {
        let map = recv_advert_map(sess)?;
        peer_adverts.insert(po.folder_id, map);
    }
    peer_folders.insert(
        po.folder_id,
        PeerFolder {
            manifest: nonzero_manifest(po.manifest_id),
        },
    );
    Ok(())
}

fn nonzero_manifest(id: BlobId) -> Option<BlobId> {
    if id == [0; 32] {
        None
    } else {
        Some(id)
    }
}

/// Read one folder's advert sequence (until a more=0 frame).
fn recv_advert_map<S: ByteStream>(sess: &mut Session<'_, S>) -> Result<AdvertMap, ProtoError> {
    let mut map = AdvertMap::new();
    loop {
        let fb = sess.expect_frame(codec::MSG_INDEX_ADVERT)?;
        let adv = IndexAdvert::parse(&fb.payload)?;
        for e in adv.entries {
            map.insert(e.id, e);
        }
        if !adv.more {
            return Ok(map);
        }
    }
}

/// Send our index entries for one folder, chunked; a `None` store means we
/// hold nothing for this folder (one empty advert closes the sequence).
fn send_my_adverts<S: ByteStream>(
    sess: &mut Session<'_, S>,
    store: Option<&Arc<Store>>,
) -> Result<(), ProtoError> {
    let entries = match store {
        Some(s) => s.index_entries().map_err(store_err)?,
        None => Vec::new(),
    };
    if entries.is_empty() {
        sess.send_frame(codec::MSG_INDEX_ADVERT, IndexAdvert { entries: vec![], more: false }.encode())?;
        return Ok(());
    }
    let chunks: Vec<&[IndexEntry]> = entries.chunks(IndexAdvert::MAX_ROWS).collect();
    let last = chunks.len() - 1;
    for (i, c) in chunks.into_iter().enumerate() {
        sess.send_frame(codec::MSG_INDEX_ADVERT, IndexAdvert { entries: c.to_vec(), more: i != last }.encode())?;
    }
    Ok(())
}

// --- pulling -------------------------------------------------------------------

/// Fetch a set of blobs of one kind by id, with verification-on-receipt and
/// a re-request budget. Every accepted blob is already stored by the time
/// this returns; corrupt or wrong-id items are counted as rejections and
/// retried.
#[allow(clippy::too_many_arguments)]
fn fetch_blobs<S: ByteStream>(
    sess: &mut Session<'_, S>,
    folder_id: [u8; 16],
    kind: BlobKind,
    want: &[BlobId],
    store: &Arc<Store>,
    retries: u32,
    rejections: &mut usize,
) -> Result<(), ProtoError> {
    let mut outstanding: Vec<BlobId> = want.to_vec();
    for _ in 0..=retries {
        if outstanding.is_empty() {
            return Ok(());
        }
        let mut got: BTreeSet<BlobId> = BTreeSet::new();
        for group in outstanding.chunks(codec::MAX_REQUEST_ITEMS) {
            sess.send_frame(codec::MSG_REQUEST_ITEMS, RequestItems {
                folder_id,
                items: group.iter().map(|id| (kind, *id)).collect(),
            }.encode()?)?;
            got.extend(read_item_batches(sess, store, rejections)?);
        }
        outstanding.retain(|id| !got.contains(id));
    }
    if outstanding.is_empty() {
        Ok(())
    } else {
        Err(ProtoError::MissingItems(outstanding.len()))
    }
}

/// Read ITEM_BATCH frames until the terminator; verify EVERY item against
/// its claimed id AFTER decryption and store only verified bytes. Returns
/// the ids accepted into the store.
fn read_item_batches<S: ByteStream>(
    sess: &mut Session<'_, S>,
    store: &Arc<Store>,
    rejections: &mut usize,
) -> Result<BTreeSet<BlobId>, ProtoError> {
    let mut got = BTreeSet::new();
    loop {
        let fb = sess.expect_frame(codec::MSG_ITEM_BATCH)?;
        let batch = ItemBatch::parse(&fb.payload)?;
        if batch.items.is_empty() {
            return Ok(got);
        }
        for (kind, id, bytes) in batch.items {
            // The verify-after-decrypt rule: BLAKE3(plaintext) MUST equal
            // the claimed id BEFORE anything touches the store.
            if *blake3::hash(&bytes).as_bytes() != id {
                *rejections += 1;
                continue;
            }
            store.put_blob(kind, &bytes).map_err(store_err)?;
            got.insert(id);
        }
    }
}

/// Pack-granular fetch: request whole packs by ciphertext name where the
/// granularity policy says they pay off. Returns the wanted ids satisfied
/// through packs.
fn fetch_via_packs<S: ByteStream>(
    sess: &mut Session<'_, S>,
    folder_id: [u8; 16],
    wanted: &[BlobId],
    adverts: &AdvertMap,
    gran: Granularity,
    store: &Arc<Store>,
    rejections: &mut usize,
) -> Result<BTreeSet<BlobId>, ProtoError> {
    let mut satisfied = BTreeSet::new();
    if matches!(gran, Granularity::ItemsOnly) || wanted.is_empty() {
        return Ok(satisfied);
    }
    let mut by_pack: BTreeMap<BlobId, Vec<BlobId>> = BTreeMap::new();
    for id in wanted {
        if let Some(e) = adverts.get(id) {
            by_pack.entry(e.pack).or_default().push(*id);
        }
    }
    let min_count = match gran {
        Granularity::Auto => 2,
        Granularity::PacksOnly | Granularity::ItemsOnly => 1,
    };
    let packs: Vec<BlobId> = by_pack
        .into_iter()
        .filter(|(_, ids)| ids.len() >= min_count)
        .map(|(p, _)| p)
        .collect();

    for group in packs.chunks(codec::MAX_REQUEST_PACKS) {
        sess.send_frame(codec::MSG_REQUEST_PACKS, RequestPacks {
            folder_id,
            packs: group.to_vec(),
        }.encode()?)?;
        loop {
            let fb = sess.expect_frame_any(&[codec::MSG_PACK_ITEM, codec::MSG_ITEM_BATCH])?;
            if fb.msg_type == codec::MSG_PACK_ITEM {
                let item = PackItem::parse(&fb.payload)?;
                // Verify-before-store at pack level too: the NAME is the
                // hash of the ciphertext; mismatch rejects without any
                // decryption or disk write.
                if *blake3::hash(&item.bytes).as_bytes() != item.pack {
                    *rejections += 1;
                    continue;
                }
                ingest_pack(store, &item.bytes)?;
                for id in wanted {
                    if satisfied.contains(id) {
                        continue;
                    }
                    if adverts
                        .get(id)
                        .map(|e| e.pack == item.pack)
                        .unwrap_or(false)
                        && store.get(BlobKind::DataChunk, id).is_ok()
                    {
                        satisfied.insert(*id);
                    }
                }
            } else {
                let b = ItemBatch::parse(&fb.payload)?;
                if b.items.is_empty() {
                    break;
                }
                return Err(unexpected(codec::MSG_ITEM_BATCH));
            }
        }
    }
    Ok(satisfied)
}

/// Write a received pack into the store under its verified name (temp +
/// rename), then fold its locations into the index via rebuild.
fn ingest_pack(store: &Arc<Store>, bytes: &[u8]) -> Result<BlobId, ProtoError> {
    let name = *blake3::hash(bytes).as_bytes();
    let packs_dir = store.store_dir().join("packs");
    let dest = packs_dir.join(format!("{}.pack", hex(&name)));
    if !dest.exists() {
        let tmp_dir = store.store_dir().join("tmp");
        std::fs::create_dir_all(&tmp_dir).map_err(ProtoError::Io)?;
        let tmp = tmp_dir.join(format!("pull-{}", hex(&name)));
        std::fs::write(&tmp, bytes).map_err(ProtoError::Io)?;
        std::fs::rename(&tmp, &dest).map_err(ProtoError::Io)?;
        store.rebuild_index().map_err(store_err)?;
    }
    Ok(name)
}

/// Full pull of one folder's content from the peer.
#[allow(clippy::too_many_arguments)]
fn pull_folder<S: ByteStream>(
    sess: &mut Session<'_, S>,
    folder_id: [u8; 16],
    store: &Arc<Store>,
    target: BlobId,
    current: Option<BlobId>,
    adverts: &AdvertMap,
    gran: Granularity,
    retries: u32,
    rejections: &mut usize,
) -> Result<(), ProtoError> {
    // 1. The peer's root manifest, by id, verified after receipt.
    fetch_blobs(sess, folder_id, BlobKind::Manifest, &[target], store, retries, rejections)?;
    let man_bytes = store.get(BlobKind::Manifest, &target).map_err(store_err)?;
    let manifest = parse_manifest(&man_bytes)
        .map_err(|_| ProtoError::ProtocolViolation("peer manifest failed to parse"))?;

    // 2. Breadth-first walk of the peer's tree: fetch missing nodes,
    //    accumulate every referenced chunk id.
    let mut queue = vec![manifest.root_tree_id];
    let mut enqueued: BTreeSet<BlobId> = queue.iter().copied().collect();
    let mut wanted_chunks: BTreeSet<BlobId> = BTreeSet::new();
    let mut rounds = 0usize;
    while !queue.is_empty() {
        rounds += 1;
        if rounds > MAX_BFS_ROUNDS {
            return Err(ProtoError::MissingItems(queue.len()));
        }
        let batch = std::mem::take(&mut queue);
        fetch_blobs(sess, folder_id, BlobKind::TreeNode, &batch, store, retries, rejections)?;
        for id in batch {
            let bytes = store.get(BlobKind::TreeNode, &id).map_err(store_err)?;
            let node = parse_tree_node(&bytes)
                .map_err(|_| ProtoError::ProtocolViolation("peer tree node failed to parse"))?;
            for e in node.entries {
                match e.payload {
                    EntryPayload::Dir { child_tree_id } => {
                        if enqueued.insert(child_tree_id) {
                            queue.push(child_tree_id);
                        }
                    }
                    EntryPayload::File { chunks, .. } => {
                        for (cid, _) in chunks {
                            wanted_chunks.insert(cid);
                        }
                    }
                    EntryPayload::Symlink { .. } => {}
                }
            }
        }
    }

    // 3. Chunks missing locally are what we actually need.
    let wanted: Vec<BlobId> = wanted_chunks
        .into_iter()
        .filter(|id| store.get(BlobKind::DataChunk, id).is_err())
        .collect();

    // 4-5. Packs first (policy-driven), items for the remainder.
    let satisfied =
        fetch_via_packs(sess, folder_id, &wanted, adverts, gran, store, rejections)?;
    let leftover: Vec<BlobId> = wanted
        .into_iter()
        .filter(|id| !satisfied.contains(id))
        .collect();
    if !leftover.is_empty() {
        fetch_blobs(sess, folder_id, BlobKind::DataChunk, &leftover, store, retries, rejections)?;
    }

    let _ = current; // adoption handled by run_stage via outcomes
    Ok(())
}

// --- round 2 + agreement ---------------------------------------------------------

/// Re-announce post-pull state; when both sides now hold the same root
/// manifest id for a folder, record the last-agreed pointer locally
/// (ADR-0004 ancestor state).
fn finish_after_sync<S: ByteStream>(
    sess: &mut Session<'_, S>,
    role: Role,
    cfg: &EngineConfig,
    outcomes: &mut [FolderOutcome],
) -> Result<(), ProtoError> {
    let (peer_folders, _) = exchange_offers(sess, role, cfg, false)?;

    for idx in 0..outcomes.len() {
        let folder_id = outcomes[idx].folder_id;
        if let Some(pf) = peer_folders.get(&folder_id) {
            outcomes[idx].remote_manifest = pf.manifest;
        }
        let (mine_now, theirs_now) = (
            outcomes[idx].local_manifest_after,
            outcomes[idx].remote_manifest,
        );
        if let (Some(mine), Some(theirs)) = (mine_now, theirs_now) {
            if mine == theirs {
                let store = find_store(cfg, folder_id)?;
                let ledger = AgreementLedger::new(store.store_dir());
                let (sec, nsec) = now_secs_nsecs();
                ledger
                    .record(
                        &folder_id,
                        &AgreementRecord {
                            peer: sess.peer_id,
                            manifest_id: mine,
                            agreed_sec: sec,
                            agreed_nsec: nsec,
                        },
                    )
                    .map_err(|e| {
                        ProtoError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
                    })?;
                outcomes[idx].agreement_recorded = Some(mine);
            }
        }
    }
    Ok(())
}
