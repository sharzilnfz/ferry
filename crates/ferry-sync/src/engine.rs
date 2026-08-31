use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ferry_crypto::identity::DeviceIdentity;
use ferry_scan::{CurrentScan, ScanConfig, ScanEngine, ScanEvent, StoreHandle, WatchSignal};
use ferry_store::diff::ChangeSet;
use ferry_store::format::{hex, BlobId, BlobKind, PackId};
use ferry_store::manifest::{parse_manifest, serialize_manifest, RootManifest};
use rand::rngs::StdRng;
use rand::SeedableRng;

use crate::exchange::{self, CurrentState, ExchangeHost};
use crate::session::{self, ConnLink, Established, ExpectPeer};
use crate::transport::{Connection, Transport};
use ferry_store::agreement::{AgreedRecord, AgreementLedger};
pub use ferry_store::snapshot::ScanStats;
use ferry_store::store::Store;

pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(200);

pub const DEFAULT_OPPORTUNISTIC_EVERY: u32 = 50;

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub tag: String,

    pub store_dir: PathBuf,

    pub tree_dir: PathBuf,

    pub poly: ferry_store::chunker::ValidatedPoly,
    pub folder_id: [u8; 16],
    pub poll_interval: Duration,
    pub opportunistic_every: u32,

    pub bind_addr: Option<SocketAddr>,

    pub connect_to: Option<SocketAddr>,

    pub allow_trust_on_first_use: bool,

    pub pin_state_dir: Option<PathBuf>,

    pub quiet: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerPolicy {
    AllowList(HashSet<BlobId>),

    TrustOnFirstUse,
}

impl Default for PeerPolicy {
    fn default() -> Self {
        PeerPolicy::AllowList(HashSet::new())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerExpectation {
    Refuse,

    Pin(BlobId),

    TrustOnFirstUse { pin: bool },
}

impl PeerPolicy {
    pub fn from_allowed<I: IntoIterator<Item = BlobId>>(peers: I) -> Self {
        PeerPolicy::AllowList(peers.into_iter().collect())
    }

    pub fn from_config_head(
        bytes: &[u8],
    ) -> Result<Self, ferry_crypto::config_head::ConfigHeadError> {
        let ch = ferry_crypto::config_head::parse_config_head(bytes)?;
        let set: HashSet<BlobId> = ch.entries.into_iter().map(|e| e.device_pub).collect();
        Ok(PeerPolicy::AllowList(set))
    }

    pub fn remote_peers(&self, self_id: &BlobId) -> Vec<BlobId> {
        match self {
            PeerPolicy::AllowList(set) => {
                let mut peers: Vec<BlobId> = set.iter().copied().filter(|p| p != self_id).collect();
                peers.sort_unstable();
                peers
            }
            PeerPolicy::TrustOnFirstUse => Vec::new(),
        }
    }

    pub fn expected_peer(
        &self,
        self_id: &BlobId,
        folder_id: &[u8; 16],
        ledger: &PeerLedger,
    ) -> Result<PeerExpectation, std::io::Error> {
        match self {
            PeerPolicy::AllowList(_) => Ok(match self.remote_peers(self_id).as_slice() {
                [] => PeerExpectation::Refuse,
                [only] => PeerExpectation::Pin(*only),
                _ => PeerExpectation::TrustOnFirstUse { pin: false },
            }),
            PeerPolicy::TrustOnFirstUse => {
                let known = ledger.list_peers(folder_id)?;
                Ok(match known.first() {
                    Some(first) => PeerExpectation::Pin(*first),
                    None => PeerExpectation::TrustOnFirstUse { pin: true },
                })
            }
        }
    }

    pub fn admits(&self, peer: &BlobId) -> bool {
        match self {
            PeerPolicy::AllowList(set) => set.contains(peer),
            PeerPolicy::TrustOnFirstUse => true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PeerLedger {
    dir: PathBuf,
}

impl PeerLedger {
    pub fn new(store_dir: impl Into<PathBuf>) -> Self {
        PeerLedger {
            dir: store_dir.into().join("peers"),
        }
    }

    pub fn path_for(&self, folder_id: &[u8; 16], peer: &[u8; 32]) -> PathBuf {
        self.dir
            .join(format!("{}-{}.peer", hex(folder_id), hex(peer)))
    }

    pub fn record_peer(&self, folder_id: &[u8; 16], peer: &[u8; 32]) -> Result<(), std::io::Error> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self
            .dir
            .join(format!(".tmp-{}-{}", hex(folder_id), hex(peer)));
        std::fs::write(&tmp, hex(peer).as_bytes())?;
        std::fs::rename(&tmp, self.path_for(folder_id, peer))?;
        Ok(())
    }

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
                    return policy;
                }
            }
        }
    }
    if cfg.allow_trust_on_first_use {
        PeerPolicy::TrustOnFirstUse
    } else {
        PeerPolicy::default()
    }
}

