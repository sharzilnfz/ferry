//! Endpoint keys derived from device identity (ADR-0003: "peers are
//! addressed by device public key").
//!
//! Ferry's device identity is an X25519 keypair ([`ferry_crypto`]); iroh's
//! endpoint identity is an ed25519 keypair. Different curves, so the
//! endpoint key is a **deterministic derivation** of the device secret, not
//! the same key material:
//!
//! ```text
//! ed25519_seed = BLAKE3("FERRY-IROH-ED25519-V1" || x25519_secret)[0..32]
//! ```
//!
//! Properties this buys:
//!
//! - One device identity → one stable `EndpointId` across restarts. Pairing
//!   once with a peer means its public key never changes underneath you.
//! - The iroh secret never exists as user-visible state; it is recomputed.
//!   Losing the device identity file loses the `EndpointId` too — one backup
//!   story, not two.
//! - The curves are unrelated, so deriving does not weaken either key.

use ferry_crypto::identity::DeviceIdentity;

/// Domain-separation label for the derivation. Changing it changes every
/// `EndpointId`; treat it as protocol constant.
pub const DERIVE_LABEL: &[u8] = b"FERRY-IROH-ED25519-V1";

/// Deterministically derive iroh endpoint secret bytes from a device
/// identity.
///
/// `DeviceIdentity` deliberately exposes no raw-secret getter; the file
/// image is the sanctioned serialization (`magic|ver|sk32|pk32`), so the
/// derivation hashes exactly those sk bytes out of it. This keeps one
/// definition of "the device secret" in the codebase.
pub fn endpoint_seed_from_device_identity(dev: &DeviceIdentity) -> [u8; 32] {
    let image = dev.to_file_bytes();
    assert_eq!(&image[..4], b"FRID", "device identity file magic");
    let mut hasher = blake3::Hasher::new();
    hasher.update(DERIVE_LABEL);
    hasher.update(&image[5..37]); // the X25519 secret scalar
    let mut out = [0u8; 32];
    out.copy_from_slice(hasher.finalize().as_bytes());
    out
}

/// Short display form of an endpoint id (first 8 hex bytes + ellipsis),
/// for logs and Debug output. Never the full key.
pub fn id_short(id: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    for b in &id[..4] {
        let _ = write!(s, "{b:02x}");
    }
    s.push('\u{2026}');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derivation_is_deterministic_and_keyed_per_device() {
        let a = DeviceIdentity::from_secret_bytes(&[7u8; 32]);
        let b = DeviceIdentity::from_secret_bytes(&[8u8; 32]);
        let sa = endpoint_seed_from_device_identity(&a);
        let sa2 = endpoint_seed_from_device_identity(&a);
        let sb = endpoint_seed_from_device_identity(&b);
        assert_eq!(sa, sa2, "same device -> same endpoint seed");
        assert_ne!(sa, sb, "distinct devices -> distinct seeds");
        // Label matters: a different label must not produce the same seed.
        let mut h = blake3::Hasher::new();
        h.update(b"SOME-OTHER-LABEL");
        h.update(&a.to_file_bytes()[5..37]);
        let other: [u8; 32] = *h.finalize().as_bytes();
        assert_ne!(sa, other);
    }

    #[test]
    fn derived_seed_is_stable_across_identity_reload() {
        // Reload through the sanctioned load path: the same secret bytes
        // must derive the same endpoint seed.
        let dev = DeviceIdentity::from_secret_bytes(&[0x42u8; 32]);
        let mut sk = [0u8; 32];
        sk.copy_from_slice(&dev.to_file_bytes()[5..37]);
        let reloaded = DeviceIdentity::from_secret_bytes(&sk);
        assert_eq!(reloaded.device_id(), dev.device_id());
        assert_eq!(
            endpoint_seed_from_device_identity(&dev),
            endpoint_seed_from_device_identity(&reloaded)
        );
    }
}
