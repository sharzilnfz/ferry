use x25519_dalek::{PublicKey, StaticSecret};

use crate::codec::{self, Bye, FrameBody, Hello, HelloAck, IndexAdvert, FLAG_EXTENSION_AWARE};
use crate::engine::{ingest_pack, recv_advert_map, MAX_ADVERT_ROWS_TOTAL};
use crate::error::ByeReason;
use crate::frame::{read_body, write_body};
use crate::secure::{kdf_handshake, traffic_keys, transcript_hash, SecureSession};
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
        stat_pub: *stranger.device_id(),
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
        matches!(err, ProtoError::IdentityMismatch { .. } | ProtoError::Io(_)),
        "{err}"
    );
}

#[test]
fn replayed_hello_cannot_complete_authentication() {
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

    dial.close();
    let err = server.join().unwrap().unwrap_err();
    assert!(matches!(err, ProtoError::Io(_)), "{err}");
}

fn policy_session(
    io: DuplexHalf,
    peer_max: ProtocolVersion,
    peer_flags: u64,
) -> SecureSession<DuplexHalf> {
    SecureSession::from_parts(
        io,
        ProtocolVersion::V1_0,
        peer_max,
        peer_flags,
        [0; 32],
        None,
        None,
    )
}

#[test]
fn unknown_type_same_version_is_a_protocol_violation() {
    let (mut inject, inbox) = duplex_pair();
    let mut sess = policy_session(inbox, ProtocolVersion::V1_0, FLAG_EXTENSION_AWARE);
    write_frame(
        &mut inject,
        &FrameBody::new(0x7F, ProtocolVersion::V1_0, vec![]),
    );
    let err = sess.recv_frame().unwrap_err();
    assert!(matches!(err, ProtoError::UnknownMessage { msg_type: 0x7F }));
}

#[test]
fn unknown_type_higher_minor_with_unknown_flags_is_skipped() {
    let (mut inject, inbox) = duplex_pair();
    let mut sess = policy_session(
        inbox,
        ProtocolVersion::new(1, 5),
        FLAG_EXTENSION_AWARE | (1 << 6),
    );

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
    let (mut inject, inbox) = duplex_pair();
    let mut sess = policy_session(inbox, ProtocolVersion::new(1, 5), FLAG_EXTENSION_AWARE);
    write_frame(
        &mut inject,
        &FrameBody::new(0x7F, ProtocolVersion::V1_0, vec![]),
    );
    assert!(sess.recv_frame().is_err());
}

#[test]
fn skipped_unknown_types_must_be_sealed_correctly_too() {
    let (mut inject, inbox) = duplex_pair();
    let (_, _, prk) = kdf_handshake(&[0; 32], &[1; 32], &[2; 32], &[3; 32]);
    let th_final = transcript_hash(&[]);
    let (ka, kb) = traffic_keys(&prk, &th_final);
    let mut sess = SecureSession::from_parts(
        inbox,
        ProtocolVersion::V1_0,
        ProtocolVersion::new(1, 5),
        FLAG_EXTENSION_AWARE | (1 << 6),
        [0; 32],
        Some(kb.cipher()),
        Some(ka.cipher()),
    );

    let body = FrameBody::new(0x7F, ProtocolVersion::V1_0, vec![1u8; 64]).encode();
    write_body(&mut inject, &body).unwrap();
    let err = sess.recv_frame().unwrap_err();
    assert!(matches!(err, ProtoError::Auth(_)), "{err}");
}

#[test]
fn endless_more_one_adverts_hit_resource_limit_instead_of_unbounded_growth() {
    let (mut inject, inbox) = duplex_pair();
    let mut sess = policy_session(inbox, ProtocolVersion::V1_0, FLAG_EXTENSION_AWARE);

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

    assert_eq!(
        store
            .get(ferry_store::format::BlobKind::DataChunk, &chunk_id)
            .unwrap(),
        body
    );

    let residue: Vec<_> = std::fs::read_dir(store_dir.join("tmp"))
        .unwrap()
        .flatten()
        .collect();
    assert!(residue.is_empty(), "{residue:?}");
}
