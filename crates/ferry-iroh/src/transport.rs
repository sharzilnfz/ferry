//! [`IrohTransport`]: the M0 `Transport` seam over iroh QUIC endpoints.
//!
//! Sync trait, async library: the transport owns a private tokio runtime on
//! which the iroh endpoint lives; every blocking call bridges through
//! `block_on`. Frames keep the exact TCP wire shape (u32 LE length prefix),
//! so the protocol layer cannot tell transports apart.
//!
//! iroh types stop here. Nothing in this file's public surface names one.

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::lock::lock;

use ferry_sync::transport::{
    Connection as DynConnection, Listener as DynListener, PeerId, MAX_FRAME_BYTES,
};
use ferry_sync::Transport;
use iroh::endpoint::{presets, Connection as IrohConn};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, TransportAddr, Watcher as _};
use tokio::runtime::{Handle, Runtime};

use crate::config::{IrohConfig, RelaySetting};
use crate::directory::{Route, RouteTable};
use crate::FERRY_ALPN;

/// Where accepted/dialed connections' path choices get recorded, per peer.
///
/// Sampled from a background task while each connection lives. This is how
/// tests assert "went through relay" / "upgraded to direct" without any
/// engine awareness — ADR-0003's negotiation, observed at the seam.
#[derive(Debug, Default)]
pub struct PathObservation {
    /// A relay-borne path was the selected transmission path at least once.
    pub selected_relay_seen: AtomicBool,
    /// A direct IP path was the selected transmission path at least once.
    pub selected_ip_seen: AtomicBool,
}

impl PathObservation {
    /// True when traffic has been observed riding only relays so far.
    pub fn relay_only_so_far(&self) -> bool {
        self.selected_relay_seen.load(Ordering::SeqCst)
            && !self.selected_ip_seen.load(Ordering::SeqCst)
    }
}

/// Typed dial failures. They map onto `io::ErrorKind`s for the trait
/// boundary but stay distinguishable for tests and logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialFailure {
    /// No route registered for this alias.
    NoRoute(SocketAddr),
    /// Refusing to connect to ourselves.
    SelfDial,
    /// The route resolved, but no usable path appeared in time.
    Timeout,
    /// iroh refused/failed the connection attempt.
    Connect(String),
}

impl DialFailure {
    fn into_io(self) -> io::Error {
        use io::ErrorKind::*;
        let (kind, msg) = match &self {
            DialFailure::NoRoute(a) => (NotFound, format!("no ferry-iroh route for {a}")),
            DialFailure::SelfDial => (ConnectionRefused, "refusing self-dial".into()),
            DialFailure::Timeout => (
                TimedOut,
                "peer did not become reachable (no discovery hit, relay unreachable, \
                 or wrong key)"
                    .into(),
            ),
            DialFailure::Connect(detail) => (ConnectionRefused, detail.clone()),
        };
        io::Error::new(kind, format!("dial failed ({self:?}): {msg}"))
    }
}

struct Inner {
    rt: Runtime,
    ep: Endpoint,
    my_id: EndpointId,
    dial_timeout: Duration,
    closed: AtomicBool,
    observations: Mutex<HashMap<[u8; 32], Arc<PathObservation>>>,
    /// Relay URLs from config. Attached to dialed `EndpointAddrs` so
    /// key-only dialing can resolve through OUR relay (deployment rule:
    /// peers we sync with are clients of the same self-hosted relay).
    relay_urls: Vec<iroh::RelayUrl>,
    routes: RouteTable,
}

impl Inner {
    fn observe(&self, id: EndpointId) -> Arc<PathObservation> {
        let mut map = lock(&self.observations);
        map.entry(*id.as_bytes())
            .or_insert_with(|| Arc::new(PathObservation::default()))
            .clone()
    }
}

/// The T-009 transport: iroh QUIC endpoints addressed by derived device
/// public keys, behind [`ferry_sync::Transport`].
#[derive(Clone)]
pub struct IrohTransport {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for IrohTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IrohTransport")
            .field(
                "endpoint",
                &crate::identity::id_short(self.inner.my_id.as_bytes()),
            )
            .finish_non_exhaustive()
    }
}

