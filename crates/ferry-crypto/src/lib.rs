




























































pub mod base32;
pub mod config_head;
pub mod crc32;
pub mod folder_key;
pub mod identity;
pub mod pack_cipher;
pub mod pairing;
pub mod recovery;



pub(crate) fn hex_short(data: &[u8]) -> String {
    ferry_store::format::hex(&data[..8.min(data.len())])
}


#[cfg(test)]
pub(crate) mod testing {
    use rand::{CryptoRng, RngCore};

    
    
    pub(crate) struct FixedRng {
        pattern: [u8; 32],
        pos: usize,
    }

    impl FixedRng {
        pub(crate) fn new(hex_pattern: &str) -> Self {
            FixedRng {
                pattern: ferry_store::format::unhex(hex_pattern).expect("valid hex"),
                pos: 0,
            }
        }
    }

    impl RngCore for FixedRng {
        fn next_u32(&mut self) -> u32 {
            let mut b = [0u8; 4];
            self.fill_bytes(&mut b);
            u32::from_le_bytes(b)
        }
        fn next_u64(&mut self) -> u64 {
            (u64::from(self.next_u32()) << 32) | u64::from(self.next_u32())
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            for b in dest {
                *b = self.pattern[self.pos % 32];
                self.pos += 1;
            }
        }
        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }
    impl CryptoRng for FixedRng {}
}
