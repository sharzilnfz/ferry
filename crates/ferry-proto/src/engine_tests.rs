//! In-crate protocol-adversary tests: version negotiation on the wire,
//! unknown-message policy, identity failures, and replay behavior. These
//! need crate internals (handshake io helpers, `Session` construction), so
//! they live inside the crate; the public-API loopback suite is in
//! `tests/acceptance.rs`.

use x25519_dalek::{PublicKey, StaticSecret};

use crate::codec::{self, Bye, FrameBody, Hello, HelloAck, IndexAdvert, FLAG_EXTENSION_AWARE};
use crate::engine::{ingest_pack, recv_advert_map, Session, MAX_ADVERT_ROWS_TOTAL};
use crate::error::ByeReason;
use crate::frame::{read_body, write_body};
use crate::secure::{kdf_handshake, traffic_keys, transcript_hash};
use crate::stream::{duplex_pair, DuplexHalf};
use crate::version::ProtocolVersion;
use crate::{run_engine, EngineConfig, Granularity, ProtoError, Role};

fn test_identity(seed: u8) -> ferry_crypto::identity::DeviceIdentity {
    let mut sk = [0u8; 32];
    for (i, b) in sk.iter_mut().enumerate() {
        *b = seed.wrapping_mul(131).wrapping_add(i as u8);
    }
    ferry_crypto::identity::DeviceIdentity::from_secret_bytes(&sk)
}

fn write_frame(io: &mut DuplexHalf, fb: &FrameBody) {
    let body = fb.encode();
    let mut wire = Vec::with_capacity(4 + body.len());
    wire.extend_from_slice(&(body.len() as u32).to_be_bytes());
    wire.extend_from_slice(&body);
    write_body(io, &wire[4..]).unwrap();
}

fn read_frame(io: &mut DuplexHalf) -> FrameBody {
    FrameBody::parse(&read_body(io).unwrap()).unwrap()
}

fn fresh_ephemeral() -> [u8; 32] {
    *PublicKey::from(&StaticSecret::random_from_rng(rand::rngs::OsRng)).as_bytes()
}

#[test]
fn major_version_mismatch_is_a_clean_bye_disconnect() {
    let responder = test_identity(1);
    let expected_peer = *responder.device_id();

    let (mut dial, listen) = duplex_pair();
    let server = std::thread::spawn(move || {
        run_engine(
            listen,
            Role::Responder,
            EngineConfig {
                identity: responder,
                expected_peer,
                folders: vec![],
                encryption: true,
                granularity: Granularity::Auto,
                max_retries: 1,
            },
        )
    });

    // A conforming-looking initiator advertising major 2.0: correct static
    // id, fresh ephemeral. Only the version differs.
    let hello = Hello {
        version: ProtocolVersion::new(2, 0),
        flags: FLAG_EXTENSION_AWARE,
        eph_pub: fresh_ephemeral(),
        stat_pub: expected_peer,
        nonce: [7; 32],
    };
    write_frame(
        &mut dial,
        &FrameBody::new(codec::MSG_HELLO, ProtocolVersion::new(2, 0), hello.encode()),
    );

    let reply = read_frame(&mut dial);
    assert_eq!(reply.msg_type, codec::MSG_BYE, "clean disconnect uses BYE");
    assert_eq!(
        Bye::parse(&reply.payload).unwrap().reason,
        ByeReason::VersionIncompatible
    );

    let err = server.join().unwrap().unwrap_err();
    assert!(
        matches!(err, ProtoError::VersionIncompatible { .. }),
        "{err}"
    );
}