impl IrohTransport {
    /// Build with [`IrohConfig`].
    pub fn new(cfg: IrohConfig) -> io::Result<Self> {
        let seed = cfg.resolve_secret().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "iroh transport needs a secret or a device identity",
            )
        })?;

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| io::Error::other(format!("tokio runtime: {e}")))?;

        let ep = rt.block_on(build_endpoint(&cfg, seed))?;
        let my_id = ep.id();
        let relay_urls = match &cfg.relays {
            RelaySetting::Custom(urls) => urls.iter().filter_map(|u| u.parse().ok()).collect(),
            _ => Vec::new(),
        };
        let routes = cfg.routes.unwrap_or_default();

        Ok(IrohTransport {
            inner: Arc::new(Inner {
                rt,
                ep,
                my_id,
                dial_timeout: cfg.dial_timeout,
                closed: AtomicBool::new(false),
                observations: Mutex::new(HashMap::new()),
                relay_urls,
                routes,
            }),
        })
    }

    /// Access this transport's route table.
    pub fn routes(&self) -> &RouteTable {
        &self.inner.routes
    }

    /// Access this transport's route table.
    pub fn route_table(&self) -> &RouteTable {
        &self.inner.routes
    }

    /// Register an explicit route for this transport instance:
    /// route-key → peer endpoint id.
    pub fn with_route(&self, key: SocketAddr, endpoint_id: [u8; 32]) -> &Self {
        let route = Route {
            endpoint_id,
            ip_hints: Vec::new(),
        };
        self.inner
            .routes
            .register_explicit_route(key, route.clone());
        crate::directory::register_explicit_route(key, route);
        self
    }

    /// Register a peer identity explicitly into this transport's route table,
    /// returning a fresh synthesized route key.
    pub fn register_peer(&self, endpoint_id: [u8; 32]) -> SocketAddr {
        let key = self.inner.routes.register_peer(endpoint_id, Vec::new());
        crate::directory::register_explicit_route(
            key,
            Route {
                endpoint_id,
                ip_hints: Vec::new(),
            },
        );
        key
    }

    /// This endpoint's public id — print it; peers dial by it.
    pub fn endpoint_id(&self) -> [u8; 32] {
        *self.inner.my_id.as_bytes()
    }

    /// Dial a peer directly by 32-byte public key identity.
    pub fn dial_peer(&self, endpoint_id: &[u8; 32]) -> Result<Box<dyn DynConnection>, DialFailure> {
        let hints = self
            .inner
            .routes
            .resolve_peer(endpoint_id)
            .map(|r| r.ip_hints)
            .unwrap_or_default();
        self.dial_endpoint(*endpoint_id, hints)
    }

    /// Publish an ephemeral pairing rendezvous topic pointing to this endpoint.
    pub fn publish_topic(&self, topic: [u8; 32]) {
        let route = Route {
            endpoint_id: *self.inner.my_id.as_bytes(),
            ip_hints: direct_hints(&self.inner.ep),
        };
        self.inner.routes.publish_topic(topic, route.clone());
        crate::directory::publish_topic(topic, route);
    }

    /// Dial a peer by an ephemeral pairing rendezvous topic.
    pub fn dial_topic(&self, topic: &[u8; 32]) -> Result<Box<dyn DynConnection>, DialFailure> {
        let route = self
            .inner
            .routes
            .resolve_topic(topic)
            .or_else(|| crate::directory::resolve_topic(topic));
        if let Some(r) = route {
            self.dial_endpoint(r.endpoint_id, r.ip_hints)
        } else {
            let hints = self
                .inner
                .routes
                .resolve_peer(topic)
                .map(|r| r.ip_hints)
                .unwrap_or_default();
            self.dial_endpoint(*topic, hints)
        }
    }

    /// Dial straight by public key — the ADR-0003 primitive that alias
    /// dialing resolves into. `hints` are optional direct addresses; with
    /// relays or discovery configured, an empty hint list still connects.
    pub fn dial_endpoint(
        &self,
        endpoint_id: [u8; 32],
        hints: Vec<SocketAddr>,
    ) -> Result<Box<dyn DynConnection>, DialFailure> {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err(DialFailure::Connect("transport closed".into()));
        }
        let id = EndpointId::from_bytes(&endpoint_id)
            .map_err(|_| DialFailure::Connect("malformed endpoint id".into()))?;
        if id == self.inner.my_id {
            return Err(DialFailure::SelfDial);
        }
        let mut addr = EndpointAddr::new(id);
        for h in hints {
            addr.addrs.insert(TransportAddr::Ip(h));
        }
        // Our relays are legitimate addressing information for peers we
        // sync with (same self-hosted relay, ADR-0003). With IP transports
        // stripped (force_relay) this is REQUIRED: without it iroh refuses
        // with "no addressing information available".
        for url in &self.inner.relay_urls {
            addr.addrs.insert(TransportAddr::Relay(url.clone()));
        }
        // Connect AND open our bi-stream in one async step: the dialer gets
        // back a ready pipe, and accept_bi on the peer resolves immediately.
        let ep = self.inner.ep.clone();
        let budget = self.inner.dial_timeout;
        let opened = self.inner.rt.block_on(async move {
            match tokio::time::timeout(budget, async {
                let conn = ep
                    .connect(addr, FERRY_ALPN)
                    .await
                    .map_err(|e| format!("{e:#}"))?;
                let streams = conn.open_bi().await.map_err(|e| format!("{e:#}"))?;
                Ok::<
                    (
                        IrohConn,
                        (iroh::endpoint::SendStream, iroh::endpoint::RecvStream),
                    ),
                    String,
                >((conn, streams))
            })
            .await
            {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => Err(e),
                Err(_) => Err("timed out waiting for peer path".into()),
            }
        });
        let (conn, (send, recv)) = match opened {
            Ok(v) => v,
            Err(detail) => return Err(DialFailure::Connect(detail)),
        };
        let obs = self.inner.observe(conn.remote_id());
        spawn_path_sampler(self.inner.rt.handle(), &conn, obs);
        Ok(Box::new(FramedConnection {
            rt: self.inner.rt.handle().clone(),
            conn,
            send: Mutex::new(send),
            recv: Mutex::new(recv),
        }))
    }

    /// Path observations recorded so far for a peer id, if we ever
    /// connected to or accepted from it.
    pub fn path_observation(&self, endpoint_id: &[u8; 32]) -> Option<Arc<PathObservation>> {
        self.inner
            .observations
            .lock()
            .unwrap()
            .get(endpoint_id)
            .cloned()
    }

    /// Best-effort graceful close; also runs on Drop.
    pub fn shutdown(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        let ep = self.inner.ep.clone();
        let _ = self.inner.rt.block_on(async move {
            tokio::time::timeout(Duration::from_millis(500), ep.close()).await
        });
    }
}

