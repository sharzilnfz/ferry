




















use ferry_crypto::identity::DeviceIdentity;



pub const DERIVE_LABEL: &[u8] = b"FERRY-IROH-ED25519-V1";








pub fn endpoint_seed_from_device_identity(dev: &DeviceIdentity) -> [u8; 32] {
    let image = dev.to_file_bytes();
    assert_eq!(&image[..4], b"FRID", "device identity file magic");
    let mut hasher = blake3::Hasher::new();
    hasher.update(DERIVE_LABEL);
    hasher.update(&image[5..37]); 
    let mut out = [0u8; 32];
    out.copy_from_slice(hasher.finalize().as_bytes());
    out
}



pub fn id_short(id: &[u8; 32]) -> String {
    format!("{}\u{2026}", ferry_store::format::hex(&id[..4]))
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
        
        let mut h = blake3::Hasher::new();
        h.update(b"SOME-OTHER-LABEL");
        h.update(&a.to_file_bytes()[5..37]);
        let other: [u8; 32] = *h.finalize().as_bytes();
        assert_ne!(sa, other);
    }

    #[test]
    fn derived_seed_is_stable_across_identity_reload() {
        
        
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
