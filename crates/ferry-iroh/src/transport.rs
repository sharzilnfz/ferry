








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






#[derive(Debug, Default)]
pub struct PathObservation {
    
    pub selected_relay_seen: AtomicBool,
    
    pub selected_ip_seen: AtomicBool,
}

impl PathObservation {
    
    pub fn relay_only_so_far(&self) -> bool {
        self.selected_relay_seen.load(Ordering::SeqCst)
            && !self.selected_ip_seen.load(Ordering::SeqCst)
    }
}



#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialFailure {
    
    NoRoute(SocketAddr),
    
    SelfDial,
    
    Timeout,
    
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
    rt: Mutex<Option<Runtime>>,
    rt_handle: Handle,
    ep: Endpoint,
    my_id: EndpointId,
    dial_timeout: Duration,
    closed: AtomicBool,
    observations: Mutex<HashMap<[u8; 32], Arc<PathObservation>>>,
    
    
    
    relay_urls: Vec<iroh::RelayUrl>,
    routes: RouteTable,
}

fn block_on<F, T>(handle: &Handle, f: F) -> T
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(curr) if curr.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| handle.block_on(f))
        }
        Ok(_) => {
            let h = handle.clone();
            std::thread::spawn(move || h.block_on(f))
                .join()
                .unwrap_or_else(|p| std::panic::resume_unwind(p))
        }
        Err(_) => handle.block_on(f),
    }
}

impl Inner {
    fn observe(&self, id: EndpointId) -> Arc<PathObservation> {
        let mut map = lock(&self.observations);
        map.entry(*id.as_bytes())
            .or_insert_with(|| Arc::new(PathObservation::default()))
            .clone()
    }