/// Direct-address hints for a freshly bound endpoint.
fn direct_hints(ep: &Endpoint) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    for bound in ep.bound_sockets() {
        match bound {
            SocketAddr::V4(_) => out.push(SocketAddr::from(([127, 0, 0, 1], bound.port()))),
            SocketAddr::V6(_) => {
                out.push(SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], bound.port())));
            }
        }
    }
    for ip in ep.watch_addr().get().ip_addrs() {
        if !out.contains(ip) {
            out.push(*ip);
        }
    }
    out
}

impl Drop for IrohTransport {
    fn drop(&mut self) {
        // Only when the last clone goes; Arc gives us that for free.
        if Arc::strong_count(&self.inner) == 1 && !self.inner.closed.load(Ordering::SeqCst) {
            self.shutdown();
        }
    }
}

async fn build_endpoint(cfg: &IrohConfig, seed: [u8; 32]) -> io::Result<Endpoint> {
    let mut builder = Endpoint::builder(presets::Minimal)
        .secret_key(iroh::SecretKey::from(seed))
        .alpns(vec![FERRY_ALPN.to_vec()]);

    builder = match &cfg.relays {
        RelaySetting::Disabled => builder.relay_mode(RelayMode::Disabled),
        RelaySetting::N0 => builder.relay_mode(RelayMode::Default),
        RelaySetting::Custom(urls) => {
            let parsed: Result<Vec<iroh::RelayUrl>, _> = urls.iter().map(|u| u.parse()).collect();
            let urls = parsed.map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidInput, format!("relay url: {e}"))
            })?;
            builder.relay_mode(RelayMode::custom(urls))
        }
    };

    if let Some(mdns) = &cfg.mdns {
        builder = builder.address_lookup(
            iroh_mdns_address_lookup::MdnsAddressLookup::builder()
                .service_name(mdns.service_name.clone())
                .advertise(mdns.advertise),
        );
    }

    if cfg.force_relay {
        // "direct disabled by config": strip every IP transport so all bytes
        // must transit a relay, even between same-host peers.
        builder = builder.clear_ip_transports();
    }

    builder
        .bind()
        .await
        .map_err(|e| io::Error::other(format!("iroh endpoint bind: {e:#}")))
}

