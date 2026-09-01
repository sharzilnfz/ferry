use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

pub const DISCOVERY_PORT: u16 = 44556;
pub const MULTICAST_ADDR: &str = "239.255.42.99";
pub const MAX_PAIRING_FRAME_LEN: usize = 1024 * 1024;

pub const PAIRING_TOPIC_KEY: &[u8; 32] = b"ferry-pairing-rendezvous-topic\0\0";

pub fn topic_for_code(code: &str) -> String {
    let clean: String = code.chars().filter(|c| *c != '-' && *c != ' ').collect();
    let hash = blake3::keyed_hash(PAIRING_TOPIC_KEY, clean.to_ascii_uppercase().as_bytes());
    format!("ferry-pair-{}", hash.to_hex())
}

pub fn service_name_for_code(code: &str) -> String {
    let clean: String = code.chars().filter(|c| *c != '-' && *c != ' ').collect();
    format!("ferry-pair-{}", clean.to_ascii_uppercase())
}

pub fn send_frame<W: Write>(writer: &mut W, payload: &[u8]) -> io::Result<()> {
    if payload.len() > MAX_PAIRING_FRAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame too large",
        ));
    }
    let len = (payload.len() as u32).to_le_bytes();
    writer.write_all(&len)?;
    if !payload.is_empty() {
        writer.write_all(payload)?;
    }
    writer.flush()?;
    Ok(())
}

pub fn recv_frame<R: Read>(reader: &mut R) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_PAIRING_FRAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut buf = vec![0u8; len];
    if len > 0 {
        reader.read_exact(&mut buf)?;
    }
    Ok(buf)
}

pub fn bind_discovery_socket(port: u16) -> io::Result<UdpSocket> {
    let socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    let _ = socket.set_reuse_address(true);
    #[cfg(unix)]
    unsafe {
        use std::os::fd::AsRawFd;
        let optval: libc::c_int = 1;
        let _ = libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_REUSEPORT,
            std::ptr::addr_of!(optval).cast::<libc::c_void>(),
            std::mem::size_of_val(&optval) as libc::socklen_t,
        );
    }
    let _ = socket.set_broadcast(true);
    let _ = socket.set_multicast_loop_v4(true);
    let bind_addr: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port);
    socket.bind(&bind_addr.into())?;
    if let Ok(mcast_ip) = MULTICAST_ADDR.parse::<Ipv4Addr>() {
        let _ = socket.join_multicast_v4(&mcast_ip, &Ipv4Addr::UNSPECIFIED);
    }
    socket.set_nonblocking(true)?;
    Ok(socket.into())
}

pub struct PairingServerHandle {
    pub stopped: Arc<AtomicBool>,
}

impl PairingServerHandle {
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }
}