fn resolve_ignore_policy_from_disk(
    cfg: &EngineConfig,
    store: &Store,
) -> Arc<dyn ferry_scan::IgnorePolicy> {
    let candidates = [
        cfg.tree_dir.join(".ferry").join("settings.json"),
        store.store_dir().join("settings.json"),
        cfg.store_dir.join("settings.json"),
        cfg.store_dir.join(".ferry").join("settings.json"),
    ];
    let mut ignore_config = ferry_ignore::IgnoreConfig::default();
    for path in &candidates {
        if path.is_file() {
            if let Ok(content) = std::fs::read_to_string(path) {
                if let Ok(settings) =
                    serde_json::from_str::<ferry_folder::folder::Settings>(&content)
                {
                    ignore_config = settings.ignore_config();
                    break;
                }
            }
        }
    }
    match ferry_ignore::FerryIgnore::new(&cfg.tree_dir, &ignore_config) {
        Ok(ignore) => Arc::new(ignore),
        Err(e) => {
            eprintln!(
                "warning: failed to compile FerryIgnore for {}: {e}",
                cfg.tree_dir.display()
            );
            Arc::new(ferry_scan::NoIgnores)
        }
    }
}

impl EngineConfig {
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
            allow_trust_on_first_use: false,
            pin_state_dir: None,
            quiet: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EngineStats {
    pub sessions_ok: u64,
    pub sessions_failed: u64,

    pub rejected_items: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("store: {0}")]
    Store(#[from] ferry_store::store::StoreError),
    #[error("scan: {0}")]
    Scan(#[from] ferry_scan::ScanError),
    #[error("{0}")]
    Other(String),
}

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

const FIRST_STATE_WAIT: Duration = Duration::from_secs(10);

const MAX_CONCURRENT_SESSIONS: usize = 4;

struct FolderState {
    inner: Mutex<FolderPointers>,
    changed: Condvar,
}

#[derive(Default)]
struct FolderPointers {
    latest: Option<Arc<SnapshotData>>,

    current: Option<Arc<SnapshotData>>,

    baseline: Option<RootManifest>,

    agreed: Option<BlobId>,

    last_own_manifest_id: BlobId,

    scan_stats: Option<ScanStats>,
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

    fn update_from_scan(&self, data: Arc<SnapshotData>, stats: ScanStats) {
        let mut g = self.lock();
        g.scan_stats = Some(stats);
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
    }

    fn adopt_peer(&self, data: Arc<SnapshotData>) {
        let id = data.manifest_id;
        let mut g = self.lock();
        g.latest = Some(Arc::clone(&data));
        g.current = Some(data);
        g.last_own_manifest_id = id;
        drop(g);
        self.changed.notify_all();
    }

    fn record_agreed(&self, manifest: RootManifest, manifest_id: BlobId) {
        let mut g = self.lock();
        g.baseline = Some(manifest);
        g.agreed = Some(manifest_id);
        drop(g);
        self.changed.notify_all();
    }

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

    fn wake_all(&self) {
        self.changed.notify_all();
    }

    fn wait_for_change(&self, timeout: Duration) -> bool {
        let guard = self.inner.lock().unwrap();
        let (_guard, res) = self.changed.wait_timeout(guard, timeout).unwrap();
        !res.timed_out()
    }
}

struct SharedState {
    shutdown: AtomicBool,
    stats: Mutex<EngineStats>,

    live_sessions: Mutex<usize>,
    live_idle: Condvar,

    free_permits: Mutex<usize>,

    park: Mutex<()>,
    park_cv: Condvar,
    peer_connectivity: Mutex<HashMap<BlobId, (Instant, &'static str)>>,
}

impl SharedState {
    fn new() -> Self {
        SharedState {
            shutdown: AtomicBool::new(false),
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

    fn register_live_session(self: &Arc<Self>) -> LiveSession {
        *self.live_sessions.lock().unwrap() += 1;
        LiveSession {
            shared: Arc::clone(self),
        }
    }

    fn wait_sessions_done(&self) {
        let mut g = self.live_sessions.lock().unwrap();
        while *g > 0 {
            g = self.live_idle.wait(g).unwrap();
        }
    }
}

struct SessionPermit {
    shared: Arc<SharedState>,
}

impl Drop for SessionPermit {
    fn drop(&mut self) {
        *self.shared.free_permits.lock().unwrap() += 1;
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Donor {
    First,

    Second,
}

fn lineage_winner(a: &RootManifest, b: &RootManifest) -> Donor {
    let ka = (a.created_sec, a.created_nsec, a.device_id, a.root_tree_id);
    let kb = (b.created_sec, b.created_nsec, b.device_id, b.root_tree_id);
    if ka >= kb {
        Donor::First
    } else {
        Donor::Second
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PeerState {
    pub current_root: BlobId,

    pub baseline_root: Option<BlobId>,
}

fn has_diverged(p: PeerState) -> bool {
    match p.baseline_root {
        None => p.current_root != crate::empty_tree_id(),
        Some(base) => base != p.current_root,
    }
}

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

    identity: DeviceIdentity,
    store: Arc<Store>,
    transport: Arc<dyn Transport>,
    session_lock: Mutex<()>,

    folder: Arc<FolderState>,
    shared: Arc<SharedState>,

    peer_policy: PeerPolicy,
    _scan_engine: Arc<ScanEngine>,
    dial_backoff: Mutex<(u32, Option<Instant>)>,
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

    fn current_snapshot(&self) -> Result<Arc<SnapshotData>, SessionError> {
        let deadline = Instant::now() + FIRST_STATE_WAIT;
        self.folder
            .wait_current(deadline)
            .ok_or_else(|| SessionError::Other("no local folder state available".into()))
    }

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

    fn handle_scan_update(&self, cur: &CurrentScan) {
        let manifest_bytes = serialize_manifest(&cur.manifest);
        let data = Arc::new(SnapshotData {
            manifest_id: cur.manifest_id,
            manifest: cur.manifest.clone(),
            manifest_bytes,
        });
        let stats = ScanStats {
            files: cur.stats.files,
            dirs: cur.stats.dirs,
            symlinks: cur.stats.symlinks,
            bytes_chunked: cur.stats.bytes_chunked,
        };
        self.folder.update_from_scan(data, stats);
        let current_manifest = self
            .folder
            .current_manifest_id()
            .unwrap_or(cur.root_tree_id);
        self.status(&format!(
            "STATE root={} agreed={}",
            hex(&current_manifest),
            self.folder.agreed_id().map_or("none".into(), |i| hex(&i))
        ));
    }

    fn dial_and_run(&self) {
        let Some(addr) = self.cfg.connect_to else {
            return;
        };
        {
            let guard = self.dial_backoff.lock().unwrap();
            if let Some(next) = guard.1 {
                if Instant::now() < next {
                    return;
                }
            }
        }
        match self.transport.dial(addr) {
            Ok(mut conn) => match run_session_v1(conn.as_mut(), self, true) {
                Ok(()) => {
                    let mut guard = self.dial_backoff.lock().unwrap();
                    *guard = (0, None);
                    self.bump_ok();
                }
                Err(e) => {
                    self.note_session_failure(&e);
                    self.status(&format!("SESSION failed (dial): {e}"));
                    self.record_dial_failure();
                }
            },
            Err(e) => {
                if let [peer] = self.current_policy().remote_peers(self.identity.public())[..] {
                    self.shared.record_peer_connectivity(peer, "unreachable");
                }
                self.status(&format!("SESSION dial error: {e}"));
                self.record_dial_failure();
            }
        }
    }

    fn record_dial_failure(&self) {
        let mut guard = self.dial_backoff.lock().unwrap();
        let failures = guard.0.saturating_add(1);
        let shift = failures.min(4);
        let base_ms = 50u64.max(self.cfg.poll_interval.as_millis() as u64);
        let millis = base_ms.saturating_mul(1 << shift).min(5_000);
        let next = Instant::now() + Duration::from_millis(millis);
        *guard = (failures, Some(next));
    }

    fn current_policy(&self) -> PeerPolicy {
        let refreshed = resolve_peer_policy_from_disk(&self.cfg, &self.store);
        if refreshed != PeerPolicy::default() || self.peer_policy == PeerPolicy::default() {
            refreshed
        } else {
            self.peer_policy.clone()
        }
    }

    fn dial_discovered_peers(&self) {
        let policy = self.current_policy();
        let peers = policy.remote_peers(self.identity.public());
        if peers.is_empty() {
            return;
        }
        {
            let guard = self.dial_backoff.lock().unwrap();
            if let Some(next) = guard.1 {
                if Instant::now() < next {
                    return;
                }
            }
        }
        let mut any_success = false;
        let mut attempted = false;
        for peer in peers {
            attempted = true;
            match self.transport.dial_peer(&peer) {
                Ok(mut conn) => match run_session_v1(conn.as_mut(), self, true) {
                    Ok(()) => {
                        any_success = true;
                        self.bump_ok();
                        self.shared.record_peer_connectivity(peer, "reachable");
                        break;
                    }
                    Err(e) => {
                        self.note_session_failure(&e);
                        self.status(&format!(
                            "SESSION failed (autonomous dial to {}): {e}",
                            hex_short(&peer)
                        ));
                        self.shared.record_peer_connectivity(peer, "unreachable");
                    }
                },
                Err(e) => {
                    self.shared.record_peer_connectivity(peer, "unreachable");
                    self.status(&format!(
                        "SESSION autonomous dial to {} error: {e}",
                        hex_short(&peer)
                    ));
                }
            }
        }
        if any_success {
            let mut guard = self.dial_backoff.lock().unwrap();
            *guard = (0, None);
        } else if attempted {
            self.record_dial_failure();
        }
    }

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

const MAX_ITEM_RETRIES: u32 = 3;

fn run_session_v1(conn: &mut dyn Connection, ctx: &Ctx, dialer: bool) -> Result<(), SessionError> {
    let role = if dialer {
        ferry_proto::Role::Initiator
    } else {
        ferry_proto::Role::Responder
    };

    let ledger = PeerLedger::new(ctx.store.store_dir());
    let policy = ctx.current_policy();
    let resolved = policy.expected_peer(ctx.identity.public(), &ctx.cfg.folder_id, &ledger)?;
    let expect = match resolved {
        PeerExpectation::Refuse => {
            ctx.status("PEER refused: allow-list names no paired peer");
            return Err(SessionError::PeerUnauthorized(
                "allow-list names no paired peer".into(),
            ));
        }
        PeerExpectation::Pin(id) => ExpectPeer::Pin(id),
        PeerExpectation::TrustOnFirstUse { .. } => ExpectPeer::TrustOnFirstUse,
    };

    let mut link = ConnLink(conn);
    let mut est: Established = session::establish(&mut link, role, &ctx.identity, expect, true)?;

    if !policy.admits(&est.peer) {
        let _ = est.io.send_bye(ferry_proto::error::ByeReason::AuthFailed);
        ctx.shared.record_peer_connectivity(est.peer, "unreachable");
        ctx.status(&format!(
            "PEER unauthorized: {} not in allow-list",
            hex(&est.peer)
        ));
        return Err(SessionError::PeerUnauthorized(hex(&est.peer)));
    }

    if let PeerExpectation::TrustOnFirstUse { pin: true } = resolved {
        ctx.status(&format!(
            "PEER new device trusted (TOFU): {}",
            hex(&est.peer)
        ));
        ledger.record_peer(&ctx.cfg.folder_id, &est.peer)?;
    }

    ctx.status(&format!(
        "SESSION v1 peer={} encrypted=yes version={} role={}",
        hex_short(&est.peer),
        est.agreed_version,
        if dialer { "initiator" } else { "responder" }
    ));

    if let Ok(run) = ctx._scan_engine.scan_once() {
        if let Some(pub_scan) = run.published {
            ctx.handle_scan_update(&pub_scan);
        }
    }

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
        let _ = ctx._scan_engine.scan_once();
        ctx.shared.record_peer_connectivity(est.peer, "reachable");
    } else {
        ctx.shared.record_peer_connectivity(est.peer, "unreachable");
    }
    res
}

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
        self.ctx._scan_engine.set_parent_manifest_id(id);
        self.ctx
            ._scan_engine
            .debug_inject_signal(WatchSignal::AuditDue);
        self.ctx.folder.adopt_peer(data);
        self.ctx.status(&format!(
            "STATE root={} adopted",
            hex(&manifest.root_tree_id)
        ));
        Ok(())
    }

    fn note_tree_mutation(&self) {
        self.ctx
            ._scan_engine
            .debug_inject_signal(WatchSignal::AuditDue);
    }

    fn agree(&self, peer: BlobId, bytes: &[u8], manifest_id: BlobId) -> Result<(), SessionError> {
        self.ctx._scan_engine.set_parent_manifest_id(manifest_id);
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

#[cfg_attr(not(test), allow(dead_code))]
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

    identity: Option<DeviceIdentity>,
    ignore_policy: Option<Arc<dyn ferry_scan::IgnorePolicy>>,
}

impl SyncEngine {
    pub fn with_store(
        cfg: EngineConfig,
        transport: Arc<dyn Transport>,
        store: Arc<Store>,
    ) -> Result<Self, EngineError> {
        std::fs::create_dir_all(&cfg.tree_dir)?;
        let stale = Duration::from_secs(ferry_materialize::DEFAULT_STALE_TEMP_AGE_SECS);
        let _ = ferry_materialize::sweep_stale_temps(&cfg.tree_dir, stale);
        let _ = ferry_store::reclaim::sweep_store_temps(&cfg.store_dir, stale);
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
            ignore_policy: None,
        })
    }

    pub fn set_peer_policy(&mut self, policy: PeerPolicy) {
        self.peer_policy = Some(policy);
    }

    pub fn set_ignore_policy(&mut self, policy: Arc<dyn ferry_scan::IgnorePolicy>) {
        self.ignore_policy = Some(policy);
    }

    pub fn with_ignore_policy(mut self, policy: Arc<dyn ferry_scan::IgnorePolicy>) -> Self {
        self.ignore_policy = Some(policy);
        self
    }

    pub fn set_identity(&mut self, identity: DeviceIdentity) {
        self.identity = Some(identity);
    }

    pub fn listen_addr(&self) -> Option<SocketAddr> {
        self.listener.as_ref().and_then(|l| l.local_addr().ok())
    }

    pub fn ingest_pack_bytes_for_test(
        store: &Store,
        claimed_name: &PackId,
        bytes: &[u8],
    ) -> Result<(), IngestError> {
        crate::exchange::ingest_pack_verified(store, claimed_name, bytes)
    }

    pub fn start(mut self) -> EngineHandle {
        let listener: Option<Arc<dyn crate::transport::Listener>> =
            self.listener.take().map(Arc::from);
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
                        folder.record_agreed(manifest.clone(), rec.manifest_id);
                        folder.adopt_peer(Arc::new(SnapshotData {
                            manifest_id: rec.manifest_id,
                            manifest_bytes: bytes,
                            manifest,
                        }));
                    }
                }
            }
        }
        let store_dir = self.store.store_dir().to_path_buf();
        let folder_id = self.cfg.folder_id;

        let device = self
            .identity
            .take()
            .unwrap_or_else(|| device_identity_for_tag(&self.cfg.tag));
        let peer_policy = if let Some(policy) = self.peer_policy.take() {
            policy
        } else {
            resolve_peer_policy_from_disk(&self.cfg, &self.store)
        };
        let ignore_policy = if let Some(policy) = self.ignore_policy.take() {
            policy
        } else {
            resolve_ignore_policy_from_disk(&self.cfg, &self.store)
        };

        let scan_cfg = ScanConfig {
            poll_interval: self.cfg.poll_interval,
            quiet_window: Duration::from_millis(50).min(self.cfg.poll_interval),
            parent_manifest_id: folder.agreed_id(),
            ..ScanConfig::default()
        };
        let scan_handle = StoreHandle {
            store: Arc::clone(&self.store),
            poly: self.cfg.poly,
            folder_id: self.cfg.folder_id,
            device_id: *device.device_id(),
        };
        let scan_engine = Arc::new(
            ScanEngine::watch_with(&self.cfg.tree_dir, scan_handle, scan_cfg, ignore_policy)
                .expect("start scan engine"),
        );
        if let Some(agreed_id) = folder.agreed_id() {
            scan_engine.set_parent_manifest_id(agreed_id);
        }
        let rx = scan_engine.subscribe();

        let ctx = Arc::new(Ctx {
            cfg: self.cfg.clone(),
            identity: device,
            store: Arc::clone(&self.store),
            transport: Arc::clone(&self.transport),
            session_lock: Mutex::new(()),
            folder: Arc::clone(&folder),
            shared: Arc::clone(&shared),
            peer_policy,
            _scan_engine: Arc::clone(&scan_engine),
            dial_backoff: Mutex::new((0, None)),
        });

        if let Some(cur) = scan_engine.current() {
            ctx.handle_scan_update(&cur);
        }

        let joins: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
        let loops: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));