#[test]
fn stranger_identity_fails_the_handshake_before_any_secrets_move() {
    // The dialer is NOT who the responder expects. The responder must
    // refuse at the Hello (public) layer — nothing encrypted is even
    // attempted — with BYE(AuthFailed).
    let responder = test_identity(2);
    let real_owner = test_identity(3);
    let stranger = test_identity(4);

    let (mut dial, listen) = duplex_pair();
    let server = std::thread::spawn(move || {
        run_engine(
            listen,
            Role::Responder,
            EngineConfig {
                identity: responder,
                expected_peer: *real_owner.device_id(),
                folders: vec![],
                encryption: true,
                granularity: Granularity::Auto,
                max_retries: 1,
            },
        )
    });

    let hello = Hello {
        version: ProtocolVersion::V1_0,
        flags: FLAG_EXTENSION_AWARE,
        eph_pub: fresh_ephemeral(),
        stat_pub: *stranger.device_id(), // wrong device
        nonce: [9; 32],
    };
    write_frame(
        &mut dial,
        &FrameBody::new(codec::MSG_HELLO, ProtocolVersion::V1_0, hello.encode()),
    );

    let reply = read_frame(&mut dial);
    assert_eq!(reply.msg_type, codec::MSG_BYE);
    assert_eq!(
        Bye::parse(&reply.payload).unwrap().reason,
        ByeReason::AuthFailed
    );

    let err = server.join().unwrap().unwrap_err();
    assert!(
        matches!(
            err,
            ProtoError::IdentityMismatch { .. } | ProtoError::Io(_) // peer vanished after BYE
        ),
        "{err}"
    );
}

#[test]
fn replayed_hello_cannot_complete_authentication() {
    // A captured Hello replays fine at the FRAMING layer — the responder
    // answers ACK — but the connection dies at auth unless the replaying
    // party holds the static secret AND can produce a fresh valid proof for
    // THIS connection's transcript (fresh responder ephemeral + nonce).
    // Here no proof arrives; closing yields a clean typed EOF and zero
    // folder side effects.
    let responder = test_identity(5);
    let peer = test_identity(6);
    let peer_id = *peer.device_id();

    let (mut dial, listen) = duplex_pair();
    let server = std::thread::spawn(move || {
        run_engine(
            listen,
            Role::Responder,
            EngineConfig {
                identity: responder,
                expected_peer: peer_id,
                folders: vec![],
                encryption: true,
                granularity: Granularity::Auto,
                max_retries: 1,
            },
        )
    });

    let captured = Hello {
        version: ProtocolVersion::V1_0,
        flags: FLAG_EXTENSION_AWARE,
        eph_pub: fresh_ephemeral(),
        stat_pub: peer_id,
        nonce: [11; 32],
    };
    write_frame(
        &mut dial,
        &FrameBody::new(codec::MSG_HELLO, ProtocolVersion::V1_0, captured.encode()),
    );
    let ack_fb = read_frame(&mut dial);
    assert_eq!(ack_fb.msg_type, codec::MSG_HELLO_ACK);
    let ack = HelloAck::parse(&ack_fb.payload).unwrap();
    assert_eq!(ack.agreed, ProtocolVersion::V1_0);

    // No AUTH_INIT will ever come.
    dial.close();
    let err = server.join().unwrap().unwrap_err();
    assert!(matches!(err, ProtoError::Io(_)), "{err}");
}

// --- Session.recv_frame unknown-type policy ----------------------------------

/// Build a live post-auth `Session` over a duplex half using the REAL key
/// schedule (fabricated DH terms), so sealed frames authenticate normally.
fn policy_session(
    io: &mut DuplexHalf,
    peer_max: ProtocolVersion,
    peer_flags: u64,
) -> Session<'_, DuplexHalf> {
    // tx/rx None = plaintext frames: isolates the TYPE policy from sealing.
    Session {
        io,
        version: ProtocolVersion::V1_0,
        peer_max,
        peer_flags,
        peer_id: [0; 32],
        tx: None,
        rx: None,
    }
}

#[test]
fn unknown_type_same_version_is_a_protocol_violation() {
    let (mut inject, mut inbox) = duplex_pair();
    let mut sess = policy_session(&mut inbox, ProtocolVersion::V1_0, FLAG_EXTENSION_AWARE);
    write_frame(
        &mut inject,
        &FrameBody::new(0x7F, ProtocolVersion::V1_0, vec![]),
    );
    let err = sess.recv_frame().unwrap_err();
    assert!(matches!(err, ProtoError::UnknownMessage { msg_type: 0x7F }));
}

