


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
    IrohTransport::new(IrohConfig {
        secret: Some(seed),
        ..Default::default()
    })
    .expect("transport builds")
}

fn test_transport_with_routes(seed_byte: u8, routes: ferry_iroh::RouteTable) -> IrohTransport {
    let mut seed = [0u8; 32];
    seed[0] = seed_byte;
    seed[1] = seed_byte;
    rand::thread_rng().fill_bytes(&mut seed[2..]);
    IrohTransport::new(IrohConfig {
        secret: Some(seed),
        routes: Some(routes),
        ..Default::default()
    })
    .expect("transport builds")
}

#[test]
fn frames_round_trip_over_iroh_including_empty_and_multi() {
    let shared = ferry_iroh::RouteTable::new();
    let a = test_transport_with_routes(0xA0, shared.clone());
    let b = test_transport_with_routes(0xB0, shared.clone());

    let lst = a
        .listen("127.0.0.1:0".parse().unwrap())
        .expect("listen publishes a route");
    let addr = lst.local_addr().unwrap();

    let route = a.routes().resolve_route(&addr).expect("route published");
    assert_eq!(route.0.endpoint_id, a.endpoint_id());

    let server = std::thread::spawn(move || {
        let mut c = lst.accept().unwrap();
        let first = c.recv_frame().unwrap();
        let empty = c.recv_frame().unwrap();
        c.send_frame(&empty).unwrap();
        c.send_frame(&first).unwrap();
        
        assert_eq!(c.recv_frame().unwrap_err().kind(), ErrorKind::UnexpectedEof);
    });

    let mut cli = b.dial(addr).expect("dial by alias");
    cli.send_frame(b"over-quic").unwrap();
    cli.send_frame(&[]).unwrap();
    assert_eq!(cli.recv_frame().unwrap(), b"");
    assert_eq!(cli.recv_frame().unwrap(), b"over-quic");
    drop(cli); 
    server.join().unwrap();
}

#[test]
fn large_frames_survive_the_quic_path() {
    
    
    let payload: Vec<u8> = (0..3 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    let payload_hash = blake3_hash(&payload);

    let shared = ferry_iroh::RouteTable::new();
    let a = test_transport_with_routes(0xA1, shared.clone());
    let b = test_transport_with_routes(0xB1, shared.clone());
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
    
    
    
    let mut ghost_key = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut ghost_key);

    let alias: SocketAddr = "127.0.0.1:49771".parse().unwrap();
    let shared = ferry_iroh::RouteTable::new();
    shared.register_explicit_route(
        alias,
        ferry_iroh::Route {
            endpoint_id: ghost_key,
            ip_hints: vec![],
        },
    );

    let short_timeout = IrohTransport::new(IrohConfig {
        secret: Some([9u8; 32]),
        dial_timeout: Duration::from_secs(2),
        routes: Some(shared),
        ..Default::default()
    })
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
    
    
    
}

#[test]
fn self_dial_is_cleanly_refused() {
    let a = test_transport(0xE0);
    let lst = a.listen("127.0.0.1:0".parse().unwrap()).unwrap();
    let own = lst.local_addr().unwrap();

    let err = a.dial(own).err().expect("self-dial must fail");
    assert_eq!(err.kind(), ErrorKind::ConnectionRefused);
    assert!(err.to_string().contains("self-dial"), "{err}");
}

#[test]
fn listener_close_unblocks_accept() {
    let a = test_transport(0xE1);
    let lst: std::sync::Arc<dyn ferry_sync::Listener> =
        std::sync::Arc::from(a.listen("127.0.0.1:0".parse().unwrap()).unwrap());

    let server_lst = std::sync::Arc::clone(&lst);
    let server = std::thread::spawn(move || match server_lst.accept() {
        Err(e) => assert_eq!(e.kind(), ErrorKind::ConnectionAborted),
        Ok(_) => panic!("expected accept to fail after close"),
    });

    std::thread::sleep(Duration::from_millis(50));
    lst.close().unwrap();
    server.join().unwrap();
}
