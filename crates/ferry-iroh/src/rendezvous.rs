//! Rendezvous for zero-file pairing: mDNS service `ferry-pair-<code>` and relay topic `code`.
//! Reuses existing `MdnsSetting` if present, does not duplicate it.

use std::io;

use crate::config::MdnsSetting;

/// Topic / rendezvous key derived from a pairing code. Used as both mDNS service suffix and relay topic.
pub fn topic_for_code(code: &str) -> String {
    format!("ferry-pair-{}", code.to_ascii_lowercase())
}

/// mDNS service name for a given code. Advertised on LAN so peers without direct address hints can still find the initiator.
pub fn service_name_for_code(code: &str) -> String {
    format!("ferry-pair-{}", code.to_ascii_uppercase())
}

/// Advertise this code on the local network via mDNS. Stub for now: runtime mDNS is driven by `IrohConfig::mdns`.
/// Keeping the helper here reuses `MdnsSetting` without duplicating config types. The actual iroh endpoint is built with
/// the code-specific service name when pairing is active.
pub fn advertise(code: &str, mdns: Option<&MdnsSetting>) -> io::Result<()> {
    // In production the daemon builds an ephemeral IrohTransport with mdns.service_name = service_name_for_code(code).
    // For tests / in-memory path this is a no-op (rendezvous is the shared HashMap).
    let _ = (code, mdns);
    Ok(())
}

/// Discover a peer advertising `code` via mDNS or relay. Stub: production dials via the same topic using iroh discovery.
/// Returns None when no peer is found within the caller's timeout; caller falls back to relay.
pub fn discover(code: &str, mdns: Option<&MdnsSetting>) -> io::Result<Option<std::net::SocketAddr>> {
    let _ = (code, mdns);
    Ok(None)
}
