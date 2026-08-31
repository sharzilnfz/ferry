










use std::time::{Duration, Instant};

use ferry_iroh::{IrohConfig, IrohTransport};
use ferry_sync::Transport as _;

fn mdns_transport(seed_byte: u8, service: String) -> IrohTransport {
    let mut seed = [seed_byte; 32];
    seed[1] = seed_byte.wrapping_mul(7);
    let cfg = IrohConfig {
        secret: Some(seed),
        mdns: Some(ferry_iroh::MdnsSetting {
            service_name: service,
            advertise: true,
        }),
        dial_timeout: Duration::from_secs(5),
        ..Default::default()
    };
    IrohTransport::new(cfg).expect("mdns transport")
}

#[test]
fn two_same_host_endpoints_discover_each_other_and_dial_by_key() {
    
    
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let service = format!("ferry-t009-{unique}");

    let a = mdns_transport(0x2A, service.clone());
    let b = mdns_transport(0x2B, service);

    
    
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

    
    assert!(
        a.routes().resolve_route(&addr_for_frames).is_some(),
        "listener still publishes its own directory entry"
    );
}
