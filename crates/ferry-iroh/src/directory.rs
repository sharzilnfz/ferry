//! The route table: opaque `SocketAddr` route keys → `EndpointId` (+ hints).
//!
//! Why this exists: the M0 [`ferry_sync::Transport`] trait addresses peers
//! by `SocketAddr`. ADR-0003 addresses peers by public key. Rather than
//! widen the trait (engine signatures would ripple for no behavioral gain),
//! a route key is an *opaque handle* the transport resolves to an
//! `EndpointId` before any packet is sent. Two sources feed resolution:
//!
//! - **explicit routes** registered via [`IrohTransport::with_route`]
//!   (`RouteScope::Explicit`); the daemon CLI builds these from `--peer`.
//! - **the process-local directory** ([`RouteScope::Directory`]):
//!   `listen()` registers the bound alias with the listener's own `EndpointId`
//!   and real socket addresses, so in-process engine pairs (every existing
//!   integration test) interop with zero wiring, like loopback TCP did.
//!
//! Route keys are never put on the wire; they mean nothing outside the
//! process that minted them.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock};

use crate::lock::lock;

/// An explicit or directory-discovered route to a peer.
#[derive(Debug, Clone)]
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

static DIRECTORY: OnceLock<Mutex<HashMap<RouteKey, (Route, RouteScope)>>> = OnceLock::new();

fn table() -> &'static Mutex<HashMap<RouteKey, (Route, RouteScope)>> {
    DIRECTORY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Publish a route so every transport in this process can resolve it.
///
/// Directory routes are only added if no entry (of either scope) exists:
/// first publisher wins, keeping repeated test setups stable.
pub fn publish_route(key: RouteKey, route: Route) {
    let mut t = lock(table());
    t.entry(key).or_insert((route, RouteScope::Directory));
}

/// Register an operator/config-provided route. Overrides any previous
/// registration for the same key.
pub fn register_explicit_route(key: RouteKey, route: Route) {
    let mut t = lock(table());
    t.insert(key, (route, RouteScope::Explicit));
}

/// Resolve a route key. Returns None when nothing was ever published.
pub fn resolve_route(key: &RouteKey) -> Option<(Route, RouteScope)> {
    lock(table()).get(key).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    #[test]
    fn explicit_overrides_and_directory_first_wins() {
        let k: RouteKey = "127.0.0.1:49001".parse().unwrap();
        assert!(resolve_route(&k).is_none());
        publish_route(
            k,
            Route {
                endpoint_id: id(1),
                ip_hints: vec![],
            },
        );
        let got = resolve_route(&k).unwrap();
        assert_eq!(got.0.endpoint_id, id(1));
        assert_eq!(got.1, RouteScope::Directory);

        // Directory republish loses to the first claimant.
        publish_route(
            k,
            Route {
                endpoint_id: id(2),
                ip_hints: vec![],
            },
        );
        assert_eq!(resolve_route(&k).unwrap().0.endpoint_id, id(1));

        // Explicit registration overrides anything.
        register_explicit_route(
            k,
            Route {
                endpoint_id: id(3),
                ip_hints: vec![],
            },
        );
        let got = resolve_route(&k).unwrap();
        assert_eq!(got.0.endpoint_id, id(3));
        assert_eq!(got.1, RouteScope::Explicit);
    }
}