fn spawn_path_sampler(handle: &Handle, conn: &IrohConn, obs: Arc<PathObservation>) {
    let conn = conn.clone();
    handle.spawn(async move {
        // Observations are monotone latches: one sample at open, one final
        // sample once the connection closes (post-upgrade state included),
        // no polling in between.
        latch_selected_paths(&conn, &obs);
        let _ = conn.closed().await;
        latch_selected_paths(&conn, &obs);
    });
}

fn latch_selected_paths(conn: &IrohConn, obs: &PathObservation) {
    for path in conn.paths().iter() {
        if path.is_selected() {
            if path.is_relay() {
                obs.selected_relay_seen.store(true, Ordering::SeqCst);
            }
            if path.is_ip() {
                obs.selected_ip_seen.store(true, Ordering::SeqCst);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Transport impl: alias dialing over explicit routes + the route table.
// ---------------------------------------------------------------------------

impl Transport for IrohTransport {
    fn dial(&self, addr: SocketAddr) -> io::Result<Box<dyn DynConnection>> {
        let route = self
            .inner
            .routes
            .resolve_route(&addr)
            .or_else(|| crate::directory::resolve_route(&addr));
        let Some((route, _scope)) = route else {
            return Err(DialFailure::NoRoute(addr).into_io());
        };
        self.dial_endpoint(route.endpoint_id, route.ip_hints)
            .map_err(DialFailure::into_io)
    }

    fn dial_peer(&self, peer: &PeerId) -> io::Result<Box<dyn DynConnection>> {
        self.dial_peer(peer).map_err(DialFailure::into_io)
    }

    fn listen(&self, addr: SocketAddr) -> io::Result<Box<dyn DynListener>> {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "transport closed",
            ));
        }
        let key = if addr.port() == 0 {
            self.inner.routes.next_synth_key(addr.ip())
        } else {
            addr
        };
        // Publish ourselves so dialers resolve this key to our public key + real bound sockets.
        let route = Route {
            endpoint_id: *self.inner.my_id.as_bytes(),
            ip_hints: direct_hints(&self.inner.ep),
        };
        self.inner.routes.publish_route(key, route.clone());
        crate::directory::publish_route(key, route);
        Ok(Box::new(IrohListener {
            inner: Arc::clone(&self.inner),
            key,
            closed: Arc::new(AtomicBool::new(false)),
        }))
    }
}

struct IrohListener {
    inner: Arc<Inner>,
    key: SocketAddr,
    closed: Arc<AtomicBool>,
}

impl DynListener for IrohListener {
    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.key)
    }

    fn close(&self) -> io::Result<()> {
        self.closed.store(true, Ordering::SeqCst);
        let ep = self.inner.ep.clone();
        let _ = self.inner.rt.block_on(async move {
            tokio::time::timeout(Duration::from_millis(200), ep.close()).await
        });
        Ok(())
    }

    fn accept(&self) -> io::Result<Box<dyn DynConnection>> {
        loop {
            if self.closed.load(Ordering::SeqCst) || self.inner.closed.load(Ordering::SeqCst) {
                return Err(io_error(
                    io::ErrorKind::ConnectionAborted,
                    "listener closed".into(),
                ));
            }
            let next = self.inner.rt.block_on(async {
                tokio::time::timeout(Duration::from_millis(200), self.inner.ep.accept()).await
            });
            let incoming = match next {
                Ok(Some(incoming)) => incoming,
                Ok(None) => {
                    return Err(io_error(
                        io::ErrorKind::ConnectionAborted,
                        "endpoint closed".into(),
                    ));
                }
                Err(_elapsed) => continue,
            };
            let conn = self.inner.rt.block_on(async {
                match incoming.accept() {
                    Ok(accepting) => accepting
                        .await
                        .map_err(|e| io::Error::other(format!("handshake: {e:#}"))),
                    Err(e) => Err(io::Error::other(format!("incoming accept: {e:#}"))),
                }
            })?;
            let obs = self.inner.observe(conn.remote_id());
            spawn_path_sampler(self.inner.rt.handle(), &conn, obs);

            let streams = self
                .inner
                .rt
                .block_on(conn.accept_bi())
                .map_err(|e| io::Error::other(format!("accept_bi: {e:#}")))?;
            return Ok(Box::new(FramedConnection {
                rt: self.inner.rt.handle().clone(),
                conn,
                send: Mutex::new(streams.0),
                recv: Mutex::new(streams.1),
            }));
        }
    }
}

