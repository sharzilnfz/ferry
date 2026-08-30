


















use std::future::Future;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use ferry_store::format::hex;




fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

use iroh::RelayUrl;
use iroh_relay::server::{
    Access, AccessControl, ClientRequest, Server as RelayServer, ServerConfig,
};


#[derive(Debug, Clone)]
pub struct RelayOptions {
    
    
    pub http_bind_addr: SocketAddr,
}

impl RelayOptions {
    pub fn new(http_bind_addr: SocketAddr) -> Self {
        RelayOptions { http_bind_addr }
    }
}



#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerEntry {
    Connected {
        endpoint_id_hex: String,
        remote: Option<SocketAddr>,
    },
    Disconnected {
        endpoint_id_hex: String,
    },
}


#[derive(Debug, Clone, Default)]
pub struct Ledger(Arc<Mutex<Vec<LedgerEntry>>>);

impl Ledger {
    fn push(&self, e: LedgerEntry) {
        lock(&self.0).push(e);
    }

    
    pub fn entries(&self) -> Vec<LedgerEntry> {
        lock(&self.0).clone()
    }

    
    
    pub fn render(&self) -> Vec<String> {
        self.entries()
            .into_iter()
            .map(|e| match e {
                LedgerEntry::Connected {
                    endpoint_id_hex,
                    remote,
                } => {
                    format!("CONNECT id={endpoint_id_hex} remote={remote:?}")
                }
                LedgerEntry::Disconnected { endpoint_id_hex } => {
                    format!("DISCONNECT id={endpoint_id_hex}")
                }
            })
            .collect()
    }
}



#[derive(Debug)]
struct LedgerAccessControl(Ledger);

impl AccessControl for LedgerAccessControl {
    fn on_connect(&self, request: &ClientRequest) -> impl Future<Output = Access> {
        let id = hex(request.endpoint_id().as_bytes());
        let remote = request
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok());
        tracing::info!(target: "ferry_relay", %id, ?remote, "relay client connected");
        self.0.push(LedgerEntry::Connected {
            endpoint_id_hex: id,
            remote,
        });
        std::future::ready(Access::Allow)
    }

    fn on_disconnect(
        &self,
        endpoint_id: iroh::EndpointId,
        _conn: iroh_relay::server::ConnectionId,
    ) {
        let id = hex(endpoint_id.as_bytes());
        tracing::info!(target: "ferry_relay", %id, "relay client disconnected");
        self.0.push(LedgerEntry::Disconnected {
            endpoint_id_hex: id,
        });
    }
}


pub struct LocalRelay {
    url: RelayUrl,
    http_addr: SocketAddr,
    ledger: Ledger,
    server: Arc<Mutex<Option<RelayServer>>>,
    
    
    _rt: Mutex<Option<tokio::runtime::Runtime>>,
}

impl LocalRelay {
    
    pub fn url(&self) -> RelayUrl {
        self.url.clone()
    }

    
    pub fn http_addr(&self) -> SocketAddr {
        self.http_addr
    }

    
    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    
    
    pub async fn shutdown(self) {
        let taken = lock(&self.server).take();
        if let Some(server) = taken {
            let _ = server.shutdown().await;
        }
    }
}

impl Drop for LocalRelay {
    fn drop(&mut self) {
        
        
        if let Some(_server) = lock(&self.server).take() {
            
        }
    }
}






pub async fn spawn(opts: RelayOptions) -> Result<LocalRelay, String> {
    let ledger = Ledger::default();
    let mut config = ServerConfig::default();
    let mut relay_cfg = iroh_relay::server::RelayConfig::new(opts.http_bind_addr);
    relay_cfg.access = Arc::new(LedgerAccessControl(ledger.clone()));
    config.relay = Some(relay_cfg);

    let server = RelayServer::spawn(config)
        .await
        .map_err(|e| format!("relay spawn: {e:#}"))?;
    let http_addr = server
        .http_addr()
        .ok_or("relay spawned without http addr")?;
    let url: RelayUrl = format!("http://{http_addr}")
        .parse()
        .map_err(|e| format!("relay url: {e}"))?;

    Ok(LocalRelay {
        url,
        http_addr,
        ledger,
        server: Arc::new(Mutex::new(Some(server))),
        _rt: Mutex::new(None),
    })
}



pub fn spawn_sync(opts: RelayOptions) -> Result<LocalRelay, String> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    let relay = rt.block_on(spawn(opts))?;
    *lock(&relay._rt) = Some(rt);
    Ok(relay)
}







#[doc(hidden)]
pub fn install_capturing_subscriber(
    buffer: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
) -> Result<bool, String> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Layer as _;
    struct BufWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for BufWriter {
        type Writer = BufWriterClone;
        fn make_writer(&'a self) -> Self::Writer {
            BufWriterClone(self.0.clone())
        }
    }
    struct BufWriterClone(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for BufWriterClone {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            lock(&self.0).extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let subscriber = tracing_subscriber::registry().with(
        
        
        tracing_subscriber::fmt::layer()
            .with_writer(BufWriter(buffer))
            .with_ansi(false)
            .with_filter(tracing_subscriber::filter::LevelFilter::TRACE),
    );
    
    
    
    
    Ok(tracing::subscriber::set_global_default(subscriber).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn relay_spawns_and_reports_url_on_free_port() {
        let relay = spawn(RelayOptions::new("127.0.0.1:0".parse().unwrap()))
            .await
            .expect("relay spawns");
        assert!(relay.url().as_str().starts_with("http://127.0.0.1:"));
        assert_eq!(relay.ledger().entries().len(), 0, "no clients yet");
        relay.shutdown().await;
    }

    
    
    #[test]
    fn poisoned_ledger_mutex_still_usable() {
        let shared = Arc::new(Mutex::new(Vec::<LedgerEntry>::new()));
        let ledger = Ledger(shared.clone());

        
        
        let handle = {
            let shared = shared.clone();
            std::thread::spawn(move || {
                let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _guard = shared.lock().unwrap();
                    panic!("deliberate poison while holding the ledger lock");
                }));
                assert!(unwound.is_err());
            })
        };
        handle.join().expect("poisoner thread completes");

        
        ledger.push(LedgerEntry::Disconnected {
            endpoint_id_hex: "aa".into(),
        });
        assert_eq!(
            ledger.entries(),
            vec![LedgerEntry::Disconnected {
                endpoint_id_hex: "aa".into(),
            }]
        );
    }
}