#[test]
fn unknown_type_higher_minor_with_unknown_flags_is_skipped() {
    let (mut inject, mut inbox) = duplex_pair();
    let mut sess = policy_session(
        &mut inbox,
        ProtocolVersion::new(1, 5),
        FLAG_EXTENSION_AWARE | (1 << 6), // flag we do not know
    );
    // Unknown flagged type is consumed INSIDE the next recv_frame call;
    // the following real message is what that call returns.
    write_frame(
        &mut inject,
        &FrameBody::new(0x7F, ProtocolVersion::new(1, 5), vec![]),
    );
    write_frame(
        &mut inject,
        &FrameBody::new(codec::MSG_ITEM_BATCH, ProtocolVersion::V1_0, vec![]),
    );
    let fb = sess.recv_frame().unwrap().unwrap();
    assert_eq!(fb.msg_type, codec::MSG_ITEM_BATCH);
}

#[test]
fn unknown_type_higher_minor_without_new_flags_still_violates() {
    let (mut inject, mut inbox) = duplex_pair();
    let mut sess = policy_session(&mut inbox, ProtocolVersion::new(1, 5), FLAG_EXTENSION_AWARE);
    write_frame(
        &mut inject,
        &FrameBody::new(0x7F, ProtocolVersion::V1_0, vec![]),
    );
    assert!(sess.recv_frame().is_err());
}

#[test]
fn skipped_unknown_types_must_be_sealed_correctly_too() {
    // A "skippable" frame still has to authenticate under the session keys:
    // garbage wrapped in an unknown type is an auth failure, not a skip.
    let (mut inject, mut inbox) = duplex_pair();
    let (_, _, prk) = kdf_handshake(&[0; 32], &[1; 32], &[2; 32], &[3; 32]);
    let th_final = transcript_hash(&[]);
    let (ka, kb) = traffic_keys(&prk, &th_final);
    let mut sess = Session {
        io: &mut inbox,
        version: ProtocolVersion::V1_0,
        peer_max: ProtocolVersion::new(1, 5),
        peer_flags: FLAG_EXTENSION_AWARE | (1 << 6),
        peer_id: [0; 32],
        tx: Some(kb.cipher()),
        rx: Some(ka.cipher()),
    };
    // Write the frame UNSEALED while the session expects sealing. Payload
    // is large enough that the failure is the TAG check, not a length guard.
    let body = FrameBody::new(0x7F, ProtocolVersion::V1_0, vec![1u8; 64]).encode();
    write_body(&mut inject, &body).unwrap();
    let err = sess.recv_frame().unwrap_err();
    assert!(matches!(err, ProtoError::Auth(_)), "{err}");
}

// --- T-016: session-wide receive budgets + crash-safe pack ingest -------------

