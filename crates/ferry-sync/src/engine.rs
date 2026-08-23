//! The skeleton engine: poll → snapshot → compare → exchange → materialize
//! → record agreement.
//!
//! Session shape (see [`crate::proto`]): two HELLOs, two OFFERs, then a
//! deterministic donor/puller split ([`pick_donor`]). The puller walks the
//! offered tree requesting missing TREE NODES as individual blobs, diffs the
//! offered manifest against its own, then requests the missing DATA CHUNKS
//! **by id**; the donor resolves each id to its owning PACK and streams
//! **whole packs by ciphertext name** (`BLAKE3(ciphertext) == name` is
//! verified on receipt before anything touches disk state), falling back to
//! individual blobs only for unmapped chunks. The puller ingests,
//! materializes durably, THEN confirms with AGREED; both sides record the
//! winning manifest as their last-agreed pointer.
//!
//! Why whole packs: `docs/store-format.md`'s wire note fixes packs as the
//! transfer unit precisely because name-verified ciphertext gives end-to-end
//! integrity for free, and exercising that unit early forces T-008 to deal
//! with resume-at-pack-granularity instead of inventing it later. Meta stays
//! individual because the puller needs tree nodes BEFORE it can compute what
//! data it wants.
//!
//! Concurrency: at most one session runs per daemon (a mutex serializes the
//! dialer and every accepted handler). Only the connect-role daemon dials;
//! the listen-role daemon serves and relies on the peer's opportunistic
//! dials (~1s) to discover its changes. Simultaneous edits on both sides
//! resolve by last-writer-wins and may LOSE the loser's changes — explicit
//! M0 scope, T-010 owns conflicts.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ferry_store::crypto::PassthroughCipher;
use ferry_store::diff::ChangeSet;
use ferry_store::format::{hex, BlobId, BlobKind, PackId};
use ferry_store::manifest::{parse_manifest, serialize_manifest, RootManifest};
use ferry_store::snapshot::{snapshot_dir, SnapshotIdentity, SnapshotOutput};
use rand::rngs::StdRng;
use rand::SeedableRng;

use crate::materialize::{BlobSource, InlineMaterializer, MaterializeError, Materializer};
use crate::proto::{self, ItemPayload, ProtoError};
use crate::state::{device_id_from_tag, AgreedRecord, AgreementStore};
use crate::transport::{Connection, Transport};
use ferry_store::store::Store;

/// chunk id -> owning pack name + cached pack bytes.
type PackMap = HashMap<BlobId, (PackId, Arc<Vec<u8>>)>;

/// Default poll cadence from the ticket ("sleep 200ms").
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(200);
/// Idle dials happen every Nth poll tick, bounding reverse-direction lag.
pub const DEFAULT_OPPORTUNISTIC_EVERY: u32 = 5;

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub tag: String,
    /// Directory containing `.ferry/` (created if missing).
    pub store_dir: PathBuf,
    /// The synced working tree (created if missing).
    pub tree_dir: PathBuf,
    /// Folder chunker polynomial — MUST match the peer's or CDC boundaries
    /// diverge. Generate once with `ferry-sync genpoly`.
    pub poly: u64,
    pub folder_id: [u8; 16],
    pub poll_interval: Duration,
    pub opportunistic_every: u32,
    /// Listen role: bind this address (`:0` allowed). None = no listener.
    pub bind_addr: Option<SocketAddr>,
    /// Connect role: dial this peer address. Exactly one of bind/connect
    /// should be set per daemon; the connector drives sessions.
    pub connect_to: Option<SocketAddr>,
    /// Silence stdout status lines (tests).
    pub quiet: bool,
}

