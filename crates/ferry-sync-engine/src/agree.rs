//! Last-agreed manifest pointers per folder per peer (ADR-0004), in the
//! canonical record format from `docs/store-format.md`
//! ("Last-agreed manifest pointer"):
//!
//! ```text
//! 32B peer_device_id    # the peer's X25519 public key
//! 32B manifest_id       # the manifest both sides agreed on
//! i64 LE agreed_sec     # local wall clock when agreement was recorded
//! u32 LE agreed_nsec
//! u8  flags             # 0 in v1
//! ```
//!
//! 77 bytes, little-endian, no framing. The record is local state; it is
//! never transmitted.
//!
//! Storage layout decision: ONE FILE PER PEER at
//! `<state_dir>/peers/<64-lowercase-hex-of-peer>.agreed`, each holding
//! exactly one canonical record. The synced folder is implied by which tree
//! the state dir belongs to, so a per-folder-per-peer pointer needs no
//! folder key inside the record and no multi-record framing. A combined
//! database was rejected: per-peer files make replacement atomic per peer,
//! a truncated write cannot corrupt unrelated peers, and the state stays
//! inspectable with `xxd`.
//!
//! Semantics: absent file means "never agreed with this peer" (initial
//! sync). A file that is the wrong size, has trailing bytes, carries
//! nonzero flags, or names a different peer than its own file name is
//! CORRUPT and loads as a loud error, never as `None`.

use std::path::{Path, PathBuf};

use ferry_store::format::{hex, unhex, put_bytes, put_i64, put_u32, Reader};
use thiserror::Error;

/// Size of one v1 canonical record.
pub const RECORD_LEN: usize = 77;

pub type DeviceId = [u8; 32];

/// One agreement with one peer about one folder's manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgreedRecord {
    pub peer_device_id: DeviceId,
    pub manifest_id: DeviceId,
    pub agreed_sec: i64,
    pub agreed_nsec: u32,
}

#[derive(Debug, Error)]
pub enum StateError {
    #[error("agreement state at {path} is corrupt: {reason}")]
    Corrupt { path: PathBuf, reason: &'static str },
    #[error("io failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

fn io_at(path: impl Into<PathBuf>, e: std::io::Error) -> StateError {
    StateError::Io {
        path: path.into(),
        source: e,
    }
}

/// Canonical serialization (normative shape from docs/store-format.md).
pub fn serialize_agreed_record(r: &AgreedRecord) -> Vec<u8> {
    let mut out = Vec::with_capacity(RECORD_LEN);
    put_bytes(&mut out, &r.peer_device_id);
    put_bytes(&mut out, &r.manifest_id);
    put_i64(&mut out, r.agreed_sec);
    put_u32(&mut out, r.agreed_nsec);
    out.push(0); // flags
    out
}

/// Strict parse of a canonical record. Anything but exactly 77 well-formed
/// bytes with zero flags is an error.
pub fn parse_agreed_record(bytes: &[u8]) -> Result<AgreedRecord, &'static str> {
    if bytes.len() != RECORD_LEN {
        return Err("wrong length");
    }
    let mut rd = Reader::new(bytes);
    let peer = rd.array::<32>().map_err(|_| "truncated")?;
    let manifest = rd.array::<32>().map_err(|_| "truncated")?;
    let sec = rd.i64().map_err(|_| "truncated")?;
    let nsec = rd.u32().map_err(|_| "truncated")?;
    match rd.u8().map_err(|_| "truncated")? {
        0 => {}
        _ => return Err("reserved flags byte nonzero"),
    }
    Ok(AgreedRecord {
        peer_device_id: peer,
        manifest_id: manifest,
        agreed_sec: sec,
        agreed_nsec: nsec,
    })
}

/// Filesystem home of the per-peer agreement records for one synced folder.
#[derive(Clone, Debug)]
pub struct PeerState {
    peers_dir: PathBuf,
}

impl PeerState {
    /// `state_dir` is the folder's `.ferry` directory (or a stand-in in
    /// tests); records live under `<state_dir>/peers/`.
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        PeerState {
            peers_dir: state_dir.into().join("peers"),
        }
    }

    fn path_for(&self, peer: &DeviceId) -> PathBuf {
        self.peers_dir.join(format!("{}.agreed", hex(peer)))
    }

