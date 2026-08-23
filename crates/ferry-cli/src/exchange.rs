//! One exchange round between two Ferry folders over the existing M0
//! transport seam (`ferry_sync::transport`) speaking the existing M0
//! message inventory (`ferry_sync::proto`: HELLO/OFFER/REQ_META/REQ_DATA/
//! ITEM/ITEMS_DONE). T-009/T-014 replace the TCP implementation and the
//! wire under these same seams; the flow here is v0-honest and documented.
//!
//! # Round script (strictly alternating; dialer always speaks first)
//!
//! ```text
//! D -> L  HELLO {tag = ferry-<dev8>-<folder8>}     L -> D HELLO
//! both    OFFER {manifest bytes, agreed id, agreed root}
//!         -- roots equal? settle agreement (min manifest id), close --
//! D->L    REQ_META* until frontier satisfied (empty REQ_META = done)
//! L->D    ITEM* ITEMS_DONE per request
//! L->D    REQ_META* until satisfied                 D serves
//! D->L    REQ_DATA(plan_D.fetch)                    L serves blobs
//! L->D    REQ_DATA(plan_L.fetch)                    D serves blobs
//!         close; each side executes its own reconcile plan, rescans,
//!         records agreement on the NEXT equal-roots round
//! ```
//!
//! Decisions worth recording:
//! - **Agreement = min(manifest ids) at an equal-roots OFFER.** Manifests
//!   embed device id + timestamps so two converged trees still produce two
//!   manifest ids; picking the lexicographically smaller id is deterministic
//!   and identical on both sides. Its bytes are already in both stores.
//! - **Reconcile is three-way** (ferry-sync-engine): local, remote, and the
//!   last-agreed base. Conflicts quarantine loudly per ADR-0004.
//! - **Blobs move individually** in v0 (no pack-granular transfer yet);
//!   every chunk is verified `BLAKE3(bytes) == id` before touching disk.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use ferry_store::format::{hex, BlobId, BlobKind};
use ferry_store::manifest::{parse_manifest, RootManifest};
use ferry_store::store::Store;
use ferry_sync::proto::{self, ItemPayload, ItemStream};
use ferry_sync::transport::Connection;
use ferry_sync_engine::{execute, reconcile, AgreedRecord, PeerState};

use crate::error::{CliError, CliResult};

/// Everything one folder needs to run a round.
pub struct FolderSession {
    /// The `.ferry` directory (state_dir for agreements + conflicts.jsonl).
    pub state_dir: PathBuf,
    pub tree_root: PathBuf,
    pub store: Arc<Store>,
    pub folder_id: [u8; 16],
    pub device_id: [u8; 32],
    /// The folder chunker polynomial (from the store's polynomial record).
    pub poly: u64,
}

impl FolderSession {
    pub fn hello_tag(&self) -> String {
        format!(
            "ferry-{}-{}",
            &hex(&self.device_id)[..8],
            &hex(&self.folder_id)[..8]
        )
    }
}