impl EngineConfig {
    /// Sensible test defaults: fixed folder id, fast polling.
    pub fn default_for_test(poly_seed: u64) -> Self {
        EngineConfig {
            tag: "test-node".into(),
            store_dir: PathBuf::from(".ferry-sync-test-store"),
            tree_dir: PathBuf::from(".ferry-sync-test-tree"),
            poly: ferry_store::chunker::generate_polynomial(&mut StdRng::seed_from_u64(poly_seed)),
            folder_id: crate::DEFAULT_FOLDER_ID,
            poll_interval: Duration::from_millis(50),
            opportunistic_every: 3,
            bind_addr: None,
            connect_to: None,
            quiet: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EngineStats {
    pub sessions_ok: u64,
    pub sessions_failed: u64,
    /// Items refused by verification (hash/name mismatch on receipt).
    pub rejected_items: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("store: {0}")]
    Store(#[from] ferry_store::store::StoreError),
    #[error("{0}")]
    Other(String),
}

/// Receiver-side verification failure. Distinct type because tests assert on
/// it directly and because the engine counts it separately from IO trouble.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("pack name mismatch: claimed {claimed}, BLAKE3(ciphertext) says {found}")]
    NameMismatch { claimed: String, found: String },
    #[error("blob {id} failed verify-after-receipt: content hashes to {found}")]
    BlobHashMismatch { id: String, found: String },
    #[error("store rejected ingested blob: {0}")]
    Store(#[from] ferry_store::store::StoreError),
    #[error("io while ingesting: {0}")]
    Io(#[from] std::io::Error),
    #[error("pack: {0}")]
    Pack(#[from] ferry_store::pack::PackError),
    #[error("index rebuild found damaged packs: {0:?}")]
    RebuildSkipped(Vec<String>),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("proto: {0}")]
    Proto(#[from] ProtoError),
    #[error("ingest: {0}")]
    Ingest(#[from] IngestError),
    #[error("apply failed: {0}")]
    Apply(String),
    #[error("store: {0}")]
    Store(#[from] ferry_store::store::StoreError),
    #[error("manifest: {0}")]
    Manifest(#[from] ferry_store::manifest::ManifestError),
    #[error("snapshot failed: {0}")]
    Snapshot(#[from] ferry_store::snapshot::SnapshotError),
    #[error("diff failed: {0}")]
    Diff(#[from] ferry_store::diff::DiffError),
    #[error("agreement state: {0}")]
    State(#[from] crate::state::StateError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

struct SnapshotData {
    manifest_bytes: Vec<u8>,
    manifest: RootManifest,
    manifest_id: BlobId,
}

/// What the local engine knows when a session starts.
struct MyOffer {
    snap: Arc<SnapshotData>,
    agreed_id: BlobId,
    agreed_root: Option<BlobId>,
}
struct SharedState {
    shutdown: AtomicBool,
    stats: Mutex<EngineStats>,
    agreed: Mutex<Option<BlobId>>,
    root: Mutex<Option<BlobId>>,
}

impl SharedState {
    fn bump(&self, f: impl FnOnce(&mut EngineStats)) {
        f(&mut self.stats.lock().unwrap());
    }

    fn stats(&self) -> EngineStats {
        *self.stats.lock().unwrap()
    }
}

/// Who transfers to whom. Returned by [`pick_donor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Donor {
    /// The FIRST argument's owner sends.
    First,
    /// The SECOND argument's owner sends.
    Second,
}

/// Lineage tiebreak only: newer creation timestamp wins; device id and root
/// break ties for a total order both peers compute identically.
fn lineage_winner(a: &RootManifest, b: &RootManifest) -> Donor {
    let ka = (a.created_sec, a.created_nsec, a.device_id, a.root_tree_id);
    let kb = (b.created_sec, b.created_nsec, b.device_id, b.root_tree_id);
    if ka >= kb {
        Donor::First
    } else {
        Donor::Second
    }
}

/// What one OFFER says about its sender's sync state.
#[derive(Debug, Clone, Copy)]
pub struct PeerState {
    /// Root tree of the offered (current) snapshot.
    pub current_root: BlobId,
    /// Root tree of the sender's last-agreed manifest, if any.
    pub baseline_root: Option<BlobId>,
}

/// Did this peer change anything since the last agreement? A fresh device
/// (no baseline) counts as changed iff its tree is non-empty — an empty new
/// device has nothing to contribute and must never win the bootstrap race.
fn has_diverged(p: PeerState) -> bool {
    match p.baseline_root {
        None => p.current_root != crate::empty_tree_id(),
        Some(base) => base != p.current_root,
    }
}

/// Clock-free donor selection. Both peers compute this from the same
/// inputs, so they always agree on direction.
///
/// Rule 1: exactly one side diverged from its baseline — that side sends.
/// This is what makes steady-state single-sided edits ALWAYS flow the right
/// way, no matter whose poll tick happened to fire last.
///
/// Rule 2: anything else (simultaneous edits — explicitly OUT of M0 scope,
/// or fresh-device bootstrap corner cases) falls back to [`pick_donor`]'s
/// manifest-only rules: non-empty beats empty on bootstrap, then lineage
/// last-writer-wins. The simultaneous-edit loser's changes are LOST until
/// T-010 ships quarantine.
pub fn select_donor(
    mine: PeerState,
    theirs: PeerState,
    my_manifest: &RootManifest,
    their_manifest: &RootManifest,
) -> Donor {
    debug_assert_ne!(
        mine.current_root, theirs.current_root,
        "caller checked roots"
    );
    match (has_diverged(mine), has_diverged(theirs)) {
        (true, false) => Donor::First,
        (false, true) => Donor::Second,
        _ => pick_donor(my_manifest, their_manifest),
    }
}

/// Deterministic donor choice from two manifests with UNEQUAL roots.
/// Identical inputs on both peers produce identical choices, which is what
/// keeps M0 livelock-free.
///
/// Rule 1 (bootstrap safety): empty tree vs non-empty tree — the NON-empty
/// side sends. Without this, a fresh device's empty snapshot could win the
/// timestamp race and wipe the populated peer. Known cost: deleting EVERY
/// file no longer propagates until T-010's three-way reconciliation exists;
/// documented M0 limitation.
///
/// Rule 2 (everything else): last-writer-wins by lineage. Simultaneous edits
/// lose the older writer's changes — explicitly out of M0 scope; T-010 owns
/// conflict quarantine.
pub fn pick_donor(a: &RootManifest, b: &RootManifest) -> Donor {
    debug_assert_ne!(a.root_tree_id, b.root_tree_id, "caller checked roots");
    let empty = crate::empty_tree_id();
    match (a.root_tree_id == empty, b.root_tree_id == empty) {
        (false, true) => return Donor::First,
        (true, false) => return Donor::Second,
        _ => {}
    }
    lineage_winner(a, b)
}

struct Ctx {
    cfg: EngineConfig,
    store: Arc<Store>,
    transport: Arc<dyn Transport>,
    agreements: AgreementStore,
    session_lock: Mutex<()>,
    latest: Mutex<Option<Arc<SnapshotData>>>,
    baseline: Mutex<Option<RootManifest>>,
    last_own_manifest_id: Mutex<BlobId>,
    shared: Arc<SharedState>,
}

impl Ctx {
    fn status(&self, line: &str) {
        if !self.cfg.quiet {
            println!("[{}] {}", self.cfg.tag, line);
        }
    }

    fn bump_ok(&self) {
        self.shared.bump(|s| s.sessions_ok += 1);
    }

    fn bump_failed(&self) {
        self.shared.bump(|s| s.sessions_failed += 1);
    }

    fn bump_rejected(&self) {
        self.shared.bump(|s| s.rejected_items += 1);
    }

    fn my_offer(&self) -> Result<MyOffer, SessionError> {
        // Sessions may arrive before the very first poll tick; wait briefly.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(snap) = self.latest.lock().unwrap().clone() {
                let baseline = self.baseline.lock().unwrap().clone();
                let (agreed_id, agreed_root) = match &baseline {
                    Some(m) => (
                        *blake3::hash(&serialize_manifest(m)).as_bytes(),
                        Some(m.root_tree_id),
                    ),
                    None => ([0u8; 32], None),
                };
                return Ok(MyOffer {
                    snap,
                    agreed_id,
                    agreed_root,
                });
            }
            if Instant::now() > deadline {
                return Err(SessionError::Other("no local snapshot available".into()));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn record_agreement(
        &self,
        peer_tag: &str,
        manifest_bytes: &[u8],
        manifest_id: BlobId,
    ) -> Result<(), SessionError> {
        let (sec, nsec) = now_parts();
        let rec = AgreedRecord {
            peer_device_id: device_id_from_tag(peer_tag),
            manifest_id,
            agreed_sec: sec,
            agreed_nsec: nsec,
            flags: 0,
        };
        self.agreements.record(peer_tag, rec, manifest_bytes)?;
        *self.baseline.lock().unwrap() = Some(parse_manifest(manifest_bytes)?);
        *self.shared.agreed.lock().unwrap() = Some(manifest_id);
        self.status(&format!(
            "STATE agreed={} recorded vs {peer_tag}",
            hex(&manifest_id)
        ));
        Ok(())
    }

    /// One poll iteration: resnapshot, publish state, maybe dial.
    fn tick(&self, n: u64) -> Result<(), SessionError> {
        let parent = *self.last_own_manifest_id.lock().unwrap();
        let (sec, nsec) = now_parts();
        let identity = SnapshotIdentity {
            folder_id: self.cfg.folder_id,
            device_id: device_id_from_tag(&self.cfg.tag),
            parent_manifest_id: parent,
            created_sec: sec,
            created_nsec: nsec,
        };
        let out: SnapshotOutput =
            snapshot_dir(&self.store, self.cfg.poly, &self.cfg.tree_dir, &identity)?;
        let manifest_bytes = serialize_manifest(&out.manifest);
        let data = Arc::new(SnapshotData {
            manifest_id: out.manifest_id,
            manifest: out.manifest.clone(),
            manifest_bytes,
        });

        *self.latest.lock().unwrap() = Some(data.clone());
        *self.last_own_manifest_id.lock().unwrap() = out.manifest_id;
        *self.shared.root.lock().unwrap() = Some(out.root_tree_id);

        let base_root = self
            .baseline
            .lock()
            .unwrap()
            .as_ref()
            .map(|m| m.root_tree_id);
        self.status(&format!(
            "STATE root={} agreed={}",
            hex(&out.root_tree_id),
            self.shared
                .agreed
                .lock()
                .unwrap()
                .map(|i| hex(&i))
                .unwrap_or("none".into())
        ));

        // Connector drives sessions; listener relies on opportunistic dials
        // from the peer to discover ITS changes.
        if self.cfg.connect_to.is_some()
            && (base_root != Some(out.root_tree_id)
                || n.is_multiple_of(u64::from(self.cfg.opportunistic_every)))
        {
            // try_lock: never queue behind a serving session; next tick retries.
            if let Ok(_guard) = self.session_lock.try_lock() {
                self.dial_and_run();
            }
        }
        Ok(())
    }

    fn dial_and_run(&self) {
        let addr = match self.cfg.connect_to {
            Some(a) => a,
            None => return,
        };
        match self.transport.dial(addr) {
            Ok(mut conn) => match run_session(conn.as_mut(), self, true) {
                Ok(()) => self.bump_ok(),
                Err(e) => {
                    proto::send_error(conn.as_mut(), &format!("{e}"));
                    self.bump_failed();
                    self.status(&format!("SESSION failed (dial): {e}"));
                }
            },
            Err(e) => self.status(&format!("SESSION dial error: {e}")),
        }
    }
}

fn hex_short(b: &BlobId) -> String {
    hex(b)[..12].to_string()
}

fn now_parts() -> (i64, u32) {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_secs() as i64, d.subsec_nanos())
}

struct StoreSource<'a> {
    store: &'a Store,
}

impl BlobSource for StoreSource<'_> {
    fn get(&self, kind: BlobKind, id: &BlobId) -> Result<Vec<u8>, MaterializeError> {
        self.store.get(kind, id).map_err(|e| {
            MaterializeError::MissingBlob(format!("{} {}: {e}", kind_debug(kind), hex(id)))
        })
    }
}

fn kind_debug(k: BlobKind) -> &'static str {
    match k {
        BlobKind::DataChunk => "chunk",
        BlobKind::TreeNode => "tree",
        BlobKind::Manifest => "manifest",
        BlobKind::Polynomial => "poly",
    }
}

/// One sync conversation over an established connection. Caller holds the
/// per-daemon session lock. `dialer` speaks first (HELLO order only; every
/// subsequent decision is symmetric).
fn run_session(conn: &mut dyn Connection, ctx: &Ctx, dialer: bool) -> Result<(), SessionError> {
    let my = ctx.my_offer()?;

    let peer_tag = if dialer {
        proto::send_hello(conn, &ctx.cfg.tag)?;
        let h = proto::recv_hello(conn)?;
        ctx.status(&format!("SESSION opened with {}", h.device_tag));
        h.device_tag
    } else {
        let h = proto::recv_hello(conn)?;
        ctx.status(&format!("SESSION accepted from {}", h.device_tag));
        proto::send_hello(conn, &ctx.cfg.tag)?;
        h.device_tag
    };

    proto::send_offer(
        conn,
        &proto::Offer {
            manifest_bytes: my.snap.manifest_bytes.clone(),
            agreed_manifest_id: my.agreed_id,
            agreed_root_tree_id: my.agreed_root.unwrap_or([0; 32]),
        },
    )?;
    let theirs = proto::recv_offer(conn)?;
    let peer_manifest = parse_manifest(&theirs.manifest_bytes)?;
    let peer_manifest_id = *blake3::hash(&theirs.manifest_bytes).as_bytes();

    // Keep the offered manifest as a stored blob: agreement records may
    // reference it across restarts.
    ctx.store
        .put_meta(BlobKind::Manifest, &theirs.manifest_bytes)?;

    decide_and_transfer(
        conn,
        ctx,
        my,
        theirs,
        peer_manifest,
        peer_manifest_id,
        peer_tag,
    )
}

#[allow(clippy::too_many_arguments)]
fn decide_and_transfer(
    conn: &mut dyn Connection,
    ctx: &Ctx,
    my: MyOffer,
    theirs: proto::Offer,
    peer_manifest: RootManifest,
    peer_manifest_id: BlobId,
    peer_tag: String,
) -> Result<(), SessionError> {
    if my.snap.manifest.root_tree_id == peer_manifest.root_tree_id {
        // Same content already. If agreement pointers match, nothing to do;
        // otherwise settle on the lineage winner so records converge even
        // between fresh peers (both-empty bootstrap included).
        if my.agreed_id != [0; 32] && theirs.agreed_manifest_id == my.agreed_id {
            ctx.status("SESSION converged already");
            return Ok(());
        }
        let win = lineage_winner(&my.snap.manifest, &peer_manifest);
        let (winner_bytes, winner_id) = match win {
            Donor::First => (my.snap.manifest_bytes.clone(), my.snap.manifest_id),
            Donor::Second => (theirs.manifest_bytes.clone(), peer_manifest_id),
        };
        // Deterministic speakers: the First side receives the confirmation
        // it computes; the Second side records then sends it.
        match win {
            Donor::First => {
                let got = proto::recv_agreed(conn)?;
                if got != winner_id {
                    return Err(SessionError::Other(format!(
                        "agreement mismatch: expected {}, peer said {}",
                        hex(&winner_id),
                        hex(&got)
                    )));
                }
                ctx.record_agreement(&peer_tag, &winner_bytes, winner_id)?;
            }
            Donor::Second => {
                ctx.record_agreement(&peer_tag, &winner_bytes, winner_id)?;
                proto::send_agreed(conn, winner_id)?;
            }
        }
        ctx.status("SESSION settled agreement without transfer");
        return Ok(());
    }

    let their_baseline = if theirs.agreed_manifest_id == [0u8; 32] {
        None
    } else {
        Some(theirs.agreed_root_tree_id)
    };
    let win = select_donor(
        PeerState {
            current_root: my.snap.manifest.root_tree_id,
            baseline_root: my.agreed_root,
        },
        PeerState {
            current_root: peer_manifest.root_tree_id,
            baseline_root: their_baseline,
        },
        &my.snap.manifest,
        &peer_manifest,
    );

    match win {
        Donor::First => serve_as_donor(conn, ctx, &my.snap, &peer_tag)?,
        Donor::Second => run_as_puller(
            conn,
            ctx,
            my.snap,
            theirs,
            peer_manifest,
            peer_manifest_id,
            peer_tag,
        )?,
    }
    Ok(())
}

/// Donor side: answer requests until AGREED arrives.
fn serve_as_donor(
    conn: &mut dyn Connection,
    ctx: &Ctx,
    my: &Arc<SnapshotData>,
    peer_tag: &str,
) -> Result<(), SessionError> {
    let mut pack_cache: Option<PackMap> = None;
    loop {
        let (t, body) = proto::recv_msg(conn)?;
        match t {
            proto::tag::REQ_META => {
                for (kind, id) in proto::decode_req_meta(&body)? {
                    let bytes = ctx.store.get(kind, &id)?;
                    proto::send_item(conn, &ItemPayload::Blob { kind, id, bytes })?;
                }
                proto::send_items_done(conn)?;
            }
            proto::tag::REQ_DATA => {
                let ids = proto::decode_req_data(&body)?;
                if pack_cache.is_none() {
                    pack_cache = Some(build_pack_map(ctx)?);
                }
                serve_data_request(conn, ctx, pack_cache.as_ref().unwrap(), &ids)?;
            }
            proto::tag::AGREED => {
                let id = proto::decode_agreed(&body)?;
                if id != my.manifest_id {
                    return Err(SessionError::Other(format!(
                        "peer agreed on {} but I offered {}",
                        hex(&id),
                        hex(&my.manifest_id)
                    )));
                }
                ctx.record_agreement(peer_tag, &my.manifest_bytes, my.manifest_id)?;
                ctx.status(&format!(
                    "SESSION complete: peer agreed on {}",
                    hex_short(&id)
                ));
                return Ok(());
            }
            other => return Err(ProtoError::BadTag(other).into()),
        }
    }
}

fn serve_data_request(
    conn: &mut dyn Connection,
    ctx: &Ctx,
    map: &PackMap,
    chunk_ids: &[BlobId],
) -> Result<(), SessionError> {
    let mut sent_packs: HashSet<PackId> = HashSet::new();
    for id in chunk_ids {
        match map.get(id) {
            Some((name, bytes)) => {
                if sent_packs.insert(*name) {
                    proto::send_item(
                        conn,
                        &ItemPayload::Pack {
                            name: *name,
                            bytes: (**bytes).clone(),
                        },
                    )?;
                }
                // Chunk rides a pack already sent this round: nothing to do.
            }
            None => {
                // Unmapped fallback (should not happen post-seal, but stay
                // correct): serve the blob individually.
                let bytes = ctx.store.get(BlobKind::DataChunk, id)?;
                proto::send_item(
                    conn,
                    &ItemPayload::Blob {
                        kind: BlobKind::DataChunk,
                        id: *id,
                        bytes,
                    },
                )?;
            }
        }
    }
    proto::send_items_done(conn)?;
    Ok(())
}

/// Verify a pack's name against its ciphertext, then write it into the
/// store's packs directory atomically. The single ingest path shared by
/// sessions and tests.
fn ingest_pack_verified(
    store: &Store,
    claimed_name: &PackId,
    bytes: &[u8],
) -> Result<(), IngestError> {
    let found = ferry_store::pack::pack_name_of(bytes);
    if found != *claimed_name {
        return Err(IngestError::NameMismatch {
            claimed: hex(claimed_name),
            found: hex(&found),
        });
    }
    let dot = store.store_dir();
    ferry_store::pack::write_pack_atomically(&dot.join("tmp"), &dot.join("packs"), bytes)?;
    Ok(())
}

/// Scan the on-disk packs and map every DATA CHUNK id to its owning pack
/// (with the pack's bytes cached for streaming). Packs failing their own
/// name verification are skipped loudly; a requested chunk with no healthy
/// home falls back to individual-blob transfer downstream.
fn build_pack_map(ctx: &Ctx) -> Result<PackMap, SessionError> {
    let packs_dir = ctx.store.store_dir().join("packs");
    let mut out = HashMap::new();
    for entry in std::fs::read_dir(&packs_dir)?.flatten() {
        let path = entry.path();
        let name_str = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let claimed: PackId = match ferry_store::format::unhex(&name_str) {
            Some(v) => v,
            None => continue,
        };
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        if ferry_store::pack::pack_name_of(&bytes) != claimed {
            ctx.status(&format!("WARN skipping damaged pack {}", name_str));
            continue;
        }
        let (_, entries) = match ferry_store::pack::read_footer(
            &bytes,
            &claimed,
            &[0u8; ferry_store::crypto::KEY_LEN],
            &PassthroughCipher,
        ) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let shared = Arc::new(bytes);
        for e in entries {
            if e.kind == BlobKind::DataChunk {
                out.insert(e.id, (claimed, Arc::clone(&shared)));
            }
        }
    }
    Ok(out)
}

/// Puller side: hydrate the peer's tree nodes, diff, request data by chunk
/// id, ingest, materialize durably, confirm.
fn run_as_puller(
    conn: &mut dyn Connection,
    ctx: &Ctx,
    my: Arc<SnapshotData>,
    theirs: proto::Offer,
    peer_manifest: RootManifest,
    peer_manifest_id: BlobId,
    peer_tag: String,
) -> Result<(), SessionError> {
    fetch_meta_tree(conn, ctx, &peer_manifest.root_tree_id)?;

    let changes = ferry_store::diff::diff_manifests(&ctx.store, &my.manifest, &peer_manifest)?;
    let wanted = collect_chunk_ids(&changes);
    ctx.status(&format!(
        "SESSION pulling: {} added / {} removed / {} modified / {} metadata, {} chunks wanted",
        changes.added.len(),
        changes.removed.len(),
        changes.content_modified.len() + changes.type_changed.len(),
        changes.metadata_modified.len(),
        wanted.len()
    ));

    proto::send_req_data(conn, &wanted)?;
    let mut received_packs = false;
    loop {
        match proto::recv_item_stream(conn)? {
            proto::ItemStream::Done => break,
            proto::ItemStream::Item(item) => {
                if ingest_item(ctx, &item)? {
                    received_packs = true;
                }
            }
        }
    }

    if received_packs {
        // Seal staged meta blobs, then teach the location table about the
        // freshly landed packs. M0 shortcut: full rebuild per delivery;
        // T-002/T-008 replace this with incremental index appends.
        ctx.store.flush()?;
        let (_, skipped) = ctx.store.rebuild_index()?;
        if !skipped.is_empty() {
            return Err(IngestError::RebuildSkipped(skipped).into());
        }
    }

    InlineMaterializer::new(&ctx.cfg.tree_dir)
        .apply(&peer_manifest, &changes, &StoreSource { store: &ctx.store })
        .map_err(|e| SessionError::Apply(format!("{e}")))?;

    ctx.record_agreement(&peer_tag, &theirs.manifest_bytes, peer_manifest_id)?;
    proto::send_agreed(conn, peer_manifest_id)?;
    ctx.status(&format!(
        "SESSION complete: agreed on {}",
        hex_short(&peer_manifest_id)
    ));
    Ok(())
}

/// Walk the offered tree level by level, requesting missing tree nodes as
/// individual meta blobs. Presence probing goes through the store (staging
/// included), so repeat sessions only fetch what is genuinely absent.
fn fetch_meta_tree(
    conn: &mut dyn Connection,
    ctx: &Ctx,
    root: &BlobId,
) -> Result<(), SessionError> {
    let mut frontier: Vec<BlobId> = vec![*root];
    let mut fetched = 0usize;
    while !frontier.is_empty() {
        let missing: Vec<BlobId> = frontier
            .iter()
            .filter(|id| ctx.store.get(BlobKind::TreeNode, id).is_err())
            .copied()
            .collect();
        if !missing.is_empty() {
            let req: Vec<(BlobKind, BlobId)> =
                missing.iter().map(|id| (BlobKind::TreeNode, *id)).collect();
            proto::send_req_meta(conn, &req)?;
            let mut got: HashSet<BlobId> = HashSet::new();
            while got.len() < missing.len() {
                match proto::recv_item_stream(conn)? {
                    proto::ItemStream::Done => {
                        return Err(SessionError::Other("tree-node stream ended early".into()))
                    }
                    proto::ItemStream::Item(ItemPayload::Blob {
                        kind: BlobKind::TreeNode,
                        id,
                        bytes,
                    }) => {
                        if !missing.contains(&id) || got.contains(&id) {
                            return Err(SessionError::Other(format!(
                                "unexpected tree node {} in response",
                                hex(&id)
                            )));
                        }
                        let found = *blake3::hash(&bytes).as_bytes();
                        if found != id {
                            ctx.bump_rejected();
                            return Err(IngestError::BlobHashMismatch {
                                id: hex(&id),
                                found: hex(&found),
                            }
                            .into());
                        }
                        ctx.store.put_meta(BlobKind::TreeNode, &bytes)?;
                        got.insert(id);
                        fetched += 1;
                    }
                    proto::ItemStream::Item(other) => {
                        return Err(SessionError::Other(format!(
                            "expected tree node item, got kind {:?}",
                            other
                        )))
                    }
                }
            }
            proto::recv_items_done(conn)?;
        }
        // Expand frontier: children of every node now present locally.
        let mut next = Vec::new();
        for id in &frontier {
            let bytes = ctx.store.get(BlobKind::TreeNode, id)?;
            let node = ferry_store::manifest::parse_tree_node(&bytes)?;
            for e in node.entries {
                if let ferry_store::manifest::EntryPayload::Dir { child_tree_id } = e.payload {
                    next.push(child_tree_id);
                }
            }
        }
        frontier = next;
    }
    if fetched > 0 {
        ctx.status(&format!("SESSION fetched {fetched} tree nodes"));
    }
    Ok(())
}

/// Verify-after-receipt, then persist. Returns true when a PACK landed on
/// disk (caller must refresh the index).
fn ingest_item(ctx: &Ctx, item: &ItemPayload) -> Result<bool, IngestError> {
    match item {
        ItemPayload::Pack { name, bytes } => match ingest_pack_verified(&ctx.store, name, bytes) {
            Ok(()) => Ok(true),
            Err(e @ IngestError::NameMismatch { .. }) => {
                ctx.bump_rejected();
                Err(e)
            }
            Err(e) => Err(e),
        },
        ItemPayload::Blob { kind, id, bytes } => {
            let found = *blake3::hash(bytes).as_bytes();
            if found != *id {
                ctx.bump_rejected();
                return Err(IngestError::BlobHashMismatch {
                    id: hex(id),
                    found: hex(&found),
                });
            }
            ctx.store.put_blob(*kind, bytes)?;
            Ok(false)
        }
    }
}

fn collect_chunk_ids(changes: &ChangeSet) -> Vec<BlobId> {
    let mut seen: HashSet<BlobId> = HashSet::new();
    let mut out = Vec::new();
    for state in changes
        .added
        .iter()
        .map(|a| &a.state)
        .chain(changes.content_modified.iter().map(|m| &m.after))
        .chain(changes.type_changed.iter().map(|m| &m.after))
    {
        for (id, _) in &state.chunks {
            if seen.insert(*id) {
                out.push(*id);
            }
        }
    }
    out
}

pub struct SyncEngine {
    cfg: EngineConfig,
    transport: Arc<dyn Transport>,
    store: Arc<Store>,
    listener: Option<Box<dyn crate::transport::Listener>>,
}

impl SyncEngine {
    /// Build (but do not start) an engine. Opens or creates the store,
    /// creates the tree dir, binds the listener when configured.
    pub fn new(cfg: EngineConfig, transport: Arc<dyn Transport>) -> Result<Self, EngineError> {
        std::fs::create_dir_all(&cfg.tree_dir)?;
        let store = Arc::new(open_or_create_store(&cfg.store_dir)?);
        let listener = match cfg.bind_addr {
            Some(addr) => Some(Transport::listen(transport.as_ref(), addr)?),
            None => None,
        };
        Ok(SyncEngine {
            cfg,
            transport,
            store,
            listener,
        })
    }

    /// Bound address (after `:0` resolution); None for pure connectors.
    pub fn listen_addr(&self) -> Option<SocketAddr> {
        self.listener.as_ref().and_then(|l| l.local_addr().ok())
    }

    /// Direct access to the receiver-side pack verification for unit tests:
    /// refuses bytes whose BLAKE3 differs from the claimed pack name BEFORE
    /// anything is written, exactly as the session path does.
    pub fn ingest_pack_bytes_for_test(
        cfg: &EngineConfig,
        claimed_name: &PackId,
        bytes: &[u8],
    ) -> Result<(), IngestError> {
        let store =
            open_or_create_store(&cfg.store_dir).map_err(|e| IngestError::Other(format!("{e}")))?;
        ingest_pack_verified(&store, claimed_name, bytes)
    }

    /// Spawn poll (+ accept) threads. Dropping the returned handle shuts
    /// everything down and joins.
    pub fn start(mut self) -> EngineHandle {
        let listener = self.listener.take();
        let listen_addr = listener.as_ref().and_then(|l| l.local_addr().ok());
        let shared = Arc::new(SharedState {
            shutdown: AtomicBool::new(false),
            stats: Mutex::new(EngineStats::default()),
            agreed: Mutex::new(None),
            root: Mutex::new(None),
        });
        let agreements = AgreementStore::new(self.store.store_dir());
        let ctx = Arc::new(Ctx {
            cfg: self.cfg.clone(),
            store: Arc::clone(&self.store),
            transport: Arc::clone(&self.transport),
            agreements,
            session_lock: Mutex::new(()),
            latest: Mutex::new(None),
            baseline: Mutex::new(None),
            last_own_manifest_id: Mutex::new([0u8; 32]),
            shared: Arc::clone(&shared),
        });

        let joins: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));

        if let Some(listener) = listener {
            let ctx2 = Arc::clone(&ctx);
            let shared2 = Arc::clone(&shared);
            let joins2 = Arc::clone(&joins);
            joins.lock().unwrap().push(
                std::thread::Builder::new()
                    .name(format!("{}-accept", self.cfg.tag))
                    .spawn(move || accept_loop(listener, ctx2, shared2, joins2))
                    .expect("spawn accept loop"),
            );
        }

        {
            let ctx = Arc::clone(&ctx);
            let shared = Arc::clone(&shared);
            joins.lock().unwrap().push(
                std::thread::Builder::new()
                    .name(format!("{}-poll", self.cfg.tag))
                    .spawn(move || poll_loop(ctx, shared))
                    .expect("spawn poll loop"),
            );
        }

        EngineHandle {
            shared,
            joins,
            listen_addr,
            transport: Arc::clone(&self.transport),
            tag: self.cfg.tag,
        }
    }
}

fn open_or_create_store(store_dir: &std::path::Path) -> Result<Store, EngineError> {
    // Store::create uses non-recursive mkdir for `.ferry`; make sure the
    // parent chain exists first.
    std::fs::create_dir_all(store_dir)?;
    if store_dir.join(ferry_store::store::STORE_DIR_NAME).is_dir() {
        Ok(Store::open(store_dir, FMK, Box::new(PassthroughCipher))?)
    } else {
        Ok(Store::create(store_dir, FMK, Box::new(PassthroughCipher))?)
    }
}

/// v0 folder master key: zeros under the pass-through cipher. T-007 replaces
/// this with real key material; nothing else changes.
const FMK: [u8; ferry_store::crypto::KEY_LEN] = [0u8; ferry_store::crypto::KEY_LEN];

fn accept_loop(
    listener: Box<dyn crate::transport::Listener>,
    ctx: Arc<Ctx>,
    shared: Arc<SharedState>,
    joins: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>,
) {
    while !shared.shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok(mut conn) => {
                if shared.shutdown.load(Ordering::SeqCst) {
                    break;
                }
                let ctx = Arc::clone(&ctx);
                let shared = Arc::clone(&shared);
                let h = std::thread::Builder::new()
                    .name(format!("{}-session", ctx.cfg.tag))
                    .spawn(move || {
                        // Serialize sessions; bail promptly on shutdown.
                        let _guard = ctx.session_lock.lock().unwrap();
                        if shared.shutdown.load(Ordering::SeqCst) {
                            return;
                        }
                        match run_session(conn.as_mut(), &ctx, false) {
                            Ok(()) => ctx.bump_ok(),
                            Err(e) => {
                                proto::send_error(conn.as_mut(), &format!("{e}"));
                                ctx.bump_failed();
                                ctx.status(&format!("SESSION failed (accept): {e}"));
                            }
                        }
                    })
                    .expect("spawn session handler");
                joins.lock().unwrap().push(h);
            }
            Err(e) => {
                if shared.shutdown.load(Ordering::SeqCst) {
                    break;
                }
                ctx.status(&format!("ACCEPT error: {e}"));
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn poll_loop(ctx: Arc<Ctx>, shared: Arc<SharedState>) {
    let mut n: u64 = 0;
    loop {
        if shared.shutdown.load(Ordering::SeqCst) {
            return;
        }
        n += 1;
        if let Err(e) = ctx.tick(n) {
            ctx.status(&format!("TICK error: {e}"));
        }
        std::thread::sleep(ctx.cfg.poll_interval);
    }
}

/// Clonable control handle: stats, agreed id, latest root, clean shutdown.
#[derive(Clone)]
pub struct EngineHandle {
    shared: Arc<SharedState>,
    joins: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>,
    listen_addr: Option<SocketAddr>,
    transport: Arc<dyn Transport>,
    tag: String,
}

impl EngineHandle {
    pub fn tag(&self) -> &str {
        &self.tag
    }

    pub fn agreed_id(&self) -> Option<BlobId> {
        *self.shared.agreed.lock().unwrap()
    }

    pub fn root_id(&self) -> Option<BlobId> {
        *self.shared.root.lock().unwrap()
    }

    pub fn stats(&self) -> EngineStats {
        self.shared.stats()
    }

    pub fn listen_addr(&self) -> Option<SocketAddr> {
        self.listen_addr
    }

    /// Signal shutdown and wait for loops to exit. Idempotent.
    pub fn shutdown(&self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        // Unblock a possibly-blocked accept() with a throwaway connection.
        if let Some(addr) = self.listen_addr {
            let _ = self.transport.dial(addr);
        }
        while let Some(j) = self.joins.lock().unwrap().pop() {
            let _ = j.join();
        }
    }

    /// Block the calling thread while the engine runs. The daemon binary
    /// parks here; actual termination is a process signal (std has no
    /// handler story), after which Drop runs the same shutdown path.
    pub fn join_until_signal(&self) {
        loop {
            if self.shared.shutdown.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferry_store::manifest::{
        dir_entry, file_entry, serialize_manifest, RootManifest, TreeNode,
    };

    fn manifest_at(sec: i64, dev: [u8; 32], root: BlobId) -> RootManifest {
        RootManifest {
            folder_id: [1; 16],
            device_id: dev,
            created_sec: sec,
            created_nsec: 5,
            root_tree_id: root,
            parent_manifest_id: [0; 32],
        }
    }

    fn tree(entries: Vec<ferry_store::manifest::TreeEntry>) -> BlobId {
        let bytes = ferry_store::manifest::serialize_tree_node(&TreeNode { entries });
        *blake3::hash(&bytes).as_bytes()
    }

    #[test]
    fn donor_selection_prefers_nonempty_on_bootstrap() {
        let empty = crate::empty_tree_id();
        let full = tree(vec![file_entry("x", false, 0, 0, vec![])]);
        let fresh = manifest_at(999, [9; 32], empty); // NEWER timestamp...
        let populated = manifest_at(1, [1; 32], full); // ...but EMPTY tree
        assert_eq!(
            pick_donor(&fresh, &populated),
            Donor::Second,
            "empty-vs-nonempty ignores the clock"
        );
        assert_eq!(pick_donor(&populated, &fresh), Donor::First);
    }

    #[test]
    fn donor_selection_last_writer_wins_between_nonempty_trees() {
        let t1 = tree(vec![file_entry("a", false, 0, 0, vec![])]);
        let t2 = tree(vec![dir_entry("d", 0, 0, t1)]);
        let older = manifest_at(100, [7; 32], t1);
        let newer = manifest_at(200, [7; 32], t2);
        assert_eq!(pick_donor(&older, &newer), Donor::Second);
        assert_eq!(pick_donor(&newer, &older), Donor::First);

        // Timestamp tie: higher device id wins deterministically.
        let same_a = manifest_at(300, [1; 32], t1);
        let same_b = manifest_at(300, [2; 32], t2);
        assert_eq!(pick_donor(&same_a, &same_b), Donor::Second);
        assert_eq!(pick_donor(&same_b, &same_a), Donor::First);
    }

    #[test]
    fn chunk_collection_dedups_across_buckets() {
        let st = |chunks: Vec<(BlobId, u64)>| ferry_store::diff::EntryState {
            kind: ferry_store::diff::EntryKind::File,
            exec: false,
            mtime_sec: 0,
            mtime_nsec: 0,
            chunks,
            target: None,
        };
        let c1 = [1u8; 32];
        let c2 = [2u8; 32];
        let cs = ChangeSet {
            added: vec![ferry_store::diff::Added {
                path: vec!["a".into()],
                state: st(vec![(c1, 1), (c2, 2)]),
            }],
            content_modified: vec![ferry_store::diff::Modified {
                path: vec!["b".into()],
                before: st(vec![(c1, 1)]),
                after: st(vec![(c2, 9)]),
            }],
            ..Default::default()
        };
        assert_eq!(collect_chunk_ids(&cs), vec![c1, c2]);
    }

    #[test]
    fn manifest_serialization_stays_the_wire_format() {
        let m = manifest_at(42, [3; 32], [4; 32]);
        let bytes = serialize_manifest(&m);
        assert_eq!(parse_manifest(&bytes).unwrap(), m);
        assert_eq!(
            *blake3::hash(&bytes).as_bytes(),
            *blake3::hash(&serialize_manifest(&m)).as_bytes()
        );
    }
}