    /// Load the last agreement with `peer`. Absent file → `Ok(None)`.
    /// Present-but-wrong anything → loud error.
    pub fn load(&self, peer: &DeviceId) -> Result<Option<AgreedRecord>, StateError> {
        let path = self.path_for(peer);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_at(path, e)),
        };
        let rec = parse_agreed_record(&bytes).map_err(|reason| StateError::Corrupt {
            path: path.clone(),
            reason,
        })?;
        if &rec.peer_device_id != peer {
            return Err(StateError::Corrupt {
                path,
                reason: "record names a different peer than its file",
            });
        }
        Ok(Some(rec))
    }

    /// Record (or overwrite) the agreement with a peer. Atomic via temp file
    /// plus rename within the same directory.
    pub fn record(&self, rec: &AgreedRecord) -> Result<(), StateError> {
        std::fs::create_dir_all(&self.peers_dir).map_err(|e| io_at(&self.peers_dir, e))?;
        let path = self.path_for(&rec.peer_device_id);
        let tmp = self.peers_dir.join(format!(
            ".{}.tmp",
            hex(&rec.peer_device_id)
        ));
        std::fs::write(&tmp, serialize_agreed_record(rec)).map_err(|e| io_at(&tmp, e))?;
        std::fs::rename(&tmp, &path).map_err(|e| io_at(&path, e))?;
        Ok(())
    }

    /// Forget a peer's agreement (e.g. after disconnection). Returns whether
    /// a record existed.
    pub fn forget(&self, peer: &DeviceId) -> Result<bool, StateError> {
        let path = self.path_for(peer);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(io_at(path, e)),
        }
    }
}

/// Parse a device id out of a `.agreed` file name (used by tests and future
/// listing commands).
pub fn peer_from_path(path: &Path) -> Option<DeviceId> {
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_suffix(".agreed")?;
    unhex::<32>(stem)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_state() -> (tempfile::TempDir, PeerState) {
        let dir = tempfile::tempdir().unwrap();
        let ps = PeerState::new(dir.path());
        (dir, ps)
    }

    #[test]
    fn canonical_serialization_is_77_bytes_in_spec_field_order() {
        let rec = AgreedRecord {
            peer_device_id: [0xAA; 32],
            manifest_id: [0x22; 32],
            agreed_sec: 0x10000000,
            agreed_nsec: 999_999_999,
        };
        let bytes = serialize_agreed_record(&rec);
        assert_eq!(bytes.len(), RECORD_LEN);
        assert_eq!(&bytes[..32], &[0xAA; 32]);
        assert_eq!(&bytes[32..64], &[0x22; 32]);
        assert_eq!(&bytes[64..72], &0x10000000i64.to_le_bytes());
        assert_eq!(&bytes[72..76], &999_999_999u32.to_le_bytes());
        assert_eq!(bytes[76], 0);
        assert_eq!(parse_agreed_record(&bytes).unwrap(), rec);
    }

    #[test]
    fn load_or_absent_and_round_trip_through_the_directory() {
        let (_dir, ps) = tmp_state();
        let peer = [7u8; 32];
        assert_eq!(ps.load(&peer).unwrap(), None, "absent means initial sync");

        let rec = AgreedRecord {
            peer_device_id: peer,
            manifest_id: [9; 32],
            agreed_sec: 1_787_574_896,
            agreed_nsec: 42,
        };
        ps.record(&rec).unwrap();
        assert_eq!(ps.load(&peer).unwrap(), Some(rec.clone()));

        // Overwrite advances the pointer.
        let mut next = rec.clone();
        next.manifest_id = [10; 32];
        ps.record(&next).unwrap();
        assert_eq!(ps.load(&peer).unwrap(), Some(next));

        assert!(ps.forget(&peer).unwrap());
        assert_eq!(ps.load(&peer).unwrap(), None);
        assert!(!ps.forget(&peer).unwrap());
    }

    #[test]
    fn corrupt_state_is_a_loud_error_never_none() {
        let (_dir, ps) = tmp_state();
        let peer = [3u8; 32];
        ps.record(&AgreedRecord {
            peer_device_id: peer,
            manifest_id: [4; 32],
            agreed_sec: 5,
            agreed_nsec: 6,
        })
        .unwrap();
        let path = ps.path_for(&peer);

        // Truncated.
        let full = std::fs::read(&path).unwrap();
        std::fs::write(&path, &full[..70]).unwrap();
        assert!(matches!(
            ps.load(&peer),
            Err(StateError::Corrupt { reason: "wrong length", .. })
        ));

        // Trailing garbage.
        let mut padded = full.clone();
        padded.push(0);
        std::fs::write(&path, &padded).unwrap();
        assert!(matches!(
            ps.load(&peer),
            Err(StateError::Corrupt { reason: "wrong length", .. })
        ));

        // Nonzero flags byte.
        let mut flagged = full.clone();
        flagged[76] = 1;
        std::fs::write(&path, &flagged).unwrap();
        assert!(matches!(
            ps.load(&peer),
            Err(StateError::Corrupt { .. })
        ));

        // Record body names a different peer than the file.
        let mut swapped = full;
        swapped[0] ^= 0xFF;
        std::fs::write(&path, &swapped).unwrap();
        assert!(matches!(
            ps.load(&peer),
            Err(StateError::Corrupt { .. })
        ));
    }

    #[test]
    fn peer_from_path_reads_back_the_hex_name() {
        let (_dir, ps) = tmp_state();
        let peer = [0xAB; 32];
        ps.record(&AgreedRecord {
            peer_device_id: peer,
            manifest_id: [1; 32],
            agreed_sec: 0,
            agreed_nsec: 0,
        })
        .unwrap();
        assert_eq!(peer_from_path(&ps.path_for(&peer)), Some(peer));
    }
}
