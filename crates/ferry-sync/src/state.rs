//! M0 device identity derivation.
//!
//! The canonical last-agreed record codec and its ledger moved to
//! [`ferry_store::agreement`] in T-10 (ONE codec, ONE store for the whole
//! workspace); this module keeps only the deterministic tag → device-id
//! mapping the M0 engine needs.

use ferry_store::format::BlobId;

/// M0 device identity: a stable 32-byte id derived from the tag string.
/// Real X25519 device keys arrive with T-007; nothing in M0 depends on the
/// derivation being cryptographic — only on being deterministic and
/// collision-free for our test tags.
pub fn device_id_from_tag(tag: &str) -> BlobId {
    *blake3::hash(format!("ferry/m0/device:{tag}").as_bytes()).as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_ids_are_deterministic_and_tag_separated() {
        assert_eq!(device_id_from_tag("a"), device_id_from_tag("a"));
        assert_ne!(device_id_from_tag("a"), device_id_from_tag("b"));
    }
}
