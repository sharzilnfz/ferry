//! The skeleton engine: poll → snapshot → compare → exchange → materialize
//! → record agreement.
//!
//! Sessions speak protocol v1 by default ([`crate::exchange`]): offers with
//! adverts, role-serialized pull stages verified end to end, a second
//! offer round that observes equality, local last-agreed records in the
//! canonical ledger format, BYE. The puller materializes durably BEFORE
//! round 2, mirroring M0's "materialize, THEN confirm" order. The retired
//! plaintext session shape stays available behind
//! [`EngineConfig::legacy_m0_proto`] for wire-level debugging.
//!
//! State model under v1: each daemon holds a CURRENT manifest pointer that
//! either names its own latest snapshot or an ADOPTED peer manifest. A
//! poll tick only mints a new manifest when the scanned tree's root differs
//! from the current pointer's root — adopt-and-hold keeps both sides'
//! announced ids stable and comparable across sessions, which is what lets
//! lineage (creation time, then device id) decide adoption without clocks
//! racing.
//!
//! Concurrency: at most one session runs per daemon (a mutex serializes
//! the dialer and every accepted handler). Only the connect-role daemon
//! dials; the listen-role daemon serves and relies on the peer's
//! opportunistic backstop dials (default every 50 ticks ≈ 10 s) to discover
//! its changes. Simultaneous edits
//! resolve by lineage last-writer-wins and may LOSE the loser's changes —
//! explicit M0 scope, T-010 owns conflicts.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ferry_crypto::identity::DeviceIdentity;
use ferry_store::crypto::PassthroughCipher;
use ferry_store::diff::ChangeSet;
use ferry_store::format::{hex, BlobId, BlobKind, PackId};
use ferry_store::manifest::{parse_manifest, serialize_manifest, RootManifest};
use ferry_store::snapshot::{
    snapshot_dir, snapshot_dir_incremental, SnapshotError, SnapshotIdentity, SnapshotOutput,
};
use rand::rngs::StdRng;
use rand::SeedableRng;

use crate::exchange::{self, CurrentState, ExchangeHost};
use crate::session::{self, ConnLink, Established, ExpectPeer};
use crate::transport::{Connection, Transport};
use ferry_store::agreement::{AgreedRecord, AgreementLedger};
pub use ferry_store::snapshot::ScanStats;
use ferry_store::store::Store;

/// Default poll cadence from the ticket ("sleep 200ms").
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(200);
/// Idle-backstop dials happen every Nth poll tick. While the folder is
/// settled (scan root == baseline root), divergence dialing stays silent and
/// only this backstop fires — it is also how the connect-role daemon discovers
/// listen-role peer changes, so it must stay live.
pub const DEFAULT_OPPORTUNISTIC_EVERY: u32 = 50;

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub tag: String,
    /// Directory containing `.ferry/` (created if missing).
    pub store_dir: PathBuf,
    /// The synced working tree (created if missing).
    pub tree_dir: PathBuf,
    /// Folder chunker polynomial — MUST match the peer's or CDC boundaries
    /// diverge. Generate once with `ferry-sync genpoly`. Validated at
    /// config-load; an invalid value is a config error, never a mid-scan panic.
    pub poly: ferry_store::chunker::ValidatedPoly,
    pub folder_id: [u8; 16],
    pub poll_interval: Duration,
    pub opportunistic_every: u32,
    /// Listen role: bind this address (`:0` allowed). None = no listener.
    pub bind_addr: Option<SocketAddr>,
    /// Connect role: dial this peer address. Exactly one of bind/connect
    /// should be set per daemon; the connector drives sessions.
    pub connect_to: Option<SocketAddr>,
    /// Strictly expect this peer device id at the handshake (ADR-0003).
    /// If set to `Some(pin)`, the engine strictly enforces that single peer identity.
    /// `None` indicates default policy: if a `CONFIG_HEAD` exists in the folder,
    /// an allow-list is seeded from its wrapped keys (deny-unknown by default);
    /// otherwise Trust-On-First-Use (TOFU) persists the first authenticated peer
    /// identity per folder under `.ferry/peers/` and refuses any subsequent mismatches.
    pub expected_peer_id: Option<BlobId>,
    /// The folder's `.ferry` directory whose pin-state.json gates tree
    /// mutation at the shared execution boundary (T-06 session pinning).
    /// `None` (the default) is the no-pin policy: materialization never
    /// consults pin state.
    pub pin_state_dir: Option<PathBuf>,
    /// Silence stdout status lines (tests).
    pub quiet: bool,
}

/// Local peer authorization policy (T-18).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PeerPolicy {
    /// Trust on first use (TOFU): accepts the first peer that proves key
    /// possession, persists its identity per-folder to disk under `.ferry/peers/`,
    /// and strictly enforces that pinned identity on subsequent sessions
    /// (refusing any mismatches loudly).
    #[default]
    TrustOnFirstUse,
    /// Explicit allow-list: accepts only peers whose device ID is in the set.
    /// Denies unknown peers by default. Does not perform TOFU.
    AllowList(HashSet<BlobId>),
}

impl PeerPolicy {
    /// Construct an allow-list policy from an iterator of allowed peer device IDs.
    pub fn from_allowed<I: IntoIterator<Item = BlobId>>(peers: I) -> Self {
        PeerPolicy::AllowList(peers.into_iter().collect())
    }

    /// Construct an allow-list policy seeded from `CONFIG_HEAD` container bytes.
    /// Extracts every `device_pub` from the wrapped key entries.
    pub fn from_config_head(
        bytes: &[u8],
    ) -> Result<Self, ferry_crypto::config_head::ConfigHeadError> {
        let ch = ferry_crypto::config_head::parse_config_head(bytes)?;
        let set: HashSet<BlobId> = ch.entries.into_iter().map(|e| e.device_pub).collect();
        Ok(PeerPolicy::AllowList(set))
    }
}

/// On-disk ledger for persisted TOFU peer identities (T-18).
/// Records live under `<store_dir>/peers/` named `<folder_hex>-<peer_hex>.peer`.
#[derive(Clone, Debug)]
pub struct PeerLedger {
    dir: PathBuf,
}

impl PeerLedger {
    /// `store_dir` is the folder's `.ferry` directory (or stand-in in tests).
    pub fn new(store_dir: impl Into<PathBuf>) -> Self {
        PeerLedger {
            dir: store_dir.into().join("peers"),
        }
    }

    pub fn path_for(&self, folder_id: &[u8; 16], peer: &[u8; 32]) -> PathBuf {
        self.dir
            .join(format!("{}-{}.peer", hex(folder_id), hex(peer)))
    }

    /// Persist a first-seen peer identity atomically.
    pub fn record_peer(&self, folder_id: &[u8; 16], peer: &[u8; 32]) -> Result<(), std::io::Error> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self
            .dir
            .join(format!(".tmp-{}-{}", hex(folder_id), hex(peer)));
        std::fs::write(&tmp, hex(peer).as_bytes())?;
        std::fs::rename(&tmp, self.path_for(folder_id, peer))?;
        Ok(())
    }

    /// List all persisted peers for `folder_id`, sorted for determinism.
    pub fn list_peers(&self, folder_id: &[u8; 16]) -> Result<Vec<BlobId>, std::io::Error> {
        let prefix = format!("{}-", hex(folder_id));
        let rd = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        for entry in rd.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with('.')
                || !name_str.starts_with(&prefix)
                || !name_str.ends_with(".peer")
            {
                continue;
            }
            let peer_hex = name_str
                .trim_start_matches(&prefix)
                .trim_end_matches(".peer");
            if let Some(peer_id) = ferry_store::format::unhex::<32>(peer_hex) {
                out.push(peer_id);
            }
        }
        out.sort_unstable();
        Ok(out)
    }

    /// Forget a folder's persisted peer identity. Returns true if removed.
    pub fn forget_peer(
        &self,
        folder_id: &[u8; 16],
        peer: &[u8; 32],
    ) -> Result<bool, std::io::Error> {
        match std::fs::remove_file(self.path_for(folder_id, peer)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }
}

