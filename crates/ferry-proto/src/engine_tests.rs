//! In-crate protocol-adversary tests: version negotiation on the wire,
//! unknown-message policy, identity failures, and replay behavior. These
//! need crate internals (handshake io helpers, `Session` construction), so
//! they live inside the crate; the public-API loopback suite is in
//! `tests/acceptance.rs`.

use x25519_dalek::{PublicKey, StaticSecret};

use crate::codec::{self, Bye, FrameBody, Hello, HelloAck, FLAG_EXTENSION_AWARE};
use crate::engine::Session;
use crate::error::ByeReason;
use crate::frame::{read_body, write_body};
use crate::stream::{duplex_pair, ByteStream, DuplexHalf};
use crate::secure::{kdf_handshake, traffic_keys, transcript_hash};
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
    assert!(matches!(err, ProtoError::VersionIncompatible { .. }), "{err}");
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
    assert_eq!(Bye::parse(&reply.payload).unwrap().reason, ByeReason::AuthFailed);

    let err = server.join().unwrap().unwrap_err();
    assert!(
        matches!(
            err,
            ProtoError::IdentityMismatch { .. }
                | ProtoError::Io(_) // peer vanished after BYE
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
fn policy_session<'a>(
    io: &'a mut DuplexHalf,
    peer_max: ProtocolVersion,
    peer_flags: u64,
) -> Session<'a, DuplexHalf> {
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
    write_frame(&mut inject, &FrameBody::new(0x7F, ProtocolVersion::V1_0, vec![]));
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
    write_frame(&mut inject, &FrameBody::new(0x7F, ProtocolVersion::new(1, 5), vec![]));
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
    write_frame(&mut inject, &FrameBody::new(0x7F, ProtocolVersion::V1_0, vec![]));
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
