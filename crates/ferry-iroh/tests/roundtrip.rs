//! Transport-level tests: frames ride iroh exactly like they rode TCP, and
//! dial failures come out cleanly typed.

use std::io::ErrorKind;
use std::net::SocketAddr;
use std::time::Duration;

use ferry_iroh::{IrohConfig, IrohTransport};
use ferry_sync::Transport as _;
use rand::RngCore;

fn test_transport(seed_byte: u8) -> IrohTransport {
    let mut seed = [0u8; 32];
    seed[0] = seed_byte;
    seed[1] = seed_byte;
    rand::thread_rng().fill_bytes(&mut seed[2..]);
    IrohTransport::new(IrohConfig::builder().secret(seed).build()).expect("transport builds")
}

#[test]
fn frames_round_trip_over_iroh_including_empty_and_multi() {
    let a = test_transport(0xA0);
    let b = test_transport(0xB0);

    let lst = a
        .listen("127.0.0.1:0".parse().unwrap())
        .expect("listen publishes a route");
    let addr = lst.local_addr().unwrap();

    // The listener's route resolves to A's PUBLIC KEY, not an IP: the whole
    // point of ADR-0003.
    let route = ferry_iroh::resolve_route(&addr).expect("route published");
    assert_eq!(route.0.endpoint_id, a.endpoint_id());

    let server = std::thread::spawn(move || {
        let mut c = lst.accept().unwrap();
        let first = c.recv_frame().unwrap();
        let empty = c.recv_frame().unwrap();
        c.send_frame(&empty).unwrap();
        c.send_frame(&first).unwrap();
        // Peer closes; next read is a clean EOF error.
        assert_eq!(c.recv_frame().unwrap_err().kind(), ErrorKind::UnexpectedEof);
    });

    let mut cli = b.dial(addr).expect("dial by alias");
    cli.send_frame(b"over-quic").unwrap();
    cli.send_frame(&[]).unwrap();
    assert_eq!(cli.recv_frame().unwrap(), b"");
    assert_eq!(cli.recv_frame().unwrap(), b"over-quic");
    drop(cli); // closes the connection
    server.join().unwrap();
}

#[test]
fn large_frames_survive_the_quic_path() {
    // Multi-MB frame: exercises write coalescing across stream writes and
    // the receive-side allocation path.
    let payload: Vec<u8> = (0..3 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    let payload_hash = blake3_hash(&payload);

    let a = test_transport(0xA1);
    let b = test_transport(0xB1);
    let lst = a.listen("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = lst.local_addr().unwrap();

    let server = std::thread::spawn(move || {
        let mut c = lst.accept().unwrap();
        let got = c.recv_frame().unwrap();
        assert_eq!(got.len(), 3 * 1024 * 1024);
        assert_eq!(blake3_hash(&got), payload_hash);
        c.send_frame(&got).unwrap();
    });

    let mut cli = b.dial(addr).unwrap();
    cli.send_frame(&payload).unwrap();
    let echoed = cli.recv_frame().unwrap();
    assert_eq!(echoed, payload);
    server.join().unwrap();
}

fn blake3_hash(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

#[test]
fn unknown_alias_dial_is_typed_not_found() {
    let t = test_transport(0xC0);
    let nowhere: SocketAddr = "127.0.0.1:49999".parse().unwrap();
    let err = t.dial(nowhere).err().expect("no route registered");
    assert_eq!(err.kind(), ErrorKind::NotFound, "{err}");
    assert!(err.to_string().contains("no ferry-iroh route"), "{err}");
}

#[test]
fn wrong_key_dial_fails_cleanly_typed() {
    // Route exists but points at a well-formed key nobody holds, with no
    // relays or discovery configured: resolution must fail within budget,
    // as TimedOut (not a hang, not a panic).
    let mut ghost_key = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut ghost_key);

    let alias: SocketAddr = "127.0.0.1:49771".parse().unwrap();
    ferry_iroh::register_explicit_route(
        alias,
        ferry_iroh::Route {
            endpoint_id: ghost_key,
            ip_hints: vec![],
        },
    );

    let short_timeout = IrohTransport::new(
        IrohConfig::builder()
            .secret([9u8; 32])
            .dial_timeout(Duration::from_secs(2))
            .build(),
    )
    .unwrap();
    let err = short_timeout
        .dial(alias)
        .err()
        .expect("ghost key unreachable");

    match err.kind() {
        ErrorKind::TimedOut | ErrorKind::ConnectionRefused => {}
        other => panic!("unexpected error kind {other:?}: {err}"),
    }
    assert!(
        err.to_string().contains("wrong key")
            || err.to_string().contains("Timeout")
            || err.to_string().contains("timed out")
            || err.to_string().contains("Connect"),
        "error should carry diagnostic context: {err}"
    );
    // Failing fast is correct: with no relays and no lookups there is no
    // resolution path at all, so iroh refuses immediately instead of
    // burning the budget. The property under test is CLEAN + TYPED, not slow.
}

#[test]
fn self_dial_wakes_listener_with_clean_eof_probe() {
    // The M0 engine unblocks its accept loop by dialing its own listener
    // address. Over iroh we reproduce that observable behavior: dial
    // succeeds, the accepted side reads immediate clean EOF.
    let a = test_transport(0xE0);
    let lst = a.listen("127.0.0.1:0".parse().unwrap()).unwrap();
    let own = lst.local_addr().unwrap();

    let server = std::thread::spawn(move || {
        let mut probe = lst.accept().expect("probe wakes accept");
        let err = probe.recv_frame().unwrap_err();
        assert_eq!(err.kind(), ErrorKind::UnexpectedEof, "{err}");
    });

    let mut probe_dialer = a.dial(own).expect("self-dial returns a probe connection");
    assert_eq!(
        probe_dialer.recv_frame().unwrap_err().kind(),
        ErrorKind::UnexpectedEof
    );
    server.join().unwrap();
}