#[test]
fn endless_more_one_adverts_hit_resource_limit_instead_of_unbounded_growth() {
    // A hostile peer streams MAX_ROWS-row advert frames with more=1 forever.
    // recv_advert_map must stop at the session-wide row budget with a typed
    // ResourceLimit (→ BYE(ResourceLimit) on the wire), never OOM.
    let (mut inject, mut inbox) = duplex_pair();
    let mut sess = policy_session(&mut inbox, ProtocolVersion::V1_0, FLAG_EXTENSION_AWARE);

    // Distinct ids per row: the index-table encoder sorts AND dedups
    // (kind,id) pairs, so repeated rows would collapse to one and never
    // trip any budget.
    let entries_for = |frame: usize| -> Vec<ferry_store::index::IndexEntry> {
        (0..IndexAdvert::MAX_ROWS)
            .map(|j| {
                let n = (frame * IndexAdvert::MAX_ROWS + j) as u64;
                let mut id = [0u8; 32];
                id[..8].copy_from_slice(&n.to_be_bytes());
                ferry_store::index::IndexEntry {
                    kind: ferry_store::format::BlobKind::DataChunk,
                    id,
                    pack: [0u8; 32],
                    plain_off: 0,
                    plain_len: 0,
                }
            })
            .collect()
    };
    // One frame PAST the budget: 129 full frames = 264_192 rows > 262_144.
    let frames = MAX_ADVERT_ROWS_TOTAL / IndexAdvert::MAX_ROWS + 1;
    for f in 0..frames {
        write_frame(
            &mut inject,
            &FrameBody::new(
                codec::MSG_INDEX_ADVERT,
                ProtocolVersion::V1_0,
                IndexAdvert {
                    entries: entries_for(f),
                    more: true,
                }
                .encode(),
            ),
        );
    }

    let err = recv_advert_map(&mut sess).unwrap_err();
    match err {
        ProtoError::ResourceLimit { limit, .. } => assert_eq!(limit, MAX_ADVERT_ROWS_TOTAL),
        other => panic!("expected ResourceLimit, got {other}"),
    }
}

#[test]
fn concurrent_ingest_of_the_same_pack_yields_one_valid_named_pack_and_no_residue() {
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        ferry_store::store::Store::create(
            dir.path(),
            [7u8; 32],
            Box::new(ferry_store::crypto::PassthroughCipher),
        )
        .unwrap(),
    );

    // A real sealed pack (T-15: ingest verifies the BLAKE3 name AND parses
    // the footer before anything touches disk, so raw bytes are rejected).
    let body: Vec<u8> = (0..64 * 1024u32).map(|i| (i % 251) as u8).collect();
    let chunk_id = *blake3::hash(&body).as_bytes();
    let entries = vec![ferry_store::pack::FooterEntry {
        kind: ferry_store::format::BlobKind::DataChunk,
        id: chunk_id,
        plain_off: 0,
        plain_len: body.len() as u64,
    }];
    let salt: [u8; ferry_store::crypto::SALT_LEN] = core::array::from_fn(|i| i as u8);
    let bytes = ferry_store::pack::seal_pack_bytes(
        ferry_store::format::ContainerKind::PackData,
        &[7u8; 32],
        &salt,
        &body,
        &entries,
        &ferry_store::crypto::PassthroughCipher,
    )
    .unwrap();

    // Two "processes" racing on the same store dir. The unique entropy temp
    // names mean neither writer's bytes can interleave into the other's
    // file; both must succeed and the final pack must match its name.
    let s2 = Arc::clone(&store);
    let b2 = bytes.clone();
    let racer = std::thread::spawn(move || ingest_pack(&s2, &b2));
    let here = ingest_pack(&store, &bytes).unwrap();
    let there = racer.join().unwrap().unwrap();
    assert_eq!(here, there, "same bytes ⇒ same verified BLAKE3 name");

    let name = ferry_store::format::hex(&here);
    let store_dir = dir.path().join(ferry_store::store::STORE_DIR_NAME);
    let on_disk = std::fs::read(store_dir.join("packs").join(format!("{name}.pack"))).unwrap();
    assert_eq!(*blake3::hash(&on_disk).as_bytes(), here);
    assert_eq!(on_disk, bytes);

    // Concurrent adoption left exactly one valid location behind: the chunk
    // is readable through the merged table.
    assert_eq!(
        store
            .get(ferry_store::format::BlobKind::DataChunk, &chunk_id)
            .unwrap(),
        body
    );

    // No orphaned temps survive a successful ingest — and the error path
    // removes its temp too (best-effort cleanup in ingest_pack); crash
    // residue is ticket 20's sweeper.
    let residue: Vec<_> = std::fs::read_dir(store_dir.join("tmp"))
        .unwrap()
        .flatten()
        .collect();
    assert!(residue.is_empty(), "{residue:?}");
}
