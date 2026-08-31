
































use std::path::PathBuf;

use thiserror::Error;

use crate::format::{hex, unhex};


pub const AGREED_RECORD_LEN: usize = 77;


#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgreedRecord {
    pub peer_device_id: [u8; 32],
    pub manifest_id: [u8; 32],
    pub agreed_sec: i64,
    pub agreed_nsec: u32,
}

#[derive(Debug, Error)]
pub enum AgreementError {
    #[error("io error touching agreement ledger: {0}")]
    Io(#[from] std::io::Error),
    #[error("agreement record is {len} bytes, expected {AGREED_RECORD_LEN}")]
    BadLength { len: usize },
    #[error("agreement record flags byte is nonzero; refusing v0-incompatible state")]
    BadFlags,
}


pub fn encode_agreed_record(r: &AgreedRecord) -> [u8; AGREED_RECORD_LEN] {
    let mut out = [0u8; AGREED_RECORD_LEN];
    out[..32].copy_from_slice(&r.peer_device_id);
    out[32..64].copy_from_slice(&r.manifest_id);
    out[64..72].copy_from_slice(&r.agreed_sec.to_le_bytes());
    out[72..76].copy_from_slice(&r.agreed_nsec.to_le_bytes());
    out[76] = 0; 
    out
}



pub fn parse_agreed_record(bytes: &[u8]) -> Result<AgreedRecord, AgreementError> {
    if bytes.len() != AGREED_RECORD_LEN {
        return Err(AgreementError::BadLength { len: bytes.len() });
    }
    if bytes[76] != 0 {
        return Err(AgreementError::BadFlags);
    }
    Ok(AgreedRecord {
        peer_device_id: bytes[..32].try_into().expect("32 bytes"),
        manifest_id: bytes[32..64].try_into().expect("32 bytes"),
        agreed_sec: i64::from_le_bytes(bytes[64..72].try_into().expect("8 bytes")),
        agreed_nsec: u32::from_le_bytes(bytes[72..76].try_into().expect("4 bytes")),
    })
}



#[derive(Clone, Debug)]
pub struct AgreementLedger {
    dir: PathBuf,
}

impl AgreementLedger {
    
    
    pub fn new(store_dir: impl Into<PathBuf>) -> Self {
        AgreementLedger {
            dir: store_dir.into().join("agreement"),
        }
    }

    
    pub fn path_for(&self, folder_id: &[u8; 16], peer: &[u8; 32]) -> PathBuf {
        self.dir
            .join(format!("{}-{}.agree", hex(folder_id), hex(peer)))
    }

    
    
    pub fn record(&self, folder_id: &[u8; 16], rec: &AgreedRecord) -> Result<(), AgreementError> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join(format!(
            ".tmp-{}-{}",
            hex(folder_id),
            hex(&rec.peer_device_id)
        ));
        std::fs::write(&tmp, encode_agreed_record(rec))?;
        std::fs::rename(&tmp, self.path_for(folder_id, &rec.peer_device_id))?;
        Ok(())
    }

    
    
