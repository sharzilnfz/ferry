














use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::lock::lock;


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    
    pub endpoint_id: [u8; 32],
    
    
    pub ip_hints: Vec<SocketAddr>,
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteScope {
    Explicit,
    Directory,
}


pub type RouteKey = SocketAddr;

static GLOBAL_SYNTH_PORT: AtomicU16 = AtomicU16::new(45601);


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
    
    pub fn new() -> Self {
        RouteTable {
            inner: Arc::new(Mutex::new(HashMap::new())),
            by_peer: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    
    
    pub fn publish_route(&self, key: RouteKey, route: Route) {
        let mut bp = lock(&self.by_peer);
        bp.entry(route.endpoint_id).or_insert_with(|| route.clone());
        let mut t = lock(&self.inner);
        t.entry(key).or_insert((route, RouteScope::Directory));
    }

    
    
    pub fn register_explicit_route(&self, key: RouteKey, route: Route) {
        let mut bp = lock(&self.by_peer);
        bp.insert(route.endpoint_id, route.clone());
        let mut t = lock(&self.inner);
        t.insert(key, (route, RouteScope::Explicit));
    }

    
    pub fn register_peer(&self, endpoint_id: [u8; 32], ip_hints: Vec<SocketAddr>) -> RouteKey {
        let key = self.next_synth_key(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        self.register_explicit_route(
            key,
            Route {
                endpoint_id,
                ip_hints,
            },
        );
        key
    }

    
    pub fn resolve_route(&self, key: &RouteKey) -> Option<(Route, RouteScope)> {
        lock(&self.inner).get(key).cloned()
    }

    
    pub fn resolve_peer(&self, endpoint_id: &[u8; 32]) -> Option<Route> {
        lock(&self.by_peer).get(endpoint_id).cloned()
    }

    
    pub fn contains_key(&self, key: &RouteKey) -> bool {
        lock(&self.inner).contains_key(key)
    }

    
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


pub fn publish_route(key: RouteKey, route: Route) {
    global_table().publish_route(key, route);
}


pub fn register_explicit_route(key: RouteKey, route: Route) {
    global_table().register_explicit_route(key, route);
}


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

        
        table.publish_route(
            k,
            Route {
                endpoint_id: id(2),
                ip_hints: vec![],
            },
        );
        assert_eq!(table.resolve_route(&k).unwrap().0.endpoint_id, id(1));

        
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
