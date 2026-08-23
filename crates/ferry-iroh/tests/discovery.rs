//! ADR-0003's LAN shortcut, proven locally: two endpoints that have NEVER
//! been given each other's address discover one another over multicast DNS
//! and establish sessions addressed by public key alone.
//!
//! Uses what current iroh ships for local discovery:
//! `iroh-mdns-address-lookup` 0.5 (n0-maintained; wraps `swarm-discovery`
//! 0.6). The old `iroh-mdns` crate is gone; this is its successor.
//!
//! Isolation: both endpoints share a unique service name per run so they
//! only ever find each other, not other ferries on the network.

use std::time::{Duration, Instant};

use ferry_iroh::{IrohConfig, IrohTransport};
use ferry_sync::Transport as _;

fn mdns_transport(seed_byte: u8, service: String) -> IrohTransport {
    let mut seed = [seed_byte; 32];
    seed[1] = seed_byte.wrapping_mul(7);
    let cfg = IrohConfig::builder()
        .secret(seed)
        .mdns(ferry_iroh::MdnsSetting {
            service_name: service,
            advertise: true,
        })
        .dial_timeout(Duration::from_secs(5))
        .build();
    IrohTransport::new(cfg).expect("mdns transport")
}

#[test]
fn two_same_host_endpoints_discover_each_other_and_dial_by_key() {
    // Unique service name per run: concurrent test executions and other
    // processes must never cross-contaminate discovery.
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let service = format!("ferry-t009-{unique}");

    let a = mdns_transport(0x2A, service.clone());
    let b = mdns_transport(0x2B, service);

    // Node A announces itself by LISTENING. Note: no routes are registered
    // anywhere; B has no idea where A lives except via mDNS.
    let lst = a
        .listen("127.0.0.1:0".parse().unwrap())
        .expect("A listens (announces via mdns)");
    let addr_for_frames = lst.local_addr().unwrap();

    let server = std::thread::spawn(move || {
        let mut c = lst.accept().expect("discovered peer connects");
        let got = c.recv_frame().expect("frame from discovered peer");
        assert_eq!(got, b"found you by key");
        c.send_frame(b"by public key alone").unwrap();
    });

    // B dials A's PUBLIC KEY directly — no SocketAddr involved.
    let target_id = a.endpoint_id();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut conn = None;
    while Instant::now() < deadline {
        match b.dial_endpoint(target_id, vec![]) {
            Ok(c) => {
                conn = Some(c);
                break;
            }
            Err(_e) => std::thread::sleep(Duration::from_millis(250)),
        }
    }
    let mut conn = conn.expect("mDNS discovery should resolve the peer within 30s");

    conn.send_frame(b"found you by key").unwrap();
    let reply = conn.recv_frame().unwrap();
    assert_eq!(reply, b"by public key alone");
    drop(conn);
    server.join().unwrap();

    // The alias route table was never involved in this exchange.
    assert!(
        ferry_iroh::resolve_route(&addr_for_frames).is_some(),
        "listener still publishes its own directory entry"
    );
}
