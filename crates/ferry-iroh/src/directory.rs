//! The route table: opaque `SocketAddr` route keys → `EndpointId` (+ hints),
//! and explicit peer identity routing.
//!
//! Why this exists: ADR-0003 addresses peers by public key. Rather than
//! relying on a process-global static table, [`RouteTable`] encapsulates
//! routes explicitly per transport instance while providing process-wide
//! discovery fallbacks for in-process engine pairs.
//!
//! Two sources feed resolution:
//! - **explicit routes** registered via [`IrohTransport::with_route`](crate::IrohTransport::with_route)
//!   or [`RouteTable::register_explicit_route`] (`RouteScope::Explicit`); the daemon CLI builds these from `--peer`.
//! - **the local directory** ([`RouteScope::Directory`]):
//!   `listen()` registers the bound alias with the listener's own `EndpointId`
//!   and real socket addresses, so in-process engine pairs interop with zero wiring.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::lock::lock;

/// An explicit or directory-discovered route to a peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// The peer's ed25519 endpoint id — the actual dial target.
    pub endpoint_id: [u8; 32],
    /// Optional direct address hints. iroh tries these plus anything its
    /// discovery/relay layers know.
    pub ip_hints: Vec<SocketAddr>,
}

/// Where a resolved route came from. Explicit wins over directory entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteScope {
    Explicit,
    Directory,
}

/// A route key: any `SocketAddr` the caller chose as a handle.
pub type RouteKey = SocketAddr;

static GLOBAL_SYNTH_PORT: AtomicU16 = AtomicU16::new(45601);

/// Generate a unique synthetic route key on `ip`.
pub fn next_global_synth_key(ip: std::net::IpAddr) -> RouteKey {
    loop {
        let cur = GLOBAL_SYNTH_PORT.fetch_add(1, Ordering::SeqCst);
        let port = 45601 + (cur % 15000);
        let key = SocketAddr::new(ip, port);
        if global_table().resolve_route(&key).is_none() {
            return key;
        }
    }
}

/// An explicit, instance-isolated route table mapping handles and peer identities
/// to iroh endpoint destinations.
#[derive(Debug, Clone)]
pub struct RouteTable {
    inner: Arc<Mutex<HashMap<RouteKey, (Route, RouteScope)>>>,
    by_peer: Arc<Mutex<HashMap<[u8; 32], Route>>>,
}

impl Default for RouteTable {
    fn default() -> Self {
        Self::new()
    }
}

impl RouteTable {
    /// Construct a new, empty route table.
    pub fn new() -> Self {
        RouteTable {
            inner: Arc::new(Mutex::new(HashMap::new())),
            by_peer: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Publish a route (directory scope). Directory routes are only added
    /// if no entry exists: first publisher wins.
    pub fn publish_route(&self, key: RouteKey, route: Route) {
        let mut bp = lock(&self.by_peer);
        bp.entry(route.endpoint_id).or_insert_with(|| route.clone());
        let mut t = lock(&self.inner);
        t.entry(key).or_insert((route, RouteScope::Directory));
    }

    /// Register an operator/config-provided route. Overrides any previous
    /// registration for the same key.
    pub fn register_explicit_route(&self, key: RouteKey, route: Route) {
        let mut bp = lock(&self.by_peer);
        bp.insert(route.endpoint_id, route.clone());
        let mut t = lock(&self.inner);
        t.insert(key, (route, RouteScope::Explicit));
    }

    /// Register a peer identity explicitly and return a synthesized route key.
    pub fn register_peer(&self, endpoint_id: [u8; 32], ip_hints: Vec<SocketAddr>) -> RouteKey {
        let key = self.next_synth_key(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        self.register_explicit_route(key, Route { endpoint_id, ip_hints });
        key
    }

    /// Resolve a route key. Returns None when nothing was ever published.
    pub fn resolve_route(&self, key: &RouteKey) -> Option<(Route, RouteScope)> {
        lock(&self.inner).get(key).cloned()
    }

    /// Resolve a peer identity directly without requiring a route key.
    pub fn resolve_peer(&self, endpoint_id: &[u8; 32]) -> Option<Route> {
        lock(&self.by_peer).get(endpoint_id).cloned()
    }

    /// Check whether a key is present in the table.
    pub fn contains_key(&self, key: &RouteKey) -> bool {
        lock(&self.inner).contains_key(key)
    }

    /// Generate a fresh, unused synthetic route key on `ip`.
    pub fn next_synth_key(&self, ip: std::net::IpAddr) -> RouteKey {
        loop {
            let key = next_global_synth_key(ip);
            if !self.contains_key(&key) {
                return key;
            }
        }
    }
}

static GLOBAL_DIRECTORY: OnceLock<RouteTable> = OnceLock::new();

fn global_table() -> &'static RouteTable {
    GLOBAL_DIRECTORY.get_or_init(RouteTable::new)
}

/// Publish a route into the process-global fallback table.
pub fn publish_route(key: RouteKey, route: Route) {
    global_table().publish_route(key, route);
}

/// Register an explicit route into the process-global fallback table.
pub fn register_explicit_route(key: RouteKey, route: Route) {
    global_table().register_explicit_route(key, route);
}

/// Resolve a route key from the process-global fallback table.
pub fn resolve_route(key: &RouteKey) -> Option<(Route, RouteScope)> {
    global_table().resolve_route(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    #[test]
    fn explicit_overrides_and_directory_first_wins() {
        let table = RouteTable::new();
        let k: RouteKey = "127.0.0.1:49001".parse().unwrap();
        assert!(table.resolve_route(&k).is_none());
        table.publish_route(
            k,
            Route {
                endpoint_id: id(1),
                ip_hints: vec![],
            },
        );
        let got = table.resolve_route(&k).unwrap();
        assert_eq!(got.0.endpoint_id, id(1));
        assert_eq!(got.1, RouteScope::Directory);

        // Directory republish loses to the first claimant.
        table.publish_route(
            k,
            Route {
                endpoint_id: id(2),
                ip_hints: vec![],
            },
        );
        assert_eq!(table.resolve_route(&k).unwrap().0.endpoint_id, id(1));

        // Explicit registration overrides anything.
        table.register_explicit_route(
            k,
            Route {
                endpoint_id: id(3),
                ip_hints: vec![],
            },
        );
        let got = table.resolve_route(&k).unwrap();
        assert_eq!(got.0.endpoint_id, id(3));
        assert_eq!(got.1, RouteScope::Explicit);
    }

    #[test]
    fn register_peer_generates_unique_resolvable_key() {
        let table = RouteTable::new();
        let peer_a = id(10);
        let peer_b = id(20);
        let key_a = table.register_peer(peer_a, vec![]);
        let key_b = table.register_peer(peer_b, vec![]);
        assert_ne!(key_a, key_b);
        assert_eq!(table.resolve_route(&key_a).unwrap().0.endpoint_id, peer_a);
        assert_eq!(table.resolve_route(&key_b).unwrap().0.endpoint_id, peer_b);
        assert_eq!(table.resolve_peer(&peer_a).unwrap().endpoint_id, peer_a);
    }
}