    pub fn get(
        &self,
        folder_id: &[u8; 16],
        peer: &[u8; 32],
    ) -> Result<Option<AgreedRecord>, AgreementError> {
        let p = self.path_for(folder_id, peer);
        match std::fs::read(&p) {
            Ok(bytes) => Ok(Some(parse_agreed_record(&bytes)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let alt = if self.dir.ends_with(".ferry/agreement") || self.dir.to_string_lossy().contains("/.ferry/") {
                    self.dir.parent().and_then(|p| p.parent()).map(|pr| pr.join("agreement").join(format!("{}-{}.agree", hex(folder_id), hex(peer))))
                } else {
                    self.dir.parent().map(|pr| pr.join(".ferry").join("agreement").join(format!("{}-{}.agree", hex(folder_id), hex(peer))))
                };
                if let Some(alt_p) = alt {
                    if alt_p != p && alt_p.is_file() {
                        if let Ok(bytes) = std::fs::read(&alt_p) {
                            return Ok(Some(parse_agreed_record(&bytes)?));
                        }
                    }
                }
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn forget(&self, folder_id: &[u8; 16], peer: &[u8; 32]) -> Result<bool, AgreementError> {
        match std::fs::remove_file(self.path_for(folder_id, peer)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    fn list_from_dir(&self, dir: &std::path::Path, folder_id: &[u8; 16]) -> Result<Vec<([u8; 32], AgreedRecord)>, AgreementError> {
        let prefix = format!("{}-", hex(folder_id));
        let rd = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for entry in rd.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || !name.starts_with(&prefix) || !name.ends_with(".agree") {
                continue;
            }
            let Some(peer) =
                unhex::<32>(name.trim_start_matches(&prefix).trim_end_matches(".agree"))
            else {
                continue;
            };
            let bytes = std::fs::read(entry.path())?;
            out.push((peer, parse_agreed_record(&bytes)?));
        }
        Ok(out)
    }

    pub fn list_folder(
        &self,
        folder_id: &[u8; 16],
    ) -> Result<Vec<([u8; 32], AgreedRecord)>, AgreementError> {
        let mut list = self.list_from_dir(&self.dir, folder_id)?;
        let alt_dir = if self.dir.ends_with(".ferry/agreement") || self.dir.to_string_lossy().contains("/.ferry/") {
            self.dir.parent().and_then(|p| p.parent()).map(|pr| pr.join("agreement"))
        } else {
            self.dir.parent().map(|pr| pr.join(".ferry").join("agreement"))
        };
        if let Some(alt) = alt_dir {
            if alt != self.dir && alt.is_dir() {
                if let Ok(alt_list) = self.list_from_dir(&alt, folder_id) {
                    let mut seen: std::collections::BTreeSet<[u8; 32]> = list.iter().map(|(p, _)| *p).collect();
                    for item in alt_list {
                        if seen.insert(item.0) {
                            list.push(item);
                        }
                    }
                }
            }
        }
        list.sort_by_key(|(peer, _)| *peer);
        Ok(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> AgreedRecord {
        AgreedRecord {
            peer_device_id: core::array::from_fn(|i| i as u8),
            manifest_id: core::array::from_fn(|i| (i as u8).wrapping_mul(3)),
            agreed_sec: 1_700_000_123,
            agreed_nsec: 999_999_999,
        }
    }

    #[test]
    fn golden_bytes_pin_the_documented_layout_byte_for_byte() {
        
        
        
        let rec = fixture();
        let bytes = encode_agreed_record(&rec);
        assert_eq!(bytes.len(), 77);
        
        let expect: [u8; 77] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 
            0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 
            0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x00, 0x03, 0x06, 0x09, 
            0x0c, 0x0f, 0x12, 0x15, 0x18, 0x1b, 0x1e, 0x21, 0x24, 0x27, 0x2a, 0x2d, 
            0x30, 0x33, 0x36, 0x39, 0x3c, 0x3f, 0x42, 0x45, 0x48, 0x4b, 0x4e, 0x51, 
            0x54, 0x57, 0x5a, 0x5d, 
            0x7b, 0xf1, 0x53, 0x65, 0x00, 0x00, 0x00, 0x00, 
            0xff, 0xc9, 0x9a, 0x3b, 
            0x00, 
        ];
        assert_eq!(bytes, expect);
        assert_eq!(parse_agreed_record(&bytes).unwrap(), rec);

        
        let mut neg = fixture();
        neg.agreed_sec = -42;
        assert_eq!(
            parse_agreed_record(&encode_agreed_record(&neg)).unwrap(),
            neg
        );
    }

    #[test]
    fn parse_rejects_wrong_length_and_nonzero_flags() {
        let bytes = encode_agreed_record(&fixture());
        
        let mut long = bytes.to_vec();
        long.push(0);
        for bad in [&bytes[..0], &bytes[..1], &bytes[..76], &long[..]] {
            assert!(parse_agreed_record(bad).is_err());
        }
        let mut flagged = bytes;
        flagged[76] = 1;
        assert!(matches!(
            parse_agreed_record(&flagged),
            Err(AgreementError::BadFlags)
        ));
    }

    #[test]
    fn ledger_round_trips_through_disk_paths_per_folder_and_peer() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = AgreementLedger::new(tmp.path());

        assert_eq!(ledger.get(&[7; 16], &[9; 32]).unwrap(), None);
        assert!(ledger.list_folder(&[7; 16]).unwrap().is_empty());

        let rec = fixture();
        ledger.record(&[7; 16], &rec).unwrap();
        assert_eq!(
            ledger.get(&[7; 16], &rec.peer_device_id).unwrap(),
            Some(rec.clone())
        );

        
        assert_eq!(ledger.get(&[8; 16], &rec.peer_device_id).unwrap(), None);
        let mut other = rec.clone();
        other.manifest_id = [1; 32];
        ledger.record(&[8; 16], &other).unwrap();
        assert_eq!(
            ledger.get(&[8; 16], &rec.peer_device_id).unwrap(),
            Some(other)
        );
        assert_eq!(
            ledger.get(&[7; 16], &rec.peer_device_id).unwrap(),
            Some(rec.clone())
        );

        
        let mut newer = rec.clone();
        newer.agreed_sec += 5;
        ledger.record(&[7; 16], &newer).unwrap();
        assert_eq!(
            ledger.get(&[7; 16], &rec.peer_device_id).unwrap(),
            Some(newer.clone())
        );

        
        
        let reopened = AgreementLedger::new(tmp.path());
        assert_eq!(
            reopened.get(&[7; 16], &newer.peer_device_id).unwrap(),
            Some(newer.clone())
        );

        
        let listed = reopened.list_folder(&[7; 16]).unwrap();
        assert_eq!(listed, vec![(newer.peer_device_id, newer.clone())]);
        assert_eq!(reopened.list_folder(&[8; 16]).unwrap().len(), 1);

        
        assert!(reopened.forget(&[8; 16], &rec.peer_device_id).unwrap());
        assert_eq!(reopened.list_folder(&[8; 16]).unwrap(), Vec::new());
        assert!(!reopened.forget(&[8; 16], &rec.peer_device_id).unwrap());

        
        let leftovers: Vec<_> = std::fs::read_dir(&ledger.dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());

        
        let path = ledger.path_for(&[7; 16], &newer.peer_device_id);
        std::fs::write(&path, vec![0u8; 10]).unwrap();
        assert!(matches!(
            ledger.get(&[7; 16], &newer.peer_device_id),
            Err(AgreementError::BadLength { .. })
        ));
        assert!(matches!(
            ledger.list_folder(&[7; 16]),
            Err(AgreementError::BadLength { .. })
        ));
    }
}