/// What one round did (also the daemon's NDJSON event payload).
#[derive(Debug, Clone, Default)]
pub struct RoundReport {
    pub peer_device_id: Option<String>,
    pub roots_equal_at_offer: bool,
    pub meta_fetched: usize,
    pub chunks_sent: usize,
    pub chunks_received: usize,
    pub ops_applied: usize,
    pub quarantined: usize,
    pub conflicts_recorded: usize,
    /// True when this round ended with agreement recorded (equal roots).
    pub agreed: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ExchangeError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol: {0}")]
    Proto(#[from] ferry_sync::proto::ProtoError),
    #[error("store: {0}")]
    Store(#[from] ferry_store::store::StoreError),
    #[error("reconcile: {0}")]
    Reconcile(#[from] ferry_sync_engine::ReconcileError),
    #[error("execute: {0}")]
    Execute(#[from] ferry_sync_engine::execute::EngineError),
    #[error("manifest: {0}")]
    Manifest(#[from] ferry_store::manifest::ManifestError),
    #[error("{0}")]
    Other(String),
}

impl From<ExchangeError> for CliError {
    fn from(e: ExchangeError) -> CliError {
        CliError::new(
            "exchange",
            e.to_string(),
            "check that the peer daemon is running with a matching folder; retry `ferry sync`",
        )
    }
}

/// Run one full round as the dialer (`dialer = true`) or listener side.
/// `my` is the caller's CURRENT snapshot (scan before calling).
/// `pre_greeted` carries the peer's tag when the HELLO was already
/// exchanged (the daemon's accept loop reads it first for routing).
pub fn run_round(
    conn: &mut dyn Connection,
    dialer: bool,
    session: &FolderSession,
    my: &Snapshot,
    mut peer_tag: Option<String>,
) -> Result<RoundReport, ExchangeError> {
    let mut report = RoundReport::default();

    // --- hello ------------------------------------------------------------
    let tag = session.hello_tag();
    if peer_tag.is_none() {
        if dialer {
            proto::send_hello(conn, &tag)?;
            peer_tag = Some(proto::recv_hello(conn)?.device_tag);
        } else {
            let h = proto::recv_hello(conn)?.device_tag;
            proto::send_hello(conn, &tag)?;
            peer_tag = Some(h);
        }
    }
    let _ = peer_tag;

    // --- offers -----------------------------------------------------------
    let my_offer = proto::Offer {
        manifest_bytes: my.manifest_bytes.clone(),
        agreed_manifest_id: [0; 32], // informational in v0; base comes from local records
        agreed_root_tree_id: [0; 32],
    };
    if dialer {
        proto::send_offer(conn, &my_offer)?;
    }
    let theirs = if dialer {
        proto::recv_offer(conn)?
    } else {
        // Listener answers in the opposite order.
        let theirs = proto::recv_offer(conn)?;
        proto::send_offer(conn, &my_offer)?;
        theirs
    };

    let their_manifest = parse_manifest(&theirs.manifest_bytes)?;
    let their_manifest_id: BlobId = *blake3::hash(&theirs.manifest_bytes).as_bytes();
    report.peer_device_id = Some(hex(&their_manifest.device_id));

    if their_manifest.folder_id != session.folder_id {
        return Err(ExchangeError::Other(format!(
            "peer offered folder {}, we are {}",
            hex(&their_manifest.folder_id),
            hex(&session.folder_id)
        )));
    }

    // Store their manifest so agreement records can reference it later.
    session.store.put_meta(BlobKind::Manifest, &theirs.manifest_bytes)?;

    if my.manifest.root_tree_id == their_manifest.root_tree_id {
        // Converged: settle agreement deterministically on BOTH sides.
        let agreed_manifest = if my.manifest_id < their_manifest_id {
            (&my.manifest, my.manifest_id)
        } else {
            (&their_manifest, their_manifest_id)
        };
        record_agreement(session, &their_manifest.device_id, agreed_manifest.1)
            .map_err(|e| ExchangeError::Other(e.to_string()))?;
        report.roots_equal_at_offer = true;
        report.agreed = true;
        return Ok(report);
    }

    // --- meta phase: dialer pulls first -----------------------------------
    if dialer {
        report.meta_fetched += request_meta(conn, session, &their_manifest.root_tree_id)?;
        serve_meta(conn, session)?;
    } else {
        serve_meta(conn, session)?;
        report.meta_fetched += request_meta(conn, session, &their_manifest.root_tree_id)?;
    }

    // Both sides now hold all tree nodes; compute our three-way plan.
    let plan = compute_plan(session, &my.manifest, &their_manifest)?;

    // --- data phase: dialer requests first --------------------------------
    let wanted: Vec<BlobId> = plan
        .fetch
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| session.store.get(BlobKind::DataChunk, id).is_err())
        .collect();
    if dialer {
        report.chunks_received += request_data(conn, session, &wanted)?;
        report.chunks_sent += serve_data(conn, session)?;
    } else {
        report.chunks_sent += serve_data(conn, session)?;
        report.chunks_received += request_data(conn, session, &wanted)?;
    }

    // Connection work done; apply our plan locally.
    let now = ferry_sync_engine::timefmt::now_unix();
    let stats = execute(&session.store, &session.tree_root, &plan, Some(&session.state_dir), now)?;
    report.ops_applied = stats.apply.mutations();
    report.quarantined = stats.quarantined.len();
    report.conflicts_recorded = stats.conflicts.len();
    Ok(report)
}

/// A scanned current state for one folder.
pub struct Snapshot {
    pub manifest: RootManifest,
    pub manifest_bytes: Vec<u8>,
    pub manifest_id: BlobId,
}

pub fn scan_snapshot(session: &FolderSession) -> CliResult<Snapshot> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let identity = ferry_store::snapshot::SnapshotIdentity {
        folder_id: session.folder_id,
        device_id: session.device_id,
        parent_manifest_id: [0u8; 32],
        created_sec: d.as_secs() as i64,
        created_nsec: d.subsec_nanos(),
    };
    let out = ferry_store::snapshot::snapshot_dir(&session.store, session.poly, &session.tree_root, &identity)
        .map_err(|e| CliError::new("scan", e.to_string(), "check the folder for unreadable paths"))?;
    Ok(Snapshot {
        manifest_bytes: ferry_store::manifest::serialize_manifest(&out.manifest),
        manifest: out.manifest,
        manifest_id: out.manifest_id,
    })
}

// ---------------------------------------------------------------------------
// meta phase helpers
// ---------------------------------------------------------------------------

fn request_meta(
    conn: &mut dyn Connection,
    session: &FolderSession,
    root_tree: &BlobId,
) -> Result<usize, ExchangeError> {
    let mut fetched = 0usize;
    let mut frontier: Vec<BlobId> = vec![*root_tree];
    loop {
        let missing: Vec<(BlobKind, BlobId)> = frontier
            .iter()
            .filter(|id| session.store.get(BlobKind::TreeNode, id).is_err())
            .map(|id| (BlobKind::TreeNode, *id))
            .collect();
        proto::send_req_meta(conn, &missing)?;
        if missing.is_empty() {
            break; // terminator round; server replies with an empty stream
        }
        let mut got: HashSet<BlobId> = HashSet::new();
        while got.len() < missing.len() {
            match proto::recv_item_stream(conn)? {
                ItemStream::Done => {
                    return Err(ExchangeError::Other("tree-node stream ended early".into()))
                }
                ItemStream::Item(ItemPayload::Blob {
                    kind: BlobKind::TreeNode,
                    id,
                    bytes,
                }) => {
                    verify_blob(&id, &bytes)?;
                    session.store.put_meta(BlobKind::TreeNode, &bytes)?;
                    got.insert(id);
                    fetched += 1;
                }
                ItemStream::Item(other) => {
                    return Err(ExchangeError::Other(format!("expected tree node, got {other:?}")))
                }
            }
        }
        proto::recv_items_done(conn)?;
        // Expand frontier through children of everything now present.
        let mut next = Vec::new();
        for id in &frontier {
            if let Ok(bytes) = session.store.get(BlobKind::TreeNode, id) {
                if let Ok(node) = ferry_store::manifest::parse_tree_node(&bytes) {
                    for e in node.entries {
                        if let ferry_store::manifest::EntryPayload::Dir { child_tree_id } = e.payload {
                            next.push(child_tree_id);
                        }
                    }
                }
            }
        }
        frontier = next;
    }
    Ok(fetched)
}

fn serve_meta(conn: &mut dyn Connection, session: &FolderSession) -> Result<(), ExchangeError> {
    loop {
        let req = proto::recv_req_meta(conn)?;
        for (kind, id) in &req {
            let bytes = session.store.get(*kind, id)?;
            proto::send_item(conn, &ItemPayload::Blob { kind: *kind, id: *id, bytes })?;
        }
        proto::send_items_done(conn)?;
        if req.is_empty() {
            break; // peer signalled satisfaction
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// data phase helpers
// ---------------------------------------------------------------------------

fn request_data(
    conn: &mut dyn Connection,
    session: &FolderSession,
    chunk_ids: &[BlobId],
) -> Result<usize, ExchangeError> {
    proto::send_req_data(conn, chunk_ids)?;
    let mut received = 0usize;
    loop {
        match proto::recv_item_stream(conn)? {
            ItemStream::Done => break,
            ItemStream::Item(ItemPayload::Blob {
                kind: BlobKind::DataChunk,
                id,
                bytes,
            }) => {
                verify_blob(&id, &bytes)?;
                session.store.put_blob(BlobKind::DataChunk, &bytes)?;
                received += 1;
            }
            ItemStream::Item(other) => {
                return Err(ExchangeError::Other(format!("expected data chunk, got {other:?}")))
            }
        }
    }
    if !chunk_ids.is_empty() && received < chunk_ids.len() {
        // Peer may legitimately skip chunks it lacks; surface loudly but
        // non-fatally — execution will fail below if something vital missed.
        eprintln!(
            "warning: requested {} chunk(s), received {} — missing chunks may abort application",
            chunk_ids.len(),
            received
        );
    }
    // Seal staged blobs so reads see them, then teach the index about any
    // newly landed packs.
    session.store.flush()?;
    Ok(received)
}

fn serve_data(conn: &mut dyn Connection, session: &FolderSession) -> Result<usize, ExchangeError> {
    let ids = proto::recv_req_data(conn)?;
    let mut served = 0usize;
    for id in &ids {
        if let Ok(bytes) = session.store.get(BlobKind::DataChunk, id) {
            proto::send_item(
                conn,
                &ItemPayload::Blob {
                    kind: BlobKind::DataChunk,
                    id: *id,
                    bytes,
                },
            )?;
            served += 1;
        }
        // Missing chunks are skipped; requester warns on shortfall.
    }
    proto::send_items_done(conn)?;
    Ok(served)
}

fn verify_blob(id: &BlobId, bytes: &[u8]) -> Result<(), ExchangeError> {
    let found: BlobId = *blake3::hash(bytes).as_bytes();
    if &found != id {
        return Err(ExchangeError::Other(format!(
            "verify-after-receipt failed: expected {}, got {}",
            hex(id),
            hex(&found)
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// reconcile / agreement helpers
// ---------------------------------------------------------------------------

fn compute_plan(
    session: &FolderSession,
    local: &RootManifest,
    remote: &RootManifest,
) -> Result<ferry_sync_engine::ActionPlan, ExchangeError> {
    let ps = PeerState::new(&session.state_dir);
    let base = load_base(session, &ps, &remote.device_id);
    Ok(reconcile(ferry_sync_engine::reconcile::ReconcileInput {
        store: &session.store,
        local,
        remote,
        base: base.as_ref(),
    })?)
}

/// Parse the last-agreed manifest object for a peer; None when never agreed
/// or when its bytes are gone from the store (documented degradation).
fn load_base(
    session: &FolderSession,
    ps: &PeerState,
    peer: &[u8; 32],
) -> Option<RootManifest> {
    let rec = ps.load(peer).ok().flatten()?;
    let bytes = session.store.get(BlobKind::Manifest, &rec.manifest_id).ok()?;
    parse_manifest(&bytes).ok()
}

fn record_agreement(
    session: &FolderSession,
    peer: &[u8; 32],
    manifest_id: BlobId,
) -> CliResult<()> {
    let ps = PeerState::new(&session.state_dir);
    let (sec, nsec) = ferry_sync_engine::timefmt::now_unix();
    ps.record(&AgreedRecord {
        peer_device_id: *peer,
        manifest_id,
        agreed_sec: sec,
        agreed_nsec: nsec,
    })
    .map_err(|e| CliError::new("agreement-state", e.to_string(), "check .ferry/peers permissions"))?;
    Ok(())
}