// ---------------------------------------------------------------------------
// One framed connection type; wire shape identical to TcpTransport's frames.
// ---------------------------------------------------------------------------

struct FramedConnection {
    rt: Handle,
    #[allow(dead_code)]
    conn: IrohConn,
    send: Mutex<iroh::endpoint::SendStream>,
    recv: Mutex<iroh::endpoint::RecvStream>,
}

fn io_error(kind: io::ErrorKind, msg: String) -> io::Error {
    io::Error::new(kind, msg)
}

fn check_outgoing_len(n: usize) -> io::Result<()> {
    let Ok(len) = u32::try_from(n) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame exceeds u32 length prefix",
        ));
    };
    check_incoming_len(len)
}

fn check_incoming_len(len: u32) -> io::Result<()> {
    if len > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    Ok(())
}

impl DynConnection for FramedConnection {
    fn send_frame(&mut self, payload: &[u8]) -> io::Result<()> {
        check_outgoing_len(payload.len())?;
        let mut send = lock(&self.send);
        let len = (payload.len() as u32).to_le_bytes();
        self.rt.block_on(async {
            send.write_all(&len).await?;
            if !payload.is_empty() {
                send.write_all(payload).await?;
            }
            Ok::<(), io::Error>(())
        })
    }

    fn recv_frame(&mut self) -> io::Result<Vec<u8>> {
        let mut recv = lock(&self.recv);
        let mut len_buf = [0u8; 4];
        self.rt.block_on(async {
            match recv.read_exact(&mut len_buf).await {
                Ok(()) => {}
                // FinishedEarly(0): peer closed cleanly at a frame boundary.
                Err(iroh::endpoint::ReadExactError::FinishedEarly(0)) => {
                    return Err(io_error(
                        io::ErrorKind::UnexpectedEof,
                        "peer closed cleanly".into(),
                    ));
                }
                Err(iroh::endpoint::ReadExactError::FinishedEarly(n)) => {
                    return Err(io_error(
                        io::ErrorKind::UnexpectedEof,
                        format!("peer closed mid-frame after {n} length bytes"),
                    ));
                }
                Err(e @ iroh::endpoint::ReadExactError::ReadError(_)) => {
                    return Err(io::Error::other(format!("recv: {e:#}")));
                }
            }
            let len = u32::from_le_bytes(len_buf);
            check_incoming_len(len)?;
            let mut payload = vec![0u8; len as usize];
            if len > 0 {
                recv.read_exact(&mut payload)
                    .await
                    .map_err(|e| io::Error::other(format!("recv payload: {e:#}")))?;
            }
            Ok(payload)
        })
    }
}