    fn rt_handle(&self) -> &Handle {
        &self.rt_handle
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        if let Ok(mut g) = self.rt.lock() {
            if let Some(rt) = g.take() {
                let ep = self.ep.clone();
                let handle = rt.handle().clone();
                let close_and_shutdown = move || {
                    handle.block_on(async move {
                        let _ = tokio::time::timeout(Duration::from_millis(200), ep.close()).await;
                    });
                    rt.shutdown_timeout(Duration::from_millis(500));
                };

                if tokio::runtime::Handle::try_current().is_ok() {
                    let _ = std::thread::spawn(close_and_shutdown).join();
                } else {
                    close_and_shutdown();
                }
            }
        }
    }
}



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
    
    pub fn new(cfg: IrohConfig) -> io::Result<Self> {
        let seed = cfg.resolve_secret().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "iroh transport needs a secret or a device identity",
            )
        })?;

        let dial_timeout = cfg.dial_timeout;
        let relay_urls = match &cfg.relays {
            RelaySetting::Custom(urls) => urls.iter().filter_map(|u| u.parse().ok()).collect(),
            _ => Vec::new(),
        };
        let routes = cfg.routes.clone().unwrap_or_default();

        let (rt, ep) = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|e| io::Error::other(format!("tokio runtime: {e}")))?;

            let ep = rt.block_on(build_endpoint(&cfg, seed))?;
            Ok::<_, io::Error>((rt, ep))
        })
        .join()
        .map_err(|_| io::Error::other("thread panicked building iroh transport"))??;
        let my_id = ep.id();
        let rt_handle = rt.handle().clone();

        Ok(IrohTransport {
            inner: Arc::new(Inner {
                rt: Mutex::new(Some(rt)),
                rt_handle,
                ep,
                my_id,
                dial_timeout,
                closed: AtomicBool::new(false),
                observations: Mutex::new(HashMap::new()),
                relay_urls,
                routes,
            }),
        })
    }

    
    pub fn routes(&self) -> &RouteTable {
        &self.inner.routes
    }

    
    pub fn route_table(&self) -> &RouteTable {
        &self.inner.routes
    }

    
    
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

    
    pub fn endpoint_id(&self) -> [u8; 32] {
        *self.inner.my_id.as_bytes()
    }

    
    pub fn dial_peer(&self, endpoint_id: &[u8; 32]) -> Result<Box<dyn DynConnection>, DialFailure> {
        let hints = self
            .inner
            .routes
            .resolve_peer(endpoint_id)
            .map(|r| r.ip_hints)
            .unwrap_or_default();
        self.dial_endpoint(*endpoint_id, hints)
    }

    
    
    
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
        
        
        
        
        for url in &self.inner.relay_urls {
            addr.addrs.insert(TransportAddr::Relay(url.clone()));
        }
        
        
        let ep = self.inner.ep.clone();
        let budget = self.inner.dial_timeout;
        let opened = block_on(self.inner.rt_handle(), async move {
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
        spawn_path_sampler(self.inner.rt_handle(), &conn, obs);
        Ok(Box::new(FramedConnection {
            inner: Arc::clone(&self.inner),
            conn,
            send: Arc::new(tokio::sync::Mutex::new(send)),
            recv: Arc::new(tokio::sync::Mutex::new(recv)),
        }))
    }

    
    
    pub fn path_observation(&self, endpoint_id: &[u8; 32]) -> Option<Arc<PathObservation>> {
        self.inner
            .observations
            .lock()
            .unwrap()
            .get(endpoint_id)
            .cloned()
    }

    
    pub fn shutdown(&self) {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let ep = self.inner.ep.clone();
        block_on(self.inner.rt_handle(), async move {
            let _ = tokio::time::timeout(Duration::from_millis(500), ep.close()).await;
        });
    }
}


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
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let ep = self.inner.ep.clone();
        block_on(self.inner.rt_handle(), async move {
            let _ = tokio::time::timeout(Duration::from_millis(200), ep.close()).await;
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
            let ep = self.inner.ep.clone();
            let next = block_on(self.inner.rt_handle(), async move {
                tokio::time::timeout(Duration::from_millis(200), ep.accept()).await
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
            let conn = block_on(self.inner.rt_handle(), async move {
                match incoming.accept() {
                    Ok(accepting) => accepting
                        .await
                        .map_err(|e| io::Error::other(format!("handshake: {e:#}"))),
                    Err(e) => Err(io::Error::other(format!("incoming accept: {e:#}"))),
                }
            })?;
            let obs = self.inner.observe(conn.remote_id());
            spawn_path_sampler(self.inner.rt_handle(), &conn, obs);

            let conn_for_streams = conn.clone();
            let streams = block_on(self.inner.rt_handle(), async move {
                conn_for_streams.accept_bi().await
            })
            .map_err(|e| io::Error::other(format!("accept_bi: {e:#}")))?;
            return Ok(Box::new(FramedConnection {
                inner: Arc::clone(&self.inner),
                conn,
                send: Arc::new(tokio::sync::Mutex::new(streams.0)),
                recv: Arc::new(tokio::sync::Mutex::new(streams.1)),
            }));
        }
    }
}





struct FramedConnection {
    inner: Arc<Inner>,
    #[allow(dead_code)]
    conn: IrohConn,
    send: Arc<tokio::sync::Mutex<iroh::endpoint::SendStream>>,
    recv: Arc<tokio::sync::Mutex<iroh::endpoint::RecvStream>>,
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
        let len = (payload.len() as u32).to_le_bytes();
        let payload = payload.to_vec();
        let send = Arc::clone(&self.send);
        block_on(self.inner.rt_handle(), async move {
            let mut send = send.lock().await;
            send.write_all(&len).await?;
            if !payload.is_empty() {
                send.write_all(&payload).await?;
            }
            Ok::<(), io::Error>(())
        })
    }

    fn recv_frame(&mut self) -> io::Result<Vec<u8>> {
        let recv = Arc::clone(&self.recv);
        block_on(self.inner.rt_handle(), async move {
            let mut recv = recv.lock().await;
            let mut len_buf = [0u8; 4];
            match recv.read_exact(&mut len_buf).await {
                Ok(()) => {}
                
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

#[cfg(test)]
mod tests {
    use super::*;
    use rand::RngCore as _;

    fn test_transport(seed_byte: u8) -> IrohTransport {
        let mut seed = [0u8; 32];
        seed[0] = seed_byte;
        seed[1] = seed_byte;
        rand::thread_rng().fill_bytes(&mut seed[2..]);
        IrohTransport::new(IrohConfig::builder().secret(seed).build()).expect("transport builds")
    }

    #[test]
    fn synchronous_calls_work_inside_multithread_tokio_runtime() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let a = test_transport(0x31);
            let b = test_transport(0x32);

            let lst = a.listen("127.0.0.1:0".parse().unwrap()).unwrap();
            let addr = lst.local_addr().unwrap();

            let server = std::thread::spawn(move || {
                let mut c = lst.accept().unwrap();
                let msg = c.recv_frame().unwrap();
                assert_eq!(msg, b"ping");
                c.send_frame(b"pong").unwrap();
            });

            let mut cli = b.dial(addr).unwrap();
            cli.send_frame(b"ping").unwrap();
            let reply = cli.recv_frame().unwrap();
            assert_eq!(reply, b"pong");

            server.join().unwrap();
        });
    }

    #[test]
    fn synchronous_calls_work_inside_current_thread_tokio_runtime() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let a = test_transport(0x41);
            let b = test_transport(0x42);

            let lst = a.listen("127.0.0.1:0".parse().unwrap()).unwrap();
            let addr = lst.local_addr().unwrap();

            let server = std::thread::spawn(move || {
                let mut c = lst.accept().unwrap();
                let msg = c.recv_frame().unwrap();
                assert_eq!(msg, b"current-thread-ping");
                c.send_frame(b"current-thread-pong").unwrap();
            });

            let mut cli = b.dial(addr).unwrap();
            cli.send_frame(b"current-thread-ping").unwrap();
            let reply = cli.recv_frame().unwrap();
            assert_eq!(reply, b"current-thread-pong");

            server.join().unwrap();
        });
    }

    #[test]
    fn connection_and_listener_survive_transport_drop() {
        let a = test_transport(0x51);
        let b = test_transport(0x52);

        let lst = a.listen("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = lst.local_addr().unwrap();
        drop(a); 

        let server = std::thread::spawn(move || {
            let mut c = lst.accept().unwrap();
            let msg = c.recv_frame().unwrap();
            assert_eq!(msg, b"survived-transport-drop");
            c.send_frame(b"ack").unwrap();
            let _ = c.recv_frame();
        });

        let mut cli = b.dial(addr).unwrap();
        drop(b); 

        cli.send_frame(b"survived-transport-drop").unwrap();
        let reply = cli.recv_frame().unwrap();
        assert_eq!(reply, b"ack");
        drop(cli);

        server.join().unwrap();
    }

    #[test]
    fn transport_shutdown_is_idempotent() {
        let a = test_transport(0x61);
        a.shutdown();
        a.shutdown();
    }
}