/// Check candidate paths for a `CONFIG_HEAD` file to seed allow-list mode.
fn resolve_peer_policy_from_disk(cfg: &EngineConfig, store: &Store) -> PeerPolicy {
    let candidates = [
        store.store_dir().join("config"),
        cfg.store_dir.join("config"),
        cfg.store_dir.join(".ferry").join("config"),
    ];
    for path in &candidates {
        if path.is_file() {
            if let Ok(bytes) = std::fs::read(path) {
                if let Ok(policy) = PeerPolicy::from_config_head(&bytes) {
                    if let PeerPolicy::AllowList(ref set) = policy {
                        if !set.is_empty() {
                            return policy;
                        }
                    }
                }
            }
        }
    }
    PeerPolicy::TrustOnFirstUse
}

impl EngineConfig {
    /// Sensible test defaults: fixed folder id, fast polling, protocol v1.
    pub fn default_for_test(poly_seed: u64) -> Self {
        EngineConfig {
            tag: "test-node".into(),
            store_dir: PathBuf::from(".ferry-sync-test-store"),
            tree_dir: PathBuf::from(".ferry-sync-test-tree"),
            poly: ferry_store::chunker::ValidatedPoly::generate(&mut StdRng::seed_from_u64(
                poly_seed,
            )),
            folder_id: crate::DEFAULT_FOLDER_ID,
            poll_interval: Duration::from_millis(50),
            opportunistic_every: 3,
            bind_addr: None,
            connect_to: None,
            expected_peer_id: None,
            pin_state_dir: None,
            quiet: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EngineStats {
    pub sessions_ok: u64,
    pub sessions_failed: u64,
    /// Transfers refused by verification: AEAD tag failures on sealed
    /// frames, blob hash mismatches after decrypt, and pack-name
    /// mismatches before insertion.
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
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// Protocol v1 wire failure (handshake, framing, seal/open, flow).
    #[error("v1 wire: {0}")]
    Wire(#[from] ferry_proto::error::ProtoError),
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
    Agreement(#[from] ferry_store::agreement::AgreementError),
    #[error("peer unauthorized: {0}")]
    PeerUnauthorized(String),
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

/// How long a session waits for the first poll tick to publish state.
const FIRST_STATE_WAIT: Duration = Duration::from_secs(10);

/// Max simultaneously ACCEPTED session threads. Beyond this, inbound
/// connections are dropped politely (logged, never queued) — a hostile or
/// accidental connection storm cannot exhaust threads or memory.
const MAX_CONCURRENT_SESSIONS: usize = 4;

/// The folder-pointer state machine (T-07): ONE owner for the current
/// pointer, the raw latest scan, the last-agreed baseline, the agreed id,
/// and the next self-mint parent — all behind one mutex so poll ticks and
/// session adoptions can no longer interleave writes across four locks
/// (spec B1). Every mutation is an explicit op below; readers wait on the
/// condvar instead of spin-sleeping.
///
/// Ordering rule that makes clobbering impossible by construction:
/// `publish_scan` re-checks under the lock whether the current pointer
/// changed since the scan STARTED (`ScanToken`). An adoption that landed
/// mid-scan wins; the stale pre-adoption scan is discarded whole rather
/// than published over the adopted lineage.
struct FolderState {
    inner: Mutex<FolderPointers>,
    changed: Condvar,
}

#[derive(Default)]
struct FolderPointers {
    /// Latest raw scan — refreshed every tick (legacy sessions read this).
    latest: Option<Arc<SnapshotData>>,
    /// CURRENT folder pointer: our latest snapshot OR an adopted peer manifest.
    current: Option<Arc<SnapshotData>>,
    /// Last-agreed manifest (divergence baseline).
    baseline: Option<RootManifest>,
    /// Manifest id of `baseline` (kept alongside it under the same lock).
    agreed: Option<BlobId>,
    /// Parent lineage for the next SELF-minted manifest.
    last_own_manifest_id: BlobId,
    /// Last-tick scan counts.
    scan_stats: Option<ScanStats>,
}

/// What a scan captured before it started, so publication can tell whether
/// the world moved underneath it.
#[derive(Clone, Copy)]
struct ScanToken {
    parent: BlobId,
    observed_current: Option<BlobId>,
}

/// What happened to a finished scan at publication time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublishOutcome {
    /// Real local change: current moved to the fresh scan.
    Minted,
    /// Scanned root equals the current pointer's root: adopt-and-hold.
    Held,
    /// An adoption landed mid-scan: the scan was discarded untouched.
    DiscardedStale,
}

impl FolderState {
    fn new() -> Self {
        FolderState {
            inner: Mutex::new(FolderPointers {
                last_own_manifest_id: [0u8; 32],
                ..FolderPointers::default()
            }),
            changed: Condvar::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FolderPointers> {
        self.inner.lock().unwrap()
    }

    /// Capture the pre-scan view a later [`Self::publish_scan`] validates
    /// against. Call BEFORE scanning; the token is cheap (`Copy`).
    fn scan_token(&self) -> ScanToken {
        let g = self.lock();
        ScanToken {
            parent: g.last_own_manifest_id,
            observed_current: g.current.as_ref().map(|s| s.manifest_id),
        }
    }

    /// Tick path: atomically validate + publish a finished scan. See the
    /// type-level docs for why check-and-write share one critical section.
    fn publish_scan(
        &self,
        tok: ScanToken,
        data: Arc<SnapshotData>,
        stats: ScanStats,
    ) -> PublishOutcome {
        let mut g = self.lock();
        g.scan_stats = Some(stats);
        if g.current.as_ref().map(|s| s.manifest_id) != tok.observed_current {
            // The current pointer moved while we scanned (an adoption): the
            // scan describes PRE-adoption tree state. Publishing it would
            // clobber the adopted lineage — discard whole; the next tick
            // resnapshots the post-apply tree.
            return PublishOutcome::DiscardedStale;
        }
        g.latest = Some(Arc::clone(&data));
        let held_same_root = g
            .current
            .as_ref()
            .is_some_and(|c| c.manifest.root_tree_id == data.manifest.root_tree_id);
        if !held_same_root {
            g.current = Some(Arc::clone(&data));
            g.last_own_manifest_id = data.manifest_id;
        }
        drop(g);
        self.changed.notify_all();
        if held_same_root {
            PublishOutcome::Held
        } else {
            PublishOutcome::Minted
        }
    }

    /// Session path: take a peer manifest as our current folder state
    /// (adoption precedes agreement). Also refreshes `latest` so legacy
    /// sessions see adopted bytes.
    fn adopt_peer(&self, data: Arc<SnapshotData>) {
        let id = data.manifest_id;
        let mut g = self.lock();
        g.latest = Some(Arc::clone(&data));
        g.current = Some(data);
        g.last_own_manifest_id = id;
        drop(g);
        self.changed.notify_all();
    }

    /// Record agreement: baseline and agreed id move together under the
    /// single lock, so offers can never pair a new baseline with an old id.
    fn record_agreed(&self, manifest: RootManifest, manifest_id: BlobId) {
        let mut g = self.lock();
        g.baseline = Some(manifest);
        g.agreed = Some(manifest_id);
        drop(g);
        self.changed.notify_all();
    }

    /// Wait for the CURRENT folder pointer the same way.
    fn wait_current(&self, deadline: Instant) -> Option<Arc<SnapshotData>> {
        let mut g = self.lock();
        loop {
            if let Some(snap) = g.current.clone() {
                return Some(snap);
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let (ng, _) = self.changed.wait_timeout(g, deadline - now).unwrap();
            g = ng;
        }
    }

    fn baseline_root(&self) -> Option<BlobId> {
        self.lock().baseline.as_ref().map(|m| m.root_tree_id)
    }

    fn agreed_id(&self) -> Option<BlobId> {
        self.lock().agreed
    }

    fn current_root(&self) -> Option<BlobId> {
        self.lock()
            .current
            .as_ref()
            .map(|s| s.manifest.root_tree_id)
    }

    pub fn current_manifest_id(&self) -> Option<BlobId> {
        self.lock().current.as_ref().map(|s| s.manifest_id)
    }

    fn scan_counts(&self) -> Option<ScanStats> {
        self.lock().scan_stats.clone()
    }

    fn pending_changes(&self) -> Option<i64> {
        let g = self.lock();
        let baseline = g.baseline.as_ref()?;
        let current = g.current.as_ref()?;
        if baseline.root_tree_id == current.manifest.root_tree_id {
            Some(0)
        } else {
            Some(-1)
        }
    }

    /// Wake every waiter (shutdown): they re-check their deadlines instead
    /// of sleeping out a 10s window while the engine dies.
    fn wake_all(&self) {
        self.changed.notify_all();
    }
}

struct SharedState {
    shutdown: AtomicBool,
    force_full_scan: AtomicBool,
    stats: Mutex<EngineStats>,
    /// window before their `JoinHandle` lands in the joins vec.
    /// Incremented SYNCHRONOUSLY in the accept loop before `spawn`,
    /// decremented by each handler's [`LiveSession`], so shutdown can
    /// account for handlers even in that window.
    live_sessions: Mutex<usize>,
    live_idle: Condvar,
    /// Remaining accept permits ([`MAX_CONCURRENT_SESSIONS`]).
    free_permits: Mutex<usize>,
    /// Park for [`EngineHandle::join_until_signal`]; notified by shutdown.
    park: Mutex<()>,
    park_cv: Condvar,
    peer_connectivity: Mutex<HashMap<BlobId, (Instant, &'static str)>>,
}

impl SharedState {
    fn new() -> Self {
        SharedState {
            shutdown: AtomicBool::new(false),
            force_full_scan: AtomicBool::new(false),
            stats: Mutex::new(EngineStats::default()),
            live_sessions: Mutex::new(0),
            live_idle: Condvar::new(),
            free_permits: Mutex::new(MAX_CONCURRENT_SESSIONS),
            park: Mutex::new(()),
            park_cv: Condvar::new(),
            peer_connectivity: Mutex::new(HashMap::new()),
        }
    }

    fn record_peer_connectivity(&self, peer: BlobId, status: &'static str) {
        if let Ok(mut map) = self.peer_connectivity.lock() {
            map.insert(peer, (Instant::now(), status));
        }
    }

    fn peer_connectivity(&self, peer: &BlobId) -> &'static str {
        if let Ok(map) = self.peer_connectivity.lock() {
            if let Some((at, status)) = map.get(peer) {
                if at.elapsed() < Duration::from_secs(60) {
                    return status;
                }
            }
        }
        "unknown"
    }

    fn wake_parked(&self) {
        self.park_cv.notify_all();
    }

    fn bump(&self, f: impl FnOnce(&mut EngineStats)) {
        f(&mut self.stats.lock().unwrap());
    }

    fn stats(&self) -> EngineStats {
        *self.stats.lock().unwrap()
    }

    fn shutting_down(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    /// Semaphore-style try-acquire. `None` = engine busy: caller rejects
    /// politely (log + drop) instead of queueing unbounded work.
    fn acquire_permit(self: &Arc<Self>) -> Option<SessionPermit> {
        let mut g = self.free_permits.lock().unwrap();
        if *g == 0 {
            return None;
        }
        *g -= 1;
        Some(SessionPermit {
            shared: Arc::clone(self),
        })
    }

    /// Count this handler BEFORE spawning its thread (see `live_sessions`).
    fn register_live_session(self: &Arc<Self>) -> LiveSession {
        *self.live_sessions.lock().unwrap() += 1;
        LiveSession {
            shared: Arc::clone(self),
        }
    }

    /// Block until every registered session handler has exited.
    fn wait_sessions_done(&self) {
        let mut g = self.live_sessions.lock().unwrap();
        while *g > 0 {
            g = self.live_idle.wait(g).unwrap();
        }
    }
}

/// RAII accept permit: released when the handler finishes or is dropped.
struct SessionPermit {
    shared: Arc<SharedState>,
}

impl Drop for SessionPermit {
    fn drop(&mut self) {
        *self.shared.free_permits.lock().unwrap() += 1;
    }
}

/// RAII liveness marker: decrement + notify shutdown on exit, panics
/// included, so a dying handler can never wedge `shutdown`.
struct LiveSession {
    shared: Arc<SharedState>,
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        let mut g = self.shared.live_sessions.lock().unwrap();
        *g -= 1;
        self.shared.live_idle.notify_all();
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

/// Injectable wall clock: seconds + nanoseconds since the epoch. Tests
/// pin this so tick logic is deterministic without real time.
pub(crate) type ClockFn = Arc<dyn Fn() -> (i64, u32) + Send + Sync>;

/// Injectable snapshot source (the scan). Tests substitute canned outputs
/// to interleave tick-vs-adopt deterministically.
pub(crate) type SnapshotSourceFn = Arc<
    dyn Fn(
            &Store,
            ferry_store::chunker::ValidatedPoly,
            &Path,
            &SnapshotIdentity,
        ) -> Result<SnapshotOutput, SnapshotError>
        + Send
        + Sync,
>;

fn system_clock() -> ClockFn {
    Arc::new(now_parts)
}

fn real_snapshot_source() -> SnapshotSourceFn {
    Arc::new(snapshot_dir_incremental)
}

/// Audit-grade scanner for the first tick after a session touched the tree:
/// apply paths restore recorded mtimes, so strict stat-reuse could mistake a
/// same-size conflict-tie rewrite for an untouched file. One full read+chunk
/// pass re-grounds the reuse baseline; only quiet ticks run incremental.
fn audit_snapshot_source() -> SnapshotSourceFn {
    Arc::new(snapshot_dir)
}

struct Ctx {
    cfg: EngineConfig,
    /// This daemon's long-term device identity: a persisted X25519 keypair
    /// loaded via `SyncEngine::set_identity` (production) or, in tests, the
    /// tag-derived constructor. Its PUBLIC key is the manifest
    /// `device_id`, the handshake `stat_pub`, and the ledger's peer key.
    identity: DeviceIdentity,
    store: Arc<Store>,
    transport: Arc<dyn Transport>,
    session_lock: Mutex<()>,
    /// The folder-pointer state machine (T-07): one owner for current /
    /// latest / baseline / agreed / next-parent.
    folder: Arc<FolderState>,
    shared: Arc<SharedState>,
    /// Local peer authorization policy (T-18).
    peer_policy: PeerPolicy,
    clock: ClockFn,
    snapshot_source: SnapshotSourceFn,
    /// Present only when the real scanner is in use (no test injection):
    /// the audit-grade walk for post-session ticks. Tests inject a single
    /// scripted source, so there is nothing to upgrade to.
    audit_source: Option<SnapshotSourceFn>,
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

    /// The CURRENT folder pointer (own latest or adopted), waiting out the
    /// same pre-first-tick window.
    fn current_snapshot(&self) -> Result<Arc<SnapshotData>, SessionError> {
        let deadline = Instant::now() + FIRST_STATE_WAIT;
        self.folder
            .wait_current(deadline)
            .ok_or_else(|| SessionError::Other("no local folder state available".into()))
    }

    /// Record the last-agreed pointer against a peer device: THE canonical
    /// 77-byte ledger record (`ferry_store::agreement`, byte-exact per
    /// `docs/store-format.md` §"Last-agreed manifest pointer"). Also moves
    /// the in-memory baseline so divergence gating sees agreement.
    fn record_agreement(
        &self,
        peer: BlobId,
        manifest_bytes: &[u8],
        manifest_id: BlobId,
    ) -> Result<(), SessionError> {
        let (sec, nsec) = now_parts();
        AgreementLedger::new(self.store.store_dir())
            .record(
                &self.cfg.folder_id,
                &AgreedRecord {
                    peer_device_id: peer,
                    manifest_id,
                    agreed_sec: sec,
                    agreed_nsec: nsec,
                },
            )
            .map_err(|e| SessionError::Other(format!("agreement ledger: {e}")))?;
        let manifest = if manifest_bytes.is_empty() {
            match self
                .store
                .get(ferry_store::format::BlobKind::Manifest, &manifest_id)
            {
                Ok(b) => parse_manifest(&b)?,
                Err(_) => {
                    self.status(&format!(
                        "STATE agreed={} recorded vs {} (manifest body not in store)",
                        hex(&manifest_id),
                        hex_short(&peer)
                    ));
                    return Ok(());
                }
            }
        } else {
            let _ = self
                .store
                .put_meta(ferry_store::format::BlobKind::Manifest, manifest_bytes);
            let _ = self.store.flush();
            let _ = self.store.write_index_snapshot();
            parse_manifest(manifest_bytes)?
        };
        self.folder.record_agreed(manifest, manifest_id);
        self.status(&format!(
            "STATE agreed={} recorded vs {}",
            hex(&manifest_id),
            hex_short(&peer)
        ));
        Ok(())
    }

    /// One poll iteration: resnapshot, publish state (adopt-and-hold),
    /// maybe dial.
    ///
    /// Hold rule: when the scanned tree's root equals the current
    /// pointer's root, the scan minted nothing worth announcing — keep the
    /// current manifest id (possibly an ADOPTED peer manifest) so both
    /// sides' round-2 ids stay comparable. A differing root means real
    /// local change: mint a child of the current lineage.
    fn tick(&self, n: u64) -> Result<(), SessionError> {
        // Capture the pre-scan view FIRST: publication validates against it
        // under the folder lock, so an adoption landing mid-scan wins.
        let tok = self.folder.scan_token();
        let (sec, nsec) = (self.clock)();
        let identity = SnapshotIdentity {
            folder_id: self.cfg.folder_id,
            device_id: *self.identity.device_id(),
            parent_manifest_id: tok.parent,
            created_sec: sec,
            created_nsec: nsec,
        };
        // A session that applied changes or adopted a manifest forces ONE
        // audit-grade scan (apply restores mtimes; stat-reuse must not judge
        // the post-session tree). Quiet ticks stay on the incremental walk.
        let forced = self.shared.force_full_scan.swap(false, Ordering::Relaxed);
        let scan: SnapshotSourceFn = match (forced, &self.audit_source) {
            (true, Some(audit)) => Arc::clone(audit),
            _ => Arc::clone(&self.snapshot_source),
        };
        let out: SnapshotOutput =
            (scan)(&self.store, self.cfg.poly, &self.cfg.tree_dir, &identity)?;
        let manifest_bytes = serialize_manifest(&out.manifest);
        let data = Arc::new(SnapshotData {
            manifest_id: out.manifest_id,
            manifest: out.manifest.clone(),
            manifest_bytes,
        });

        let outcome = self.folder.publish_scan(tok, data, out.stats);
        match outcome {
            PublishOutcome::DiscardedStale => {
                // An adoption landed while we scanned; the scan described
                // pre-adoption tree state. The next tick resnapshots the
                // post-apply tree and publishes then.
                self.status("STATE scan discarded: adoption landed mid-scan");
                return Ok(());
            }
            PublishOutcome::Held | PublishOutcome::Minted => {}
        }

        let base_root = self.folder.baseline_root();
        let current_manifest = self
            .folder
            .current_manifest_id()
            .unwrap_or(out.root_tree_id);
        self.status(&format!(
            "STATE root={} agreed={}",
            hex(&current_manifest),
            self.folder.agreed_id().map_or("none".into(), |i| hex(&i))
        ));

        // Connector drives sessions; listener relies on opportunistic dials
        // from the peer to discover ITS changes. Divergence from the agreed
        // baseline still gates dialing (the M0 bone). When fully settled —
        // current root == baseline root == the manifest both sides settled on
        // last session — the per-tick dial is skipped entirely; only the
        // opportunistic backstop fires, which must stay live because it is
        // also how connector-side peers announce listen-role changes.
        let diverged = base_root != Some(out.root_tree_id);
        if self.cfg.connect_to.is_some()
            && (diverged || n.is_multiple_of(u64::from(self.cfg.opportunistic_every)))
        {
            // try_lock: never queue behind a serving session; next tick retries.
            if let Ok(_guard) = self.session_lock.try_lock() {
                self.dial_and_run();
            }
        }
        Ok(())
    }

    fn dial_and_run(&self) {
        let Some(addr) = self.cfg.connect_to else {
            return;
        };
        match self.transport.dial(addr) {
            Ok(mut conn) => match run_session_v1(conn.as_mut(), self, true) {
                Ok(()) => self.bump_ok(),
                Err(e) => {
                    self.note_session_failure(&e);
                    self.status(&format!("SESSION failed (dial): {e}"));
                }
            },
            Err(e) => {
                if let Some(peer) = self.cfg.expected_peer_id {
                    self.shared.record_peer_connectivity(peer, "unreachable");
                }
                self.status(&format!("SESSION dial error: {e}"));
            }
        }
    }

    /// Failed-session bookkeeping: every failure counts once; verification
    /// refusals that surface AS session errors (a tampered sealed frame
    /// dies at its tag before any item-level check can run) also count as
    /// rejected transfers so integrity accounting stays complete.
    fn note_session_failure(&self, e: &SessionError) {
        if matches!(
            e,
            SessionError::Wire(
                ferry_proto::error::ProtoError::Auth(_)
                    | ferry_proto::error::ProtoError::IdentityMismatch { .. }
            ) | SessionError::PeerUnauthorized(_)
        ) {
            self.bump_rejected();
        }
        self.bump_failed();
    }
}

/// Re-request budget for corrupt/missing items within one session.
const MAX_ITEM_RETRIES: u32 = 3;

/// One protocol v1 session over an established transport connection:
/// authenticated, sealed handshake first, then the offer/pull/agree/BYE
/// conversation. Caller holds the per-daemon session lock.
fn run_session_v1(conn: &mut dyn Connection, ctx: &Ctx, dialer: bool) -> Result<(), SessionError> {
    let role = if dialer {
        ferry_proto::Role::Initiator
    } else {
        ferry_proto::Role::Responder
    };

    let ledger = PeerLedger::new(ctx.store.store_dir());
    let (expect, is_tofu_fresh) = match &ctx.peer_policy {
        PeerPolicy::AllowList(set) => {
            if set.len() == 1 {
                let pin = *set.iter().next().unwrap();
                (ExpectPeer::Pin(pin), false)
            } else {
                (ExpectPeer::TrustOnFirstUse, false)
            }
        }
        PeerPolicy::TrustOnFirstUse => {
            let known = ledger.list_peers(&ctx.cfg.folder_id)?;
            if let Some(first) = known.first() {
                (ExpectPeer::Pin(*first), false)
            } else {
                (ExpectPeer::TrustOnFirstUse, true)
            }
        }
    };

    let mut link = ConnLink(conn);
    let mut est: Established = session::establish(&mut link, role, &ctx.identity, expect, true)?;

    match &ctx.peer_policy {
        PeerPolicy::AllowList(set) => {
            if !set.contains(&est.peer) {
                let _ = est.io.send_bye(ferry_proto::error::ByeReason::AuthFailed);
                ctx.shared.record_peer_connectivity(est.peer, "unreachable");
                ctx.status(&format!(
                    "PEER unauthorized: {} not in allow-list",
                    hex(&est.peer)
                ));
                return Err(SessionError::PeerUnauthorized(hex(&est.peer)));
            }
        }
        PeerPolicy::TrustOnFirstUse => {
            if is_tofu_fresh {
                ctx.status(&format!(
                    "PEER new device trusted (TOFU): {}",
                    hex(&est.peer)
                ));
                ledger.record_peer(&ctx.cfg.folder_id, &est.peer)?;
            }
        }
    }

    ctx.status(&format!(
        "SESSION v1 peer={} encrypted=yes version={} role={}",
        hex_short(&est.peer),
        est.agreed_version,
        if dialer { "initiator" } else { "responder" }
    ));

    let snap = ctx.current_snapshot()?;
    let host = EngineHost { ctx };
    let res = exchange::run_v1_session(
        &mut est,
        &host,
        &ctx.store,
        ctx.cfg.folder_id,
        CurrentState {
            id: snap.manifest_id,
            bytes: snap.manifest_bytes.clone(),
            manifest: snap.manifest.clone(),
        },
        MAX_ITEM_RETRIES,
        dialer,
    );
    if res.is_ok() {
        ctx.shared.record_peer_connectivity(est.peer, "reachable");
    } else {
        ctx.shared.record_peer_connectivity(est.peer, "unreachable");
    }
    res
}

/// The engine's [`ExchangeHost`]: routes driver callbacks into snapshot
/// pointers, stats, status lines, baselines, and agreement ledgers.
struct EngineHost<'x> {
    ctx: &'x Ctx,
}

impl ExchangeHost for EngineHost<'_> {
    fn status(&self, line: &str) {
        self.ctx.status(line);
    }

    fn bump_rejected(&self) {
        self.ctx.bump_rejected();
    }

    fn tree_root(&self) -> &std::path::Path {
        &self.ctx.cfg.tree_dir
    }

    fn pin_state_dir(&self) -> Option<&std::path::Path> {
        self.ctx.cfg.pin_state_dir.as_deref()
    }

    fn adopt(&self, bytes: &[u8], manifest: &RootManifest) -> Result<(), SessionError> {
        let id = *blake3::hash(bytes).as_bytes();
        let data = Arc::new(SnapshotData {
            manifest_id: id,
            manifest: manifest.clone(),
            manifest_bytes: bytes.to_vec(),
        });
        self.ctx
            .shared
            .force_full_scan
            .store(true, Ordering::Relaxed);
        self.ctx.folder.adopt_peer(data);
        self.ctx.status(&format!(
            "STATE root={} adopted",
            hex(&manifest.root_tree_id)
        ));
        Ok(())
    }

    fn note_tree_mutation(&self) {
        self.ctx
            .shared
            .force_full_scan
            .store(true, Ordering::Relaxed);
    }

    fn agree(&self, peer: BlobId, bytes: &[u8], manifest_id: BlobId) -> Result<(), SessionError> {
        self.ctx.record_agreement(peer, bytes, manifest_id)
    }
}

fn hex_short(b: &BlobId) -> String {
    hex(b)[..12].to_string()
}

pub(crate) fn now_parts() -> (i64, u32) {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_secs() as i64, d.subsec_nanos())
}

/// Shared with the v1 exchange driver (`exchange::pull_content`).
pub(crate) fn collect_chunk_ids(changes: &ChangeSet) -> Vec<BlobId> {
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
    peer_policy: Option<PeerPolicy>,
    /// Explicit device identity (T-14/T-18 follow-up): when set, sessions use
    /// this keypair instead of the tag-derived skeleton identity, so the wire
    /// peer id equals the `device_pub` recorded in `CONFIG_HEAD` wrap entries
    /// and allow-list authorization can match. None = legacy tag derivation.
    identity: Option<DeviceIdentity>,
    /// Test seams (T-07): None = real clock / real scanner.
    clock: Option<ClockFn>,
    snapshot_source: Option<SnapshotSourceFn>,
}

impl SyncEngine {
    /// Build (but do not start) an engine. Opens or creates the store,
    /// creates the tree dir, binds the listener when configured.
    ///
    /// Startup also runs the T-20 crash-residue sweep, bounded older-than:
    /// store-side temps under `.ferry/` (pack staging, sidecar and ledger
    /// temps — `ferry_store::reclaim`) plus tree-side materialize temps
    /// (`ferry_materialize::sweep_stale_temps`). Failures are best-effort
    /// and never block startup.
    pub fn new(cfg: EngineConfig, transport: Arc<dyn Transport>) -> Result<Self, EngineError> {
        std::fs::create_dir_all(&cfg.tree_dir)?;
        let stale = Duration::from_secs(ferry_materialize::DEFAULT_STALE_TEMP_AGE_SECS);
        let _ = ferry_materialize::sweep_stale_temps(&cfg.tree_dir, stale);
        let _ = ferry_store::reclaim::sweep_store_temps(&cfg.store_dir, stale);
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
            peer_policy: None,
            identity: None,
            clock: None,
            snapshot_source: None,
        })
    }

    /// Explicitly configure the peer authorization policy (T-18).
    pub fn set_peer_policy(&mut self, policy: PeerPolicy) {
        self.peer_policy = Some(policy);
    }

    /// Run sessions with a real device identity instead of the tag-derived
    /// skeleton keypair (T-14/T-18 follow-up). Production callers (ferry-cli)
    /// pass the `FERRY_HOME` identity so handshake ids match the `device_pub`
    /// entries their peers seed allow-lists from.
    pub fn set_identity(&mut self, identity: DeviceIdentity) {
        self.identity = Some(identity);
    }

    /// Swap the clock and the scanner (test seam, T-07): tick logic becomes
    /// deterministic without real time or a real tree.
    #[cfg(test)]
    pub(crate) fn set_test_injections(
        &mut self,
        clock: ClockFn,
        snapshot_source: SnapshotSourceFn,
    ) {
        self.clock = Some(clock);
        self.snapshot_source = Some(snapshot_source);
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
        crate::exchange::ingest_pack_verified(&store, claimed_name, bytes)
    }

    /// Spawn poll (+ accept) threads. Dropping the returned handle shuts
    /// everything down and joins.
    pub fn start(mut self) -> EngineHandle {
        let listener = self.listener.take();
        let listen_addr = listener.as_ref().and_then(|l| l.local_addr().ok());
        let shared = Arc::new(SharedState::new());
        let folder = Arc::new(FolderState::new());
        if let Ok(records) =
            AgreementLedger::new(self.store.store_dir()).list_folder(&self.cfg.folder_id)
        {
            if let Some((_, rec)) = records
                .iter()
                .max_by_key(|(_, rec)| (rec.agreed_sec, rec.agreed_nsec))
            {
                if let Ok(bytes) = self.store.get(BlobKind::Manifest, &rec.manifest_id) {
                    if let Ok(manifest) = parse_manifest(&bytes) {
                        folder.record_agreed(manifest, rec.manifest_id);
                    }
                }
            }
        }
        let peer_policy = if let Some(policy) = self.peer_policy.take() {
            policy
        } else if let Some(pin) = self.cfg.expected_peer_id {
            PeerPolicy::AllowList([pin].into())
        } else {
            resolve_peer_policy_from_disk(&self.cfg, &self.store)
        };
        let store_dir = self.store.store_dir().to_path_buf();
        let folder_id = self.cfg.folder_id;
        // Production daemon (ferry-daemon/src/main.rs) always calls
        // `set_identity(load_or_create(...))` before `start`, so the
        // tag-derived fallback is unreachable in production. It remains for
        // tests (unit tests with `cfg(test)` and integration tests without
        // `cfg(test)` that don't call `set_identity`) so they keep
        // deterministic ids explicitly via the tag.
        let device = self
            .identity
            .take()
            .unwrap_or_else(|| device_identity_for_tag(&self.cfg.tag));
        let injected_scan = self.snapshot_source.is_some();
        let snapshot_source = self
            .snapshot_source
            .take()
            .unwrap_or_else(real_snapshot_source);
        let audit_source = (!injected_scan).then(audit_snapshot_source);
        let ctx = Arc::new(Ctx {
            cfg: self.cfg.clone(),
            identity: device,
            store: Arc::clone(&self.store),
            transport: Arc::clone(&self.transport),
            session_lock: Mutex::new(()),
            folder: Arc::clone(&folder),
            shared: Arc::clone(&shared),
            peer_policy,
            clock: self.clock.take().unwrap_or_else(system_clock),
            snapshot_source,
            audit_source,
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
            folder,
            joins,
            listen_addr,
            transport: Arc::clone(&self.transport),
            store_dir,
            folder_id,
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

/// Deterministic per-tag device identity — TEST-ONLY (ticket 12). The one
/// production identity source is a persisted keypair via
/// `ferry_crypto::identity::load_or_create`; tag derivation exists so engine
/// tests keep deterministic device ids deliberately.
/// This remains `pub` so integration tests (`crates/ferry-sync/tests/*`)
/// can use it, but production `SyncEngine::start` never calls it — it
/// requires `set_identity` with a persisted keypair.
pub fn device_identity_for_tag(tag: &str) -> DeviceIdentity {
    use blake3::Hasher;
    use rand::SeedableRng;
    let mut h = Hasher::new();
    h.update(b"ferry/v0/device-key:");
    h.update(tag.as_bytes());
    let digest = h.finalize();
    let mut seed = [0u8; 32];
    // One extra widening round so the seed is not the raw hash output.
    let mut rng = StdRng::from_seed(*digest.as_bytes());
    use rand::RngCore;
    rng.fill_bytes(&mut seed);
    DeviceIdentity::from_secret_bytes(&seed)
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
    while !shared.shutting_down() {
        match listener.accept() {
            Ok(mut conn) => {
                if shared.shutting_down() {
                    break;
                }
                // Bounded accept (T-07): no permit = engine busy; reject
                // politely by dropping the connection instead of queueing
                // unbounded threads.
                let Some(permit) = shared.acquire_permit() else {
                    ctx.status("SESSION refused: engine busy");
                    drop(conn);
                    continue;
                };
                // Count the handler BEFORE spawn so shutdown can account
                // for it even before its JoinHandle lands in `joins`.
                let live = shared.register_live_session();
                let ctx = Arc::clone(&ctx);
                let h = std::thread::Builder::new()
                    .name(format!("{}-session", ctx.cfg.tag))
                    .spawn(move || {
                        let _live = live;
                        let _permit = permit;
                        // Serialize sessions; bail promptly on shutdown.
                        let _guard = ctx.session_lock.lock().unwrap();
                        if ctx.shared.shutting_down() {
                            return;
                        }
                        match run_session_v1(conn.as_mut(), &ctx, false) {
                            Ok(()) => ctx.bump_ok(),
                            Err(e) => {
                                ctx.note_session_failure(&e);
                                ctx.status(&format!("SESSION failed (accept): {e}"));
                            }
                        }
                    })
                    .expect("spawn session handler");
                {
                    let mut g = joins.lock().unwrap();
                    g.retain(|j| !j.is_finished());
                    g.push(h);
                }
            }
            Err(e) => {
                if shared.shutting_down() {
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
        if shared.shutting_down() {
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
    folder: Arc<FolderState>,
    joins: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>,
    listen_addr: Option<SocketAddr>,
    transport: Arc<dyn Transport>,
    store_dir: PathBuf,
    folder_id: [u8; 16],
    tag: String,
}

impl EngineHandle {
    pub fn tag(&self) -> &str {
        &self.tag
    }

    pub fn agreed_id(&self) -> Option<BlobId> {
        self.folder.agreed_id()
    }

    pub fn root_id(&self) -> Option<BlobId> {
        self.folder.current_root()
    }

    pub fn current_manifest_id(&self) -> Option<BlobId> {
        self.folder.current_manifest_id()
    }

    pub fn scan_counts(&self) -> Option<ScanStats> {
        self.folder.scan_counts()
    }

    pub fn pending_changes(&self) -> Option<i64> {
        self.folder.pending_changes()
    }

    pub fn stats(&self) -> EngineStats {
        self.shared.stats()
    }

    pub fn listen_addr(&self) -> Option<SocketAddr> {
        self.listen_addr
    }

    pub fn peer_connectivity(&self, peer: &BlobId) -> &'static str {
        self.shared.peer_connectivity(peer)
    }

    pub fn record_peer_connectivity(&self, peer: BlobId, status: &'static str) {
        self.shared.record_peer_connectivity(peer, status);
    }

    /// Return the list of pinned/persisted peer device IDs for this engine's folder (T-18).
    pub fn pinned_peers(&self) -> Result<Vec<BlobId>, std::io::Error> {
        let ledger = PeerLedger::new(&self.store_dir);
        ledger.list_peers(&self.folder_id)
    }

    pub fn store_dir(&self) -> &Path {
        &self.store_dir
    }

    pub fn folder_id(&self) -> &[u8; 16] {
        &self.folder_id
    }

    /// Trigger an immediate manual audit-grade filesystem rescan.
    pub fn trigger_scan(&self) {
        self.shared.force_full_scan.store(true, Ordering::Relaxed);
    }

    /// Signal shutdown and wait for every thread to exit — the poll loop,
    /// the accept loop, AND every session handler (including ones still in
    /// their spawn window). Idempotent.
    pub fn shutdown(&self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        // Wake condvar waiters so nobody sleeps out a state-wait window
        // while the engine dies; they re-check and exit on deadline/flag.
        self.folder.wake_all();
        self.shared.wake_parked();
        // Unblock a possibly-blocked accept() with a throwaway connection.
        if let Some(addr) = self.listen_addr {
            let _ = self.transport.dial(addr);
        }
        while let Some(j) = self.joins.lock().unwrap().pop() {
            let _ = j.join();
        }
        // Handlers counted before spawn (T-07): wait for any that were in
        // their registration window during the drain above.
        self.shared.wait_sessions_done();
        // Final sweep: a handler finishing now may have just pushed its
        // JoinHandle; nothing can spawn after this (accept loop is gone).
        while let Some(j) = self.joins.lock().unwrap().pop() {
            let _ = j.join();
        }
    }

    /// Block the calling thread while the engine runs. The daemon binary
    /// parks here; actual termination is a process signal (std has no
    /// handler story), after which Drop runs the same shutdown path.
    pub fn join_until_signal(&self) {
        let mut guard = self.shared.park.lock().unwrap();
        while !self.shared.shutting_down() {
            // Long slice; shutdown()'s wake_parked releases the park
            // immediately, the timeout only bounds a lost-wake race.
            let (ng, _) = self
                .shared
                .park_cv
                .wait_timeout(guard, Duration::from_secs(5))
                .unwrap();
            guard = ng;
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

    // ---- T-07: folder-pointer state machine, injectable tick inputs ----

    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    /// A canned [`SnapshotOutput`] without touching disk or a tree.
    fn fake_scan(sec: i64, dev: [u8; 32], root: BlobId, parent: BlobId) -> SnapshotOutput {
        let m = RootManifest {
            folder_id: crate::DEFAULT_FOLDER_ID,
            device_id: dev,
            created_sec: sec,
            created_nsec: 0,
            root_tree_id: root,
            parent_manifest_id: parent,
        };
        let bytes = serialize_manifest(&m);
        SnapshotOutput {
            manifest_id: *blake3::hash(&bytes).as_bytes(),
            manifest: m,
            root_tree_id: root,
            stats: ferry_store::snapshot::ScanStats::default(),
            refused: Vec::new(),
        }
    }

    fn snap_data(out: &SnapshotOutput) -> Arc<SnapshotData> {
        Arc::new(SnapshotData {
            manifest_id: out.manifest_id,
            manifest: out.manifest.clone(),
            manifest_bytes: serialize_manifest(&out.manifest),
        })
    }

    /// A full Ctx against a throwaway store, with injectable clock/scanner.
    /// Returns the Ctx and the [`tempfile::TempDir`] keeping its store alive.
    fn test_ctx(
        folder: Arc<FolderState>,
        tag: &str,
        clock: ClockFn,
        source: SnapshotSourceFn,
    ) -> (Ctx, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store_dir = dir.path().join("root");
        let store = Arc::new(open_or_create_store(&store_dir.join("store")).expect("test store"));
        let mut cfg = EngineConfig::default_for_test(11);
        cfg.tag = tag.into();
        cfg.store_dir = store_dir.join("store");
        cfg.tree_dir = store_dir.join("tree");
        std::fs::create_dir_all(&cfg.tree_dir).unwrap();
        let ctx = Ctx {
            cfg,
            identity: device_identity_for_tag(tag),
            store,
            transport: Arc::new(crate::transport::TcpTransport),
            session_lock: Mutex::new(()),
            folder,
            shared: Arc::new(SharedState::new()),
            peer_policy: PeerPolicy::TrustOnFirstUse,
            clock,
            snapshot_source: source,
            audit_source: None,
        };
        (ctx, dir)
    }

    /// Scripted scanner: pops one canned output per call; runs an optional
    /// hook BEFORE returning, which is how tests replay the exact
    /// tick-vs-adopt interleaving from spec B1 deterministically.
    type ScanScript = Arc<
        Mutex<
            VecDeque<(
                SnapshotOutput,
                Option<Box<dyn Fn(&FolderState) + Send + Sync>>,
            )>,
        >,
    >;

    fn scripted_source(script: ScanScript, folder: &Arc<FolderState>) -> SnapshotSourceFn {
        let folder = Arc::clone(folder);
        Arc::new(move |_store, _poly, _dir, _identity| {
            let (out, hook) = script
                .lock()
                .unwrap()
                .pop_front()
                .expect("script exhausted");
            if let Some(hook) = hook {
                hook(&folder);
            }
            Ok(out)
        })
    }

    fn pinned_clock(sec: i64) -> ClockFn {
        Arc::new(move || (sec, 0))
    }

    #[test]
    fn adoption_survives_a_scan_that_started_before_it() {
        // Spec B1 interleaving, replayed exactly: a tick reads its parent,
        // starts scanning; a session adopts a peer manifest DURING the
        // scan; the tick then tries to publish. The pre-adoption scan must
        // be discarded whole — never published over the adopted lineage.
        let me = [7u8; 32];
        let peer = [9u8; 32];
        let scan_a = fake_scan(10, me, [1; 32], [0; 32]); // first own snapshot
        let adopted_out = fake_scan(20, peer, [2; 32], [0; 32]); // peer manifest
        let scan_c = fake_scan(30, me, [3; 32], [0; 32]); // stale mid-scan product

        let folder = Arc::new(FolderState::new());
        let adopted = snap_data(&adopted_out);
        let adopted_id = adopted.manifest_id;

        let script: ScanScript = Arc::new(Mutex::new(VecDeque::from([
            (
                scan_c,
                Some(Box::new({
                    let adopted = Arc::clone(&adopted);
                    move |f: &FolderState| f.adopt_peer(Arc::clone(&adopted))
                })
                    as Box<dyn Fn(&FolderState) + Send + Sync>),
            ),
            (scan_a, None),
        ])));
        let (ctx, _dir) = test_ctx(
            Arc::clone(&folder),
            "t07-a",
            pinned_clock(100),
            scripted_source(Arc::clone(&script), &folder),
        );

        // Tick 1 adopts mid-scan; its own scan product must NOT clobber.
        ctx.tick(1).unwrap();
        {
            let g = folder.lock();
            assert_eq!(
                g.current.as_ref().map(|s| s.manifest_id),
                Some(adopted_id),
                "mid-scan adoption must survive the concurrent tick"
            );
            assert_eq!(
                g.last_own_manifest_id, adopted_id,
                "next self-mint continues the ADOPTED lineage"
            );
            assert_eq!(
                g.latest.as_ref().map(|s| s.manifest_id),
                Some(adopted_id),
                "stale scan must not refresh the raw-scan slot either"
            );
        }

        // Tick 2 scans cleanly (no adoption races it) and publishes.
        let fresh = fake_scan(40, me, [4; 32], [0; 32]);
        script.lock().unwrap().push_back((fresh, None));
        ctx.tick(2).unwrap();
        {
            let g = folder.lock();
            assert_ne!(
                g.current.as_ref().map(|s| s.manifest_id),
                Some(adopted_id),
                "a clean later tick may mint again"
            );
            assert_eq!(
                Some(g.last_own_manifest_id),
                g.current.as_ref().map(|s| s.manifest_id),
                "current and next-parent move together under the one lock"
            );
        }
    }

    #[test]
    fn startup_sweep_removes_planted_stale_temps_at_every_site() {
        // T-20 acceptance: SyncEngine::new is the documented startup hook;
        // residue planted before startup must be reclaimed, live files kept.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let sd = root.join(".ferry");
        for d in ["tmp", "peers", "agreement", "index"] {
            std::fs::create_dir_all(sd.join(d)).unwrap();
        }
        let tree_dir = dir.path().join("tree");
        std::fs::create_dir_all(&tree_dir).unwrap();

        let stale: Vec<std::path::PathBuf> = vec![
            sd.join("tmp").join(format!("pack-{}.tmp", hex(&[7u8; 16]))),
            sd.join(format!("pin-state.json.tmp.{}.0.9", std::process::id())),
            sd.join("peers").join(".tmp-dead-beef"),
            sd.join("agreement").join(".tmp-aa-bb"),
            tree_dir.join(".ferry.notes.tmp.aabbccdd"),
        ];
        for p in &stale {
            std::fs::write(p, b"stale residue").unwrap();
            // Epoch mtime: far older than any sane sweep bound.
            let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
            f.set_times(std::fs::FileTimes::new().set_modified(SystemTime::UNIX_EPOCH))
                .unwrap();
        }
        let fresh = vec![
            sd.join("peers")
                .join(format!("{}-{}.peer", hex(&[1u8; 16]), hex(&[2u8; 32]))),
            tree_dir.join("real.txt"),
        ];
        for p in &fresh {
            std::fs::write(p, b"live").unwrap();
        }

        let mut cfg = EngineConfig::default_for_test(11);
        cfg.store_dir = root.clone();
        cfg.tree_dir = tree_dir.clone();
        let _engine = SyncEngine::new(cfg, Arc::new(crate::transport::TcpTransport)).unwrap();

        for p in &stale {
            assert!(!p.exists(), "startup must sweep {p:?}");
        }
        for p in &fresh {
            assert!(p.exists(), "startup must never touch {p:?}");
        }
    }

    #[test]
    fn rescan_of_unchanged_tree_holds_the_current_pointer() {
        // Adopt-and-hold: same scanned root keeps the current pointer (and
        // the announced id) stable so round-2 comparisons stay valid.
        let me = [3u8; 32];
        let root = [5; 32];
        let first = fake_scan(1, me, root, [0; 32]);
        let again = fake_scan(2, me, root, [0; 32]); // same tree, later stamp
        let first_id = first.manifest_id;
        let second_id = again.manifest_id;

        let folder = Arc::new(FolderState::new());
        let script: ScanScript =
            Arc::new(Mutex::new(VecDeque::from([(first, None), (again, None)])));
        let (ctx, _dir) = test_ctx(
            Arc::clone(&folder),
            "t07-b",
            pinned_clock(7),
            scripted_source(script, &folder),
        );

        ctx.tick(1).unwrap();
        assert_eq!(
            folder.lock().current.as_ref().map(|s| s.manifest_id),
            Some(first_id)
        );

        ctx.tick(2).unwrap();
        let g = folder.lock();
        assert_eq!(
            g.current.as_ref().map(|s| s.manifest_id),
            Some(first_id),
            "unchanged root must hold the current pointer"
        );
        assert_eq!(g.last_own_manifest_id, first_id);
        assert_eq!(
            g.latest.as_ref().map(|s| s.manifest_id),
            Some(second_id),
            "raw scan slot still refreshes every tick"
        );
    }

    #[test]
    fn snapshot_and_adoption_state_transitions() {
        let me = [1u8; 32];
        let peer = [2u8; 32];

        let folder = Arc::new(FolderState::new());
        let own = fake_scan(1, me, [1; 32], [0; 32]);
        let script: ScanScript = Arc::new(Mutex::new(VecDeque::from([(own, None)])));
        let (ctx, _dir) = test_ctx(
            Arc::clone(&folder),
            "t07-c",
            pinned_clock(3),
            scripted_source(script, &folder),
        );
        ctx.tick(1).unwrap();

        // Initial snapshot
        let before = ctx.current_snapshot().unwrap();
        assert_eq!(before.manifest.root_tree_id, [1; 32]);

        // Adopt + agree while no reader holds the lock.
        let peer_out = fake_scan(9, peer, [2; 32], [0; 32]);
        let peer_snap = snap_data(&peer_out);
        folder.adopt_peer(Arc::clone(&peer_snap));
        folder.record_agreed(peer_out.manifest.clone(), peer_out.manifest_id);

        // Snapshot after adoption: fully-new pair.
        let after = ctx.current_snapshot().unwrap();
        assert_eq!(after.manifest_id, peer_out.manifest_id);
        assert_eq!(after.manifest.root_tree_id, [2; 32]);

        // The earlier snapshot is untouched: snapshots are immutable Arcs.
        assert_eq!(before.manifest.root_tree_id, [1; 32]);
    }

    #[test]
    fn accept_permits_bound_concurrency_and_replenish() {
        let shared = Arc::new(SharedState::new());
        let mut held = Vec::new();
        for _ in 0..MAX_CONCURRENT_SESSIONS {
            held.push(shared.acquire_permit().expect("permit within bound"));
        }
        assert!(
            shared.acquire_permit().is_none(),
            "bound exhausted: further accepts must be rejected"
        );
        drop(held.pop());
        assert!(
            shared.acquire_permit().is_some(),
            "a finished handler frees its permit"
        );

        // Liveness accounting: registered-before-spawn handlers are
        // counted until their guard drops.
        let live = shared.register_live_session();
        drop(live);
        shared.wait_sessions_done(); // must return promptly now
    }

    #[test]
    fn shutdown_joins_everything_and_stops_all_writes() {
        // Probe seam: every scanner invocation is a write-driving event.
        // After shutdown() returns (all threads joined), the poll loop is
        // gone, so the probe must stay silent forever after.
        let calls = Arc::new(AtomicUsize::new(0));
        let probe_calls = Arc::clone(&calls);

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("engine");
        let mut cfg = EngineConfig::default_for_test(23);
        cfg.tag = "t07-shutdown".into();
        cfg.store_dir = root.join("store");
        cfg.tree_dir = root.join("tree");
        cfg.poll_interval = Duration::from_millis(15);
        cfg.bind_addr = Some("127.0.0.1:0".parse().unwrap());
        cfg.connect_to = None;

        let mut engine =
            SyncEngine::new(cfg, Arc::new(crate::transport::TcpTransport)).expect("engine");
        engine.set_test_injections(
            system_clock(),
            Arc::new(move |store, poly, source, identity| {
                probe_calls.fetch_add(1, AtomicOrdering::SeqCst);
                snapshot_dir(store, poly, source, identity)
            }),
        );
        let handle = engine.start();

        // Let several ticks run, then shut down and time the join.
        std::thread::sleep(Duration::from_millis(150));
        let started = Instant::now();
        handle.shutdown();
        let joined = started.elapsed();
        assert!(
            joined < Duration::from_secs(5),
            "shutdown must join promptly, took {joined:?}"
        );

        let at_shutdown = calls.load(AtomicOrdering::SeqCst);
        assert!(at_shutdown > 0, "poll loop ran while alive");
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(
            calls.load(AtomicOrdering::SeqCst),
            at_shutdown,
            "probe saw writes AFTER shutdown returned: a thread was not joined"
        );
    }

    #[test]
    fn peer_ledger_records_lists_and_forgets_peers() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = PeerLedger::new(dir.path());
        let folder1 = [1u8; 16];
        let folder2 = [2u8; 16];
        let peer_a = [10u8; 32];
        let peer_b = [20u8; 32];

        // Initially empty.
        assert_eq!(ledger.list_peers(&folder1).unwrap(), Vec::<BlobId>::new());

        // Record peer_a for folder1.
        ledger.record_peer(&folder1, &peer_a).unwrap();
        assert_eq!(ledger.list_peers(&folder1).unwrap(), vec![peer_a]);
        // folder2 still empty.
        assert_eq!(ledger.list_peers(&folder2).unwrap(), Vec::<BlobId>::new());

        // Record peer_b for folder1.
        ledger.record_peer(&folder1, &peer_b).unwrap();
        assert_eq!(ledger.list_peers(&folder1).unwrap(), vec![peer_a, peer_b]);

        // Forget peer_a.
        assert!(ledger.forget_peer(&folder1, &peer_a).unwrap());
        assert_eq!(ledger.list_peers(&folder1).unwrap(), vec![peer_b]);

        // Forget peer_a again returns false (not found).
        assert!(!ledger.forget_peer(&folder1, &peer_a).unwrap());
    }

    #[test]
    fn peer_policy_seeds_from_config_head_bytes() {
        let folder_id = [42u8; 16];
        let dev1 = [1u8; 32];
        let dev2 = [2u8; 32];
        let wrapped = [7u8; ferry_crypto::folder_key::WRAPPED_LEN];

        let entries = vec![
            ferry_crypto::config_head::WrappedKeyEntry::new(dev1, wrapped),
            ferry_crypto::config_head::WrappedKeyEntry::new(dev2, wrapped),
        ];
        let bytes = ferry_crypto::config_head::write_config_head(&folder_id, &entries);

        let policy = PeerPolicy::from_config_head(&bytes).unwrap();
        match policy {
            PeerPolicy::AllowList(set) => {
                assert!(set.contains(&dev1));
                assert!(set.contains(&dev2));
                assert!(!set.contains(&[3u8; 32]));
            }
            PeerPolicy::TrustOnFirstUse => panic!("expected AllowList policy"),
        }
    }

    #[test]
    fn engine_handle_exposes_scan_counts_pending_and_connectivity() {
        let dir = tempfile::tempdir().unwrap();
        let store_dir = dir.path().join("store");
        let tree_dir = dir.path().join("tree");
        std::fs::create_dir_all(&tree_dir).unwrap();
        std::fs::write(tree_dir.join("f.txt"), b"data").unwrap();

        let mut cfg = EngineConfig::default_for_test(12345);
        cfg.store_dir = store_dir;
        cfg.tree_dir = tree_dir.clone();
        cfg.bind_addr = None;
        cfg.connect_to = None;

        let engine = SyncEngine::new(cfg, Arc::new(crate::transport::TcpTransport)).unwrap();
        let handle = engine.start();

        // Wait for first tick to scan tree
        let deadline = Instant::now() + Duration::from_secs(5);
        while handle.scan_counts().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }

        let counts = handle
            .scan_counts()
            .expect("scan counts should be available after tick");
        assert_eq!(counts.files, 1);
        assert_eq!(counts.dirs, 0);

        // No agreement recorded yet -> pending_changes is None
        assert_eq!(handle.pending_changes(), None);

        // Peer connectivity default is "unknown"
        let peer_dev = [99u8; 32];
        assert_eq!(handle.peer_connectivity(&peer_dev), "unknown");

        // Record connectivity observation
        handle.record_peer_connectivity(peer_dev, "reachable");
        assert_eq!(handle.peer_connectivity(&peer_dev), "reachable");

        handle.shutdown();
    }
}
