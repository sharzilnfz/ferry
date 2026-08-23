//! Last-agreed manifest pointers, recorded per folder per peer
//! (`docs/store-format.md`, "Last-agreed manifest pointer"; ADR-0004).
//!
//! ferry-store ships no record type for this yet — the format doc fixes only
//! the canonical serialization — so this module implements read/write of
//! exactly those bytes inside ferry-proto's state helper. One file per
//! `(folder, peer)` under `<store>/agreement/`; file contents are the
//! canonical record, nothing else:
//!
//! ```text
//! 32B peer_device_id
//! 32B manifest_id
//! i64 LE agreed_sec
//! u32 LE agreed_nsec
//! u8  flags              # must be 0 in v1
//! ```
//!
//! 77 bytes total. This is LOCAL state: peers re-derive agreement by
//! exchanging manifests; the ledger is the three-way-reconciliation ancestor
//! pointer, never a wire message.

use std::path::{Path, PathBuf};

use thiserror::Error;
use zeroize::Zeroizing;

use ferry_store::format::{hex, unhex, BlobId};
use ferry_crypto::identity::DeviceId;

#[derive(Debug, Error)]
pub enum LedgerError {
    #[error("io error touching agreement ledger: {0}")]
    Io(#[from] std::io::Error),
    #[error("agreement record for {peer} is {len} bytes, expected 77")]
    BadLength { peer: String, len: usize },
    #[error("agreement record flags byte is nonzero; refusing v0-incompatible state")]
    BadFlags,
}

/// The canonical serialization length.
pub const RECORD_LEN: usize = 77;

/// One recorded agreement point.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgreementRecord {
    pub peer: DeviceId,
    pub manifest_id: BlobId,
    pub agreed_sec: i64,
    pub agreed_nsec: u32,
}

impl AgreementRecord {
    /// Canonical bytes, byte-for-byte per the store-format section.
    pub fn to_canonical(&self) -> [u8; RECORD_LEN] {
        let mut out = [0u8; RECORD_LEN];
        out[..32].copy_from_slice(&self.peer);
        out[32..64].copy_from_slice(&self.manifest_id);
        out[64..72].copy_from_slice(&self.agreed_sec.to_le_bytes());
        out[72..76].copy_from_slice(&self.agreed_nsec.to_le_bytes());
        out[76] = 0; // flags
        out
    }

    pub fn from_canonical(bytes: &[u8]) -> Result<Self, LedgerError> {
        if bytes.len() != RECORD_LEN {
            return Err(LedgerError::BadLength {
                peer: hex(&bytes[..32.min(bytes.len())]),
                len: bytes.len(),
            });
        }
        if bytes[76] != 0 {
            return Err(LedgerError::BadFlags);
        }
        Ok(AgreementRecord {
            peer: bytes[..32].try_into().expect("32 bytes"),
            manifest_id: bytes[32..64].try_into().expect("32 bytes"),
            agreed_sec: i64::from_le_bytes(bytes[64..72].try_into().expect("8 bytes")),
            agreed_nsec: u32::from_le_bytes(bytes[72..76].try_into().expect("4 bytes")),
        })
    }

    /// The secret-free timestamp material is not sensitive, but records are
    /// handled through [`Zeroizing`] on the wire-shaped buffer anyway so a
    /// torn write never lingers in scratch buffers longer than needed.
    pub(crate) fn to_zeroizing(&self) -> Zeroizing<[u8; RECORD_LEN]> {
        Zeroizing::new(self.to_canonical())
    }
}

/// Read/write access to `<store>/agreement/`.
pub struct AgreementLedger {
    dir: PathBuf,
}

impl AgreementLedger {
    pub fn new(store_dir: &Path) -> Self {
        AgreementLedger {
            dir: store_dir.join("agreement"),
        }
    }

    fn path_for(&self, folder_id: &[u8; 16], peer: &DeviceId) -> PathBuf {
        self.dir
            .join(format!("{}-{}.agree", hex(folder_id), hex(peer)))
    }