        if let Some(ref listener) = listener {
            let listener = Arc::clone(listener);
            let ctx2 = Arc::clone(&ctx);
            let shared2 = Arc::clone(&shared);
            let joins2 = Arc::clone(&joins);
            loops.lock().unwrap().push(
                std::thread::Builder::new()
                    .name(format!("{}-accept", self.cfg.tag))
                    .spawn(move || accept_loop(listener, ctx2, shared2, joins2))
                    .expect("spawn accept loop"),
            );
        }

        {
            let ctx = Arc::clone(&ctx);
            let shared = Arc::clone(&shared);
            loops.lock().unwrap().push(
                std::thread::Builder::new()
                    .name(format!("{}-sync", self.cfg.tag))
                    .spawn(move || sync_loop(ctx, shared, rx))
                    .expect("spawn sync loop"),
            );
        }

        EngineHandle {
            shared,
            folder,
            joins,
            loops,
            listen_addr,
            listener,
            transport: Arc::clone(&self.transport),
            scan_engine,
            store_dir,
            folder_id,
            tag: self.cfg.tag,
        }
    }
    pub fn open_watched_folder(
        folder_path: &std::path::Path,
        identity: &ferry_crypto::identity::DeviceIdentity,
        transport: std::sync::Arc<dyn crate::transport::Transport>,
        bind_addr: Option<std::net::SocketAddr>,
        connect_to: Option<std::net::SocketAddr>,
        poll_interval: Option<std::time::Duration>,
        opportunistic_every: Option<u32>,
    ) -> Result<
        (
            EngineHandle,
            ferry_folder::inventory::FolderRecord,
            [u8; 16],
        ),
        EngineError,
    > {
        let opened = ferry_folder::folder::open_folder(folder_path, identity)
            .map_err(|e| EngineError::Other(format!("{}: {}", e.code, e.message)))?;
        let poly = ferry_store::chunker::ValidatedPoly::try_from(opened.poly)
            .map_err(|e| EngineError::Other(format!("poly-invalid: invalid poly: {e}")))?;
        let folder_id_hex = ferry_store::format::hex(&opened.folder_id);
        let tag = format!("ferry-{}", &folder_id_hex[..8.min(folder_id_hex.len())]);
        let cfg = EngineConfig {
            tag,
            store_dir: opened.root.clone(),
            tree_dir: opened.root.clone(),
            poly,
            folder_id: opened.folder_id,
            poll_interval: poll_interval.unwrap_or(std::time::Duration::from_millis(200)),
            opportunistic_every: opportunistic_every.unwrap_or(50),
            bind_addr,
            connect_to,
            allow_trust_on_first_use: false,
            pin_state_dir: Some(opened.state_dir()),
            quiet: true,
        };
        let mut engine =
            SyncEngine::with_store(cfg, transport, std::sync::Arc::clone(&opened.store))?;
        engine.set_identity(identity.clone());
        engine.set_ignore_policy(opened.ignore_policy());
        let handle = engine.start();
        let record = ferry_folder::inventory::FolderRecord {
            folder_id: folder_id_hex,
            path: opened.root,
            added_at: String::new(),
        };
        Ok((handle, record, opened.folder_id))
    }
}

