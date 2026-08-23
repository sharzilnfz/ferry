//! Agreement bookkeeping: the last-agreed manifest pointer, per peer.
//!
//! Record shape is the one specified in `docs/store-format.md`
//! ("Last-agreed manifest pointer"): peer device id, manifest id, local
//! wall-clock timestamp, flags. It is LOCAL state, never transmitted; peers
//! re-derive agreement by exchanging manifests (which is exactly what the
//! OFFER/AGREED flow does). T-010's three-way reconciliation consumes these
//! records as base state.
//!
//! M0 storage layout under `<store>/.ferry/sync/<peer-tag>/`:
//! - `agreed.bin`  — the spec-shaped record
//! - `manifest.bin` — the agreed manifest's full serialization, kept so a
//!   restart can recover the baseline ROOT TREE id without the network.
//!   (M0-local convenience file, not part of the compatibility contract.)
//!
//! Crypto note: with encryption OFF these files are plaintext. T-007/T-008
//! wrap this directory like every other store secret.

use std::path::{Path, PathBuf};

use ferry_store::format::{put_bytes, put_i64, put_u32, put_u8, BlobId, Reader};

use crate::proto::ProtoError;

/// 32 + 32 + 8 + 4 + 1.
pub const AGREED_RECORD_LEN: usize = 77;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgreedRecord {
    pub peer_device_id: BlobId,
    pub manifest_id: BlobId,
    pub agreed_sec: i64,
    pub agreed_nsec: u32,
    /// Always 0 in v1.
    pub flags: u8,
}

pub fn encode_agreed_record(r: &AgreedRecord) -> Vec<u8> {
    let mut b = Vec::with_capacity(AGREED_RECORD_LEN);
    put_bytes(&mut b, &r.peer_device_id);
    put_bytes(&mut b, &r.manifest_id);
    put_i64(&mut b, r.agreed_sec);
    put_u32(&mut b, r.agreed_nsec);
    put_u8(&mut b, r.flags);
    b
}