    /// Record (or overwrite) the last-agreed pointer. Atomic via temp +
    /// rename, matching every other Ferry write.
    pub fn record(
        &self,
        folder_id: &[u8; 16],
        rec: &AgreementRecord,
    ) -> Result<(), LedgerError> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.path_for(folder_id, &rec.peer);
        let tmp = self.dir.join(format!(
            ".tmp-{}-{}",
            hex(folder_id),
            hex(&rec.peer)
        ));
        std::fs::write(&tmp, rec.to_zeroizing().as_ref())?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Load the recorded pointer, if any. A corrupt or foreign record is an
    /// ERROR, never silently ignored: this value anchors reconciliation.
    pub fn get(
        &self,
        folder_id: &[u8; 16],
        peer: &DeviceId,
    ) -> Result<Option<AgreementRecord>, LedgerError> {
        let path = self.path_for(folder_id, peer);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(AgreementRecord::from_canonical(&bytes)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> AgreementRecord {
        AgreementRecord {
            peer: core::array::from_fn(|i| i as u8),
            manifest_id: core::array::from_fn(|i| (i as u8).wrapping_mul(3)),
            agreed_sec: 1_700_000_123,
            agreed_nsec: 999_999_999,
        }
    }

    #[test]
    fn canonical_serialization_matches_the_documented_layout_byte_for_byte() {
        let rec = fixture();
        let bytes = rec.to_canonical();
        assert_eq!(bytes.len(), 77);
        // Hand-computed expectation straight from docs/store-format.md.
        let mut expect = Vec::new();
        expect.extend_from_slice(&rec.peer); // 32B peer
        expect.extend_from_slice(&rec.manifest_id); // 32B manifest
        expect.extend_from_slice(&1_700_000_123i64.to_le_bytes()); // sec
        expect.extend_from_slice(&999_999_999u32.to_le_bytes()); // nsec
        expect.push(0u8); // flags
        assert_eq!(bytes.as_slice(), expect.as_slice());
        // Negative seconds survive (pre-1970 convention).
        let mut neg = fixture();
        neg.agreed_sec = -42;
        assert_eq!(
            AgreementRecord::from_canonical(&neg.to_canonical()).unwrap(),
            neg
        );
    }

    #[test]
    fn parse_rejects_wrong_length_and_nonzero_flags() {
        let bytes = fixture().to_canonical();
        // Too-short at every cut, plus one too-long input.
        let mut long = bytes.to_vec();
        long.push(0);
        for bad in [&bytes[..0], &bytes[..1], &bytes[..76], &long[..]] {
            assert!(AgreementRecord::from_canonical(bad).is_err());
        }
        let mut flagged = bytes;
        flagged[76] = 1;
        assert!(matches!(
            AgreementRecord::from_canonical(&flagged),
            Err(LedgerError::BadFlags)
        ));
    }

    #[test]
    fn ledger_round_trips_through_disk_paths_per_folder_and_peer() {
        let tmp = tempfile::tempdir().unwrap();
        let store_dir = tmp.path().join(".ferry");
        std::fs::create_dir_all(&store_dir).unwrap();
        let ledger = AgreementLedger::new(&store_dir);

        assert_eq!(ledger.get(&[7; 16], &[9; 32]).unwrap(), None);

        let rec = fixture();
        ledger.record(&[7; 16], &rec).unwrap();
        assert_eq!(ledger.get(&[7; 16], &rec.peer).unwrap().unwrap(), rec);

        // Different folder / different peer → independent slots.
        assert_eq!(ledger.get(&[8; 16], &rec.peer).unwrap(), None);
        let mut other = rec.clone();
        other.manifest_id = [1; 32];
        ledger.record(&[8; 16], &other).unwrap();
        assert_eq!(ledger.get(&[8; 16], &rec.peer).unwrap().unwrap(), other);
        assert_eq!(ledger.get(&[7; 16], &rec.peer).unwrap().unwrap(), rec);

        // Re-record overwrites atomically.
        let mut newer = rec.clone();
        newer.agreed_sec += 5;
        ledger.record(&[7; 16], &newer).unwrap();
        assert_eq!(ledger.get(&[7; 16], &rec.peer).unwrap().unwrap(), newer);

        // No stray temp files left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&ledger.dir).unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());

        // Corrupt on-disk record is a loud error.
        let path = ledger.path_for(&[7; 16], &newer.peer);
        std::fs::write(&path, vec![0u8; 10]).unwrap();
        assert!(matches!(
            ledger.get(&[7; 16], &newer.peer),
            Err(LedgerError::BadLength { .. })
        ));
        let _ = unhex::<16>("00"); // silence unused import if refactors move it
    }
}
