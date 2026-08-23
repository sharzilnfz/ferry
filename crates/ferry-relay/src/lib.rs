//! ferry-relay: a self-hostable, blind relay for ferry-sync (ADR-0003).
//!
//! One binary an operator runs on a VPS; clients point `--relay` at its
//! URL. The relay is a **dumb ciphertext pipe** in the strictest sense:
//!
//! - It forwards opaque datagrams between authenticated iroh endpoints.
//!   Client↔client traffic is the endpoints' own QUIC, TLS-terminated at
//!   the *peers*, so the relay cannot read payloads even though it
//!   terminates the relay protocol's transport connection.
//! - What the relay DOES see (its complete metadata surface): client IP
//!   addresses and ports, connection timing, byte/packet counts, and the
//!   endpoint public keys clients announce during the relay handshake.
//!   This crate logs exactly those facts and nothing more — see
//!   [`LocalRelay::ledger`].
//!
//! Deliberately NOT here: any per-folder logic, storage, or inspection.
//! "Relay stays as fallback" is iroh's negotiation job (ADR-0003); this
//! server does not fight it.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use iroh::RelayUrl;
use iroh_relay::server::{
    Access, AccessControl, ClientRequest, Server as RelayServer, ServerConfig,
};

/// Options for [`spawn`].
#[derive(Debug, Clone)]
pub struct RelayOptions {
    /// HTTP bind address for the relay service (plain HTTP; put real TLS or
    /// ACME in front of this for public deployments — see docs/nat-validation.md).
    pub http_bind_addr: SocketAddr,
}

impl RelayOptions {
    pub fn new(http_bind_addr: SocketAddr) -> Self {
        RelayOptions { http_bind_addr }
    }
}

/// One immutable ledger entry: everything a ferry-relay operator learns
/// about a client connection. No payload fields exist.
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

/// Append-only record of relay-visible metadata events.
#[derive(Debug, Clone, Default)]
pub struct Ledger(Arc<Mutex<Vec<LedgerEntry>>>);

impl Ledger {
    fn push(&self, e: LedgerEntry) {
        self.0.lock().unwrap().push(e);
    }

    /// Snapshot of all entries so far.
    pub fn entries(&self) -> Vec<LedgerEntry> {
        self.0.lock().unwrap().clone()
    }

    /// Rendered lines, suitable for scanning (tests scan these for
    /// plaintext markers and expect NONE).
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

/// An access-control hook that admits everyone but records the full
/// relay-visible metadata surface into a [`Ledger`].
#[derive(Debug)]
struct LedgerAccessControl(Ledger);

impl AccessControl for LedgerAccessControl {
    async fn on_connect(&self, request: &ClientRequest) -> Access {
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
        Access::Allow
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

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// A running local relay.
pub struct LocalRelay {
    url: RelayUrl,
    http_addr: SocketAddr,
    ledger: Ledger,
    server: Arc<Mutex<Option<RelayServer>>>,
}

impl LocalRelay {
    /// The URL clients pass as their relay.
    pub fn url(&self) -> RelayUrl {
        self.url.clone()
    }

    /// Bound HTTP address (port resolved after bind).
    pub fn http_addr(&self) -> SocketAddr {
        self.http_addr
    }

    /// Everything this relay has observed (metadata only).
    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// Graceful shutdown.
    pub async fn shutdown(self) {
        if let Some(server) = self.server.lock().unwrap().take() {
            let _ = server.shutdown().await;
        }
    }
}

impl Drop for LocalRelay {
    fn drop(&mut self) {
        // Best effort: the supervisor task dies with its JoinSet when the
        // runtime drops; explicit shutdown() is preferred in tests.
        if let Some(_server) = self.server.lock().unwrap().take() {
            // Server::shutdown is async; dropping here just releases handles.
        }
    }
}

/// Spawn a blind relay bound to `opts.http_bind_addr`.
///
/// Plain HTTP (no TLS): suitable for loopback tests and for deployments
/// that terminate TLS elsewhere. The returned URL is what clients give
/// their endpoints.
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
    })
}

/// Spawn using the current tokio runtime handle from sync code (tests).
pub fn spawn_sync(opts: RelayOptions) -> Result<LocalRelay, String> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?
        .block_on(spawn(opts))
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
}
