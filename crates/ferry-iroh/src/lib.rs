//! ferry-iroh: the T-009 transport — iroh QUIC behind the M0 `Transport` seam.
//!
//! ADR-0003: peers are addressed by device public key, never by IP;
//! connections ride a relay first and opportunistically upgrade to direct
//! QUIC; a self-hostable relay is the fallback; LAN multicast discovery
//! shortcuts shared networks. This crate is the ONLY place in the workspace
//! where an iroh type is allowed to appear: the engine keeps speaking plain
//! framed byte pipes over [`ferry_sync::Transport`], unchanged.
//!
//! ## How addressing works (and the one honest wart)
//!
//! The real dial target is always an **`EndpointId`** — an ed25519 public key
//! derived from the device's X25519 identity key ([`identity`]). But the M0
//! trait's currency is `SocketAddr`, and widening that would have touched
//! engine signatures for zero behavioral gain. So `SocketAddr` values act as
//! opaque *route keys*: a route maps route-key → `EndpointId` (+ optional
//! address hints). Routes come from two sources:
//!
//! 1. [`IrohTransport::with_route`] / the CLI (`--peer <hex id>`): explicit,
//!    cross-process. The listener prints its `EndpointId`; the connector dials
//!    by that public key.
//! 2. The process-local [`directory`], populated automatically by
//!    `listen()` with the endpoint's own id and bound addresses: two engines
//!    in one process (the whole integration suite) interop with no wiring.
//!
//! Dialing resolves the route to an `EndpointId`, then lets iroh find the path
//! (relay, hole punch, or mDNS-discovered LAN address). The wire-level peer
//! authentication is iroh's TLS-with-public-key: connecting to a key only
//! ever talks to the holder of that key.
//!
//! ## Config surface (all injectable for tests)
//!
//! - relay list ([`RelaySetting`]) — custom self-hosted relay, n0 default, or off
//! - LAN discovery ([`MdnsSetting`]) — mDNS/swarm lookup on/off + service name
//! - `force_relay` — strips all IP transports so even same-host peers must
//!   traverse the relay; this is the local stand-in for "two NATs" in tests.
//! - device identity → stable `EndpointId` derivation ([`identity`])
//!
//! Versions are pinned exactly in Cargo.toml (SPEC risk: iroh API churn);
//! every iroh type stays inside this crate.

pub mod config;
pub mod directory;
pub mod identity;
pub mod transport;

pub use config::{IrohConfig, IrohConfigBuilder, MdnsSetting, RelaySetting};
pub use directory::{publish_route, register_explicit_route, resolve_route, Route, RouteScope};
pub use transport::{DialFailure, IrohTransport, PathObservation};

/// ALPN for ferry-sync M0 sessions. Bump when the protocol changes.
pub const FERRY_ALPN: &[u8] = b"ferry-sync/m0/1";

// iroh types are deliberately absent from this facade's exports.