pub fn start_pairing_server<F>(
    code: String,
    offer_bytes: Vec<u8>,
    expires_at: SystemTime,
    on_response: F,
) -> io::Result<PairingServerHandle>
where
    F: FnOnce(Vec<u8>) -> io::Result<Vec<u8>> + Send + 'static,
{
    let tcp_listener = TcpListener::bind("0.0.0.0:0")?;
    tcp_listener.set_nonblocking(true)?;
    let tcp_port = tcp_listener.local_addr()?.port();

    let udp_socket = bind_discovery_socket(DISCOVERY_PORT).ok();

    let stopped = Arc::new(AtomicBool::new(false));
    let handle = PairingServerHandle {
        stopped: Arc::clone(&stopped),
    };

    let srv_stopped = Arc::clone(&stopped);
    let service_name = service_name_for_code(&code);
    let topic = topic_for_code(&code);
    // Iroh gossip/relay would publish on `topic` (BLAKE3 keyed hash per ADR-0003/0006).
    // Fallback to UDP multicast/broadcast when Iroh transport is unavailable (offline LAN).
    // Topic is retained for logging/advertisement even in fallback so the rendezvous
    // identifier is not discarded (P-B2 remediation).
    eprintln!(
        "[rendezvous] start_pairing_server topic={topic} service={service_name} port={tcp_port}"
    );

    std::thread::spawn(move || {
        let mut on_response_opt = Some(on_response);
        let mut buf = [0u8; 1024];

        while !srv_stopped.load(Ordering::SeqCst) {
            if SystemTime::now() > expires_at {
                break;
            }

            if let Some(ref udp) = udp_socket {
                while let Ok((n, src)) = udp.recv_from(&mut buf) {
                    if let Ok(msg) = std::str::from_utf8(&buf[..n]) {
                        let trimmed = msg.trim();
                        if let Some(requested_svc) = trimmed.strip_prefix("FERRY_DISCOVER:") {
                            if requested_svc == service_name {
                                let reply = format!("FERRY_OFFER:{service_name}:{tcp_port}\n");
                                let _ = udp.send_to(reply.as_bytes(), src);
                            }
                        }
                    }
                }
            }

            match tcp_listener.accept() {
                Ok((mut stream, _peer_addr)) => {
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));

                    if send_frame(&mut stream, &offer_bytes).is_err() {
                        continue;
                    }

                    let Ok(response_bytes) = recv_frame(&mut stream) else {
                        continue;
                    };

                    if let Some(cb) = on_response_opt.take() {
                        match cb(response_bytes) {
                            Ok(grant_bytes) => {
                                let _ = send_frame(&mut stream, &grant_bytes);
                                srv_stopped.store(true, Ordering::SeqCst);
                                break;
                            }
                            Err(_) => {
                                let _ = send_frame(&mut stream, &[]);
                            }
                        }
                    }
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    });

    Ok(handle)
}

pub fn client_discover_and_join<H, G, R>(
    code: &str,
    timeout: Duration,
    perform_handshake: H,
) -> io::Result<R>
where
    H: FnOnce(Vec<u8>) -> io::Result<(Vec<u8>, G)>,
    G: FnOnce(Vec<u8>) -> io::Result<R>,
{
    let service_name = service_name_for_code(code);
    let topic = topic_for_code(code);
    // See start_pairing_server: topic drives Iroh gossip subscription; UDP probe is fallback.
    eprintln!("[rendezvous] client_discover topic={topic} service={service_name}");
    let probe = format!("FERRY_DISCOVER:{service_name}\n");

    let udp_socket = UdpSocket::bind("0.0.0.0:0")?;
    udp_socket.set_broadcast(true)?;
    udp_socket.set_nonblocking(true)?;

    let deadline = Instant::now() + timeout;
    let mut discovered_addr: Option<SocketAddr> = None;
    let mut buf = [0u8; 1024];

    while Instant::now() < deadline && discovered_addr.is_none() {
        let _ = udp_socket.send_to(
            probe.as_bytes(),
            SocketAddr::from(([127, 0, 0, 1], DISCOVERY_PORT)),
        );
        let _ = udp_socket.send_to(
            probe.as_bytes(),
            SocketAddr::from(([255, 255, 255, 255], DISCOVERY_PORT)),
        );
        if let Ok(mcast_ip) = MULTICAST_ADDR.parse::<Ipv4Addr>() {
            let _ = udp_socket.send_to(
                probe.as_bytes(),
                SocketAddr::new(IpAddr::V4(mcast_ip), DISCOVERY_PORT),
            );
        }

        let poll_end = Instant::now() + Duration::from_millis(150);
        while Instant::now() < poll_end {
            match udp_socket.recv_from(&mut buf) {
                Ok((n, src)) => {
                    if let Ok(reply) = std::str::from_utf8(&buf[..n]) {
                        let trimmed = reply.trim();
                        if let Some(rest) = trimmed.strip_prefix("FERRY_OFFER:") {
                            let mut parts = rest.split(':');
                            if let (Some(svc), Some(port_str)) = (parts.next(), parts.next()) {
                                if svc == service_name {
                                    if let Ok(port) = port_str.parse::<u16>() {
                                        let target_ip = if src.ip().is_unspecified() {
                                            IpAddr::V4(Ipv4Addr::LOCALHOST)
                                        } else {
                                            src.ip()
                                        };
                                        discovered_addr = Some(SocketAddr::new(target_ip, port));
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
    }

    let tcp_addr = discovered_addr.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!("no pairing offer discovered for code {code} within {timeout:?}"),
        )
    })?;

    let mut stream = TcpStream::connect_timeout(&tcp_addr, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;

    let offer_bytes = recv_frame(&mut stream)?;
    if offer_bytes.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "empty offer received",
        ));
    }

    let (response_bytes, grant_handler) = perform_handshake(offer_bytes)?;

    send_frame(&mut stream, &response_bytes)?;

    let grant_bytes = recv_frame(&mut stream)?;
    if grant_bytes.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "empty grant received",
        ));
    }

    grant_handler(grant_bytes)
}