pub fn device_identity_for_tag(tag: &str) -> DeviceIdentity {
    use blake3::Hasher;
    use rand::SeedableRng;
    let mut h = Hasher::new();
    h.update(b"ferry/v0/device-key:");
    h.update(tag.as_bytes());
    let digest = h.finalize();
    let mut seed = [0u8; 32];

    let mut rng = StdRng::from_seed(*digest.as_bytes());
    use rand::RngCore;
    rng.fill_bytes(&mut seed);
    DeviceIdentity::from_secret_bytes(&seed)
}

fn accept_loop(
    listener: Arc<dyn crate::transport::Listener>,
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

                let Some(permit) = shared.acquire_permit() else {
                    ctx.status("SESSION refused: engine busy");
                    drop(conn);
                    continue;
                };

                let live = shared.register_live_session();
                let ctx = Arc::clone(&ctx);
                let h = std::thread::Builder::new()
                    .name(format!("{}-session", ctx.cfg.tag))
                    .spawn(move || {
                        let _live = live;
                        let _permit = permit;

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

fn sync_loop(ctx: Arc<Ctx>, shared: Arc<SharedState>, rx: std::sync::mpsc::Receiver<ScanEvent>) {
    let backstop_interval = ctx
        .cfg
        .poll_interval
        .saturating_mul(ctx.cfg.opportunistic_every);
    let mut last_backstop = Instant::now();

    while !shared.shutting_down() {
        let elapsed = last_backstop.elapsed();
        let remaining = backstop_interval.saturating_sub(elapsed);
        let wait_time = remaining.min(Duration::from_millis(50));

        match rx.recv_timeout(wait_time) {
            Ok(ScanEvent::Updated(cur)) => {
                ctx.handle_scan_update(&cur);
            }
            Ok(ScanEvent::Failed(err)) => {
                ctx.status(&format!("SCAN error: {err}"));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                if shared.shutting_down() {
                    return;
                }
                ctx.status("SCAN channel disconnected");
                return;
            }
        }

        let has_explicit = ctx.cfg.connect_to.is_some();
        let has_peers = !ctx
            .current_policy()
            .remote_peers(ctx.identity.public())
            .is_empty();
        if has_explicit || has_peers {
            let diverged = ctx.folder.baseline_root() != ctx.folder.current_root();
            let backstop_due = last_backstop.elapsed() >= backstop_interval;
            if diverged || backstop_due {
                if backstop_due {
                    last_backstop = Instant::now();
                }
                if let Ok(_guard) = ctx.session_lock.try_lock() {
                    if has_explicit {
                        ctx.dial_and_run();
                    }
                    if has_peers {
                        ctx.dial_discovered_peers();
                    }
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct EngineHandle {
    shared: Arc<SharedState>,
    folder: Arc<FolderState>,
    joins: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>,

    loops: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>,
    listen_addr: Option<SocketAddr>,
    listener: Option<Arc<dyn crate::transport::Listener>>,
    transport: Arc<dyn Transport>,
    scan_engine: Arc<ScanEngine>,
    store_dir: PathBuf,
    folder_id: [u8; 16],
    tag: String,
}

impl EngineHandle {
    pub fn tag(&self) -> &str {
        &self.tag
    }

    pub fn is_healthy(&self) -> bool {
        !self.shared.shutting_down() && self.loops.lock().unwrap().iter().all(|j| !j.is_finished())
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

    pub fn wait_for_change(&self, timeout: Duration) -> bool {
        self.folder.wait_for_change(timeout)
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

    pub fn trigger_scan(&self) {
        self.scan_engine.debug_inject_signal(WatchSignal::AuditDue);
        let _ = self.scan_engine.scan_once();
    }

    pub fn shutdown(&self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        self.scan_engine.stop();

        self.folder.wake_all();
        self.shared.wake_parked();
        if let Some(ref l) = self.listener {
            let _ = l.close();
        }

        if let Some(addr) = self.listen_addr {
            let _ = self.transport.dial(addr);
        }
        while let Some(j) = self.joins.lock().unwrap().pop() {
            let _ = j.join();
        }
        while let Some(j) = self.loops.lock().unwrap().pop() {
            let _ = j.join();
        }

        self.shared.wait_sessions_done();

        while let Some(j) = self.joins.lock().unwrap().pop() {
            let _ = j.join();
        }
    }

    pub fn join_until_signal(&self) {
        let mut guard = self.shared.park.lock().unwrap();
        while !self.shared.shutting_down() {
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

    fn test_store(store_dir: &Path, tag: &str) -> Arc<Store> {
        ferry_folder::open_or_create_test_store(store_dir, &device_identity_for_tag(tag))
            .expect("test store")
    }

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
        let fresh = manifest_at(999, [9; 32], empty);
        let populated = manifest_at(1, [1; 32], full);
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

    fn fake_scan(
        sec: i64,
        dev: [u8; 32],
        root: BlobId,
        parent: BlobId,
    ) -> (Arc<SnapshotData>, ScanStats) {
        let m = RootManifest {
            folder_id: crate::DEFAULT_FOLDER_ID,
            device_id: dev,
            created_sec: sec,
            created_nsec: 0,
            root_tree_id: root,
            parent_manifest_id: parent,
        };
        let bytes = serialize_manifest(&m);
        let manifest_id = *blake3::hash(&bytes).as_bytes();
        let data = Arc::new(SnapshotData {
            manifest_id,
            manifest: m,
            manifest_bytes: bytes,
        });
        let stats = ScanStats {
            files: 0,
            dirs: 0,
            symlinks: 0,
            bytes_chunked: 0,
        };
        (data, stats)
    }

    #[test]
    fn adoption_updates_folder_state() {
        let me = [7u8; 32];
        let peer = [9u8; 32];
        let (scan_a, stats_a) = fake_scan(10, me, [1; 32], [0; 32]);
        let (adopted, _) = fake_scan(20, peer, [2; 32], [0; 32]);
        let adopted_id = adopted.manifest_id;

        let folder = Arc::new(FolderState::new());
        folder.update_from_scan(scan_a, stats_a);

        folder.adopt_peer(Arc::clone(&adopted));

        {
            let g = folder.lock();
            assert_eq!(
                g.current.as_ref().map(|s| s.manifest_id),
                Some(adopted_id),
                "adoption updates current pointer"
            );
            assert_eq!(
                g.last_own_manifest_id, adopted_id,
                "next self-mint continues the ADOPTED lineage"
            );
            assert_eq!(
                g.latest.as_ref().map(|s| s.manifest_id),
                Some(adopted_id),
                "latest pointer updated on adoption"
            );
        }

        let (fresh, stats_fresh) = fake_scan(40, me, [4; 32], [0; 32]);
        folder.update_from_scan(fresh, stats_fresh);
        {
            let g = folder.lock();
            assert_ne!(
                g.current.as_ref().map(|s| s.manifest_id),
                Some(adopted_id),
                "a fresh scan on changed root mints new pointer"
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
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let store = test_store(&root, "t20-sweep");
        drop(store);
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
        let _engine = SyncEngine::with_store(
            cfg,
            Arc::new(crate::transport::TcpTransport),
            test_store(&root, "t20-sweep"),
        )
        .unwrap();

        for p in &stale {
            assert!(!p.exists(), "startup must sweep {p:?}");
        }
        for p in &fresh {
            assert!(p.exists(), "startup must never touch {p:?}");
        }
    }

    #[test]
    fn rescan_of_unchanged_tree_holds_the_current_pointer() {
        let me = [3u8; 32];
        let root = [5; 32];
        let (first, stats_1) = fake_scan(1, me, root, [0; 32]);
        let (again, stats_2) = fake_scan(2, me, root, [0; 32]);
        let first_id = first.manifest_id;
        let second_id = again.manifest_id;

        let folder = Arc::new(FolderState::new());
        folder.update_from_scan(first, stats_1);
        assert_eq!(
            folder.lock().current.as_ref().map(|s| s.manifest_id),
            Some(first_id)
        );

        folder.update_from_scan(again, stats_2);
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
        let (own, stats_own) = fake_scan(1, me, [1; 32], [0; 32]);
        folder.update_from_scan(own, stats_own);

        let before = folder
            .wait_current(Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert_eq!(before.manifest.root_tree_id, [1; 32]);

        let (peer_snap, _) = fake_scan(9, peer, [2; 32], [0; 32]);
        folder.adopt_peer(Arc::clone(&peer_snap));
        folder.record_agreed(peer_snap.manifest.clone(), peer_snap.manifest_id);

        let after = folder
            .wait_current(Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert_eq!(after.manifest_id, peer_snap.manifest_id);
        assert_eq!(after.manifest.root_tree_id, [2; 32]);

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

        let live = shared.register_live_session();
        drop(live);
        shared.wait_sessions_done();
    }

    #[test]
    fn shutdown_joins_everything_and_stops_all_writes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("engine");
        let mut cfg = EngineConfig::default_for_test(23);
        cfg.tag = "t07-shutdown".into();
        cfg.store_dir = root.join("store");
        cfg.tree_dir = root.join("tree");
        cfg.poll_interval = Duration::from_millis(15);
        cfg.bind_addr = Some("127.0.0.1:0".parse().unwrap());
        cfg.connect_to = None;

        let engine = SyncEngine::with_store(
            cfg,
            Arc::new(crate::transport::TcpTransport),
            test_store(&root.join("store"), "t07-shutdown"),
        )
        .expect("engine");
        let handle = engine.start();

        std::thread::sleep(Duration::from_millis(150));
        let started = Instant::now();
        handle.shutdown();
        let joined = started.elapsed();
        assert!(
            joined < Duration::from_secs(5),
            "shutdown must join promptly, took {joined:?}"
        );
        assert!(!handle.is_healthy());
    }

    #[test]
    fn peer_ledger_records_lists_and_forgets_peers() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = PeerLedger::new(dir.path());
        let folder1 = [1u8; 16];
        let folder2 = [2u8; 16];
        let peer_a = [10u8; 32];
        let peer_b = [20u8; 32];

        assert_eq!(ledger.list_peers(&folder1).unwrap(), Vec::<BlobId>::new());

        ledger.record_peer(&folder1, &peer_a).unwrap();
        assert_eq!(ledger.list_peers(&folder1).unwrap(), vec![peer_a]);

        assert_eq!(ledger.list_peers(&folder2).unwrap(), Vec::<BlobId>::new());

        ledger.record_peer(&folder1, &peer_b).unwrap();
        assert_eq!(ledger.list_peers(&folder1).unwrap(), vec![peer_a, peer_b]);

        assert!(ledger.forget_peer(&folder1, &peer_a).unwrap());
        assert_eq!(ledger.list_peers(&folder1).unwrap(), vec![peer_b]);

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

        let engine = SyncEngine::with_store(
            cfg,
            Arc::new(crate::transport::TcpTransport),
            test_store(&dir.path().join("store"), "t-scan-counts"),
        )
        .unwrap();
        let handle = engine.start();

        let deadline = Instant::now() + Duration::from_secs(5);
        while handle.scan_counts().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }

        let counts = handle
            .scan_counts()
            .expect("scan counts should be available after tick");
        assert_eq!(counts.files, 1);
        assert_eq!(counts.dirs, 0);

        assert_eq!(handle.pending_changes(), None);

        let peer_dev = [99u8; 32];
        assert_eq!(handle.peer_connectivity(&peer_dev), "unknown");

        handle.record_peer_connectivity(peer_dev, "reachable");
        assert_eq!(handle.peer_connectivity(&peer_dev), "reachable");

        handle.shutdown();
    }

    #[test]
    fn engine_handle_is_healthy_detects_crash_and_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let store_dir = dir.path().join("store");
        let tree_dir = dir.path().join("tree");
        std::fs::create_dir_all(&tree_dir).unwrap();

        let mut cfg = EngineConfig::default_for_test(54321);
        cfg.store_dir = store_dir;
        cfg.tree_dir = tree_dir;
        cfg.poll_interval = Duration::from_millis(20);
        cfg.bind_addr = None;
        cfg.connect_to = None;

        let engine = SyncEngine::with_store(
            cfg,
            Arc::new(crate::transport::TcpTransport),
            test_store(&dir.path().join("store"), "t-healthy-crash"),
        )
        .unwrap();
        let handle = engine.start();

        assert!(
            handle.is_healthy(),
            "handle must report healthy while running"
        );
        handle.shutdown();
        assert!(
            !handle.is_healthy(),
            "handle remains unhealthy after shutdown"
        );
    }
}
