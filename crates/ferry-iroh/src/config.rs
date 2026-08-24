//! Injectable endpoint configuration: relays, discovery, identity, forcing.

use std::time::Duration;

use ferry_crypto::identity::DeviceIdentity;

/// Which relay servers endpoints may use (ADR-0003: relay-first, fallback).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RelaySetting {
    /// No relays at all. Two peers can then only connect when discovery or
    /// explicit route hints give them direct addresses.
    #[default]
    Disabled,
    /// n0's public relays. Convenient for manual use; tests never rely on
    /// third-party infrastructure.
    N0,
    /// Operator-run relays only — `ferry-relay` URLs (ADR-0003: self-hostable
    /// from v0, never required to be anyone's paid service).
    Custom(Vec<String>),
}

/// LAN multicast discovery (mDNS/swarm) settings.
///
/// Uses `iroh-mdns-address-lookup` 0.5 (n0-maintained, wraps
/// `swarm-discovery` 0.6). The old `iroh-mdns` crate no longer exists; this
/// is what current iroh ships for local discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdnsSetting {
    /// mDNS service name; instances sharing a name discover each other.
    /// Distinct names isolate concurrent test runs from each other and from
    /// real ferries on the same network.
    pub service_name: String,
    /// Advertise our own addresses (default true).
    pub advertise: bool,
}

/// Everything needed to build an [`IrohTransport`](crate::IrohTransport).
///
/// Defaults are deliberately inert for tests: no relays, no discovery, 10s
/// dial timeout. Production wiring (the daemon bin) turns relays on and
/// passes the operator's relay list.
#[derive(Debug, Clone)]
pub struct IrohConfig {
    /// Raw ed25519 seed bytes for the endpoint key. When `None`, the seed is
    /// derived from [`device_identity`] ([`crate::identity`]); at least one
    /// of the two must be set.
    pub secret: Option<[u8; 32]>,
    /// Device identity whose X25519 secret deterministically derives the
    /// `EndpointId`. Ignored when [`IrohConfig::secret`] is set directly.
    pub device_identity: Option<DeviceIdentity>,
    pub relays: RelaySetting,
    pub mdns: Option<MdnsSetting>,
    /// Remove all IP transports so connections can ONLY traverse relays.
    ///
    /// This is iroh 1.x's `Builder::clear_ip_transports`. It is how the
    /// ticket's "relay-forced mode (direct disabled by config)" is realized:
    /// even two same-host peers must exchange every byte through the relay,
    /// which makes the plaintext-absence proof meaningful locally.
    pub force_relay: bool,
    /// How long a dial may take before failing typed (`TimedOut`). iroh has
    /// its own ~10s QUIC connect budget underneath this.
    pub dial_timeout: Duration,
}

impl Default for IrohConfig {
    fn default() -> Self {
        IrohConfig {
            secret: None,
            device_identity: None,
            relays: RelaySetting::Disabled,
            mdns: None,
            force_relay: false,
            dial_timeout: Duration::from_secs(10),
        }
    }
}

impl IrohConfig {
    pub fn builder() -> IrohConfigBuilder {
        IrohConfigBuilder(IrohConfig::default())
    }

    /// The ed25519 seed this config resolves to, if any.
    pub fn resolve_secret(&self) -> Option<[u8; 32]> {
        if let Some(bytes) = self.secret {
            return Some(bytes);
        }
        self.device_identity
            .as_ref()
            .map(crate::identity::endpoint_seed_from_device_identity)
    }
}

/// Fluent wrapper so call sites read like the config they produce.
#[derive(Debug)]
pub struct IrohConfigBuilder(IrohConfig);

impl IrohConfigBuilder {
    /// Derive the stable `EndpointId` from this device identity.
    pub fn device_identity(mut self, id: &DeviceIdentity) -> Self {
        self.0.device_identity = Some(id.clone());
        self
    }

    /// Set the ed25519 seed explicitly (tests with fixed keys).
    pub fn secret(mut self, seed: [u8; 32]) -> Self {
        self.0.secret = Some(seed);
        self
    }

    pub fn relays(mut self, relays: RelaySetting) -> Self {
        self.0.relays = relays;
        self
    }

    pub fn mdns(mut self, mdns: MdnsSetting) -> Self {
        self.0.mdns = Some(mdns);
        self
    }

    pub fn force_relay(mut self, yes: bool) -> Self {
        self.0.force_relay = yes;
        self
    }

    pub fn dial_timeout(mut self, d: Duration) -> Self {
        self.0.dial_timeout = d;
        self
    }

    pub fn build(self) -> IrohConfig {
        self.0
    }
}