pub fn parse_agreed_record(bytes: &[u8]) -> Result<AgreedRecord, ProtoError> {
    if bytes.len() != AGREED_RECORD_LEN {
        return Err(ProtoError::Malformed("agreed record length"));
    }
    let mut r = Reader::new(bytes);
    let rec = AgreedRecord {
        peer_device_id: r.array().map_err(|_| ProtoError::Malformed("peer id"))?,
        manifest_id: r
            .array()
            .map_err(|_| ProtoError::Malformed("manifest id"))?,
        agreed_sec: r.i64().map_err(|_| ProtoError::Malformed("sec"))?,
        agreed_nsec: r.u32().map_err(|_| ProtoError::Malformed("nsec"))?,
        flags: r.u8().map_err(|_| ProtoError::Malformed("flags"))?,
    };
    if rec.flags != 0 {
        return Err(ProtoError::Malformed("agreed flags must be zero in v1"));
    }
    Ok(rec)
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Proto(#[from] ProtoError),
    #[error("stored manifest failed to parse: {0}")]
    Manifest(#[from] ferry_store::manifest::ManifestError),
}

/// Durable per-peer agreement records rooted at `<store>/.ferry/sync`.
pub struct AgreementStore {
    root: PathBuf,
}

impl AgreementStore {
    pub fn new(store_dot_dir: &Path) -> Self {
        AgreementStore {
            root: store_dot_dir.join("sync"),
        }
    }

    fn peer_dir(&self, peer_tag: &str) -> PathBuf {
        self.root.join(peer_tag)
    }

    /// Atomically record agreement with `peer_tag`. Crash-safe via temp +
    /// rename in the same directory.
    pub fn record(
        &self,
        peer_tag: &str,
        record: AgreedRecord,
        manifest_bytes: &[u8],
    ) -> Result<(), StateError> {
        debug_assert_eq!(encode_agreed_record(&record).len(), AGREED_RECORD_LEN);
        let dir = self.peer_dir(peer_tag);
        std::fs::create_dir_all(&dir)?;
        write_atomic(
            &dir.join("agreed.bin.tmp"),
            &dir.join("agreed.bin"),
            &encode_agreed_record(&record),
        )?;
        write_atomic(
            &dir.join("manifest.bin.tmp"),
            &dir.join("manifest.bin"),
            manifest_bytes,
        )
    }

    /// Load the recorded agreement for `peer_tag`, including its manifest
    /// object so callers know the agreed ROOT TREE id.
    pub fn load(
        &self,
        peer_tag: &str,
    ) -> Result<Option<(AgreedRecord, ferry_store::manifest::RootManifest)>, StateError> {
        let dir = self.peer_dir(peer_tag);
        let rec_bytes = match std::fs::read(dir.join("agreed.bin")) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let m_bytes = std::fs::read(dir.join("manifest.bin"))?;
        let record = parse_agreed_record(&rec_bytes)?;
        let manifest = ferry_store::manifest::parse_manifest(&m_bytes)?;
        Ok(Some((record, manifest)))
    }
}

fn write_atomic(tmp: &Path, final_path: &Path, bytes: &[u8]) -> Result<(), StateError> {
    {
        use std::io::Write;
        let mut f = std::fs::File::create(tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(tmp, final_path)?;
    Ok(())
}

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
    fn record_codec_round_trips_and_refuses_truncation() {
        let rec = AgreedRecord {
            peer_device_id: [1; 32],
            manifest_id: [2; 32],
            agreed_sec: -42,
            agreed_nsec: 999_999_999,
            flags: 0,
        };
        let enc = encode_agreed_record(&rec);
        assert_eq!(enc.len(), AGREED_RECORD_LEN);
        assert_eq!(parse_agreed_record(&enc).unwrap(), rec);

        assert!(parse_agreed_record(&enc[..enc.len() - 1]).is_err());
        let mut bad = enc.clone();
        bad[76] = 1; // flags must be zero in v1
        assert!(parse_agreed_record(&bad).is_err());
    }

    #[test]
    fn records_persist_and_reload_per_peer() {
        let dir = tempfile::tempdir().unwrap();
        let dot = dir.path().join(".ferry");
        std::fs::create_dir_all(&dot).unwrap();
        let store = AgreementStore::new(&dot);

        let manifest = ferry_store::manifest::RootManifest {
            folder_id: [3; 16],
            device_id: [4; 32],
            created_sec: 10,
            created_nsec: 20,
            root_tree_id: [5; 32],
            parent_manifest_id: [0; 32],
        };
        let bytes = ferry_store::manifest::serialize_manifest(&manifest);
        let mid = *blake3::hash(&bytes).as_bytes();

        assert!(
            store.load("peer-a").unwrap().is_none(),
            "nothing recorded yet"
        );

        store
            .record(
                "peer-a",
                AgreedRecord {
                    peer_device_id: device_id_from_tag("peer-a"),
                    manifest_id: mid,
                    agreed_sec: 1,
                    agreed_nsec: 2,
                    flags: 0,
                },
                &bytes,
            )
            .unwrap();

        let (rec, loaded) = store.load("peer-a").unwrap().unwrap();
        assert_eq!(rec.manifest_id, mid);
        assert_eq!(loaded.root_tree_id, [5; 32]);
        assert_eq!(store.load("peer-b").unwrap(), None, "records are per-peer");

        // Overwrite wins atomically.
        store
            .record(
                "peer-a",
                AgreedRecord {
                    peer_device_id: device_id_from_tag("peer-a"),
                    manifest_id: [9; 32],
                    agreed_sec: 3,
                    agreed_nsec: 4,
                    flags: 0,
                },
                &bytes,
            )
            .unwrap();
        assert_eq!(
            store.load("peer-a").unwrap().unwrap().0.manifest_id,
            [9; 32]
        );
    }

    #[test]
    fn device_ids_are_deterministic_and_tag_separated() {
        assert_eq!(device_id_from_tag("a"), device_id_from_tag("a"));
        assert_ne!(device_id_from_tag("a"), device_id_from_tag("b"));
    }
}
