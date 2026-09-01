use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use ferry_crypto::identity::DeviceIdentity;
use ferry_folder::folder::{dot_dir, load_rules, open_folder};
use ferry_folder::inventory::{validate_path, ListDirectoryResponse};
use ferry_folder::pairing::{PairingRitual, GRANT_SUFFIX, OFFER_SUFFIX, RESPONSE_SUFFIX};
pub use ferry_ipc::backend::BoxFuture;
use ferry_ipc::backend::{
    InventoryDomain, OpError, PairResult, PinRecord, PinReleaseSummary, PinStopSummary,
    SessionDomain, ShareOffer, ShareStatus, StatusDomain, UiBackend, UiEvent, UiEventStream,
};
use ferry_ipc::protocol::{
    ConflictEntry, DeviceStamp, EngineSnapshot, PeerStatusView, PinView, ScanStatsView,
};
use ferry_store::agreement::AgreementLedger;
use ferry_store::format::hex as hex_str;
use ferry_sync_engine::pin::{PinError, PinManager};
use serde_json::{json, Value};

use super::UiState;

pub type DashboardBackend = dyn UiBackend;

#[must_use]
pub fn snapshot_to_status_doc(snap: &EngineSnapshot) -> Value {
    let mut held_by_peer_val = serde_json::Map::new();
    for (peer, paths) in &snap.held_by_peer {
        held_by_peer_val.insert(peer.clone(), json!(paths));
    }
    json!({
        "command": "status",
        "folder": snap.folder,
        "folder_id": snap.folder_id,
        "device_id": snap.device_id,
        "manifest_id": snap.manifest_id.clone().unwrap_or_default(),
        "scanned": {
            "files": snap.scanned.files,
            "dirs": snap.scanned.dirs,
            "symlinks": snap.scanned.symlinks,
            "bytes_chunked": snap.scanned.bytes_chunked,
        },
        "pending_changes": snap.pending_changes,
        "pin": {
            "state": snap.pin.state,
            "holding": snap.pin.holding,
            "paths": snap.pin.paths,
        },
        "held_changes": snap.held_changes,
        "held_by_peer": Value::Object(held_by_peer_val),
        "peers": snap.peers.iter().map(|p| json!({
            "device_id": p.device_id,
            "last_agreed_manifest_id": p.last_agreed_manifest_id,
            "agreed_at": p.agreed_at,
            "connectivity": p.connectivity,
        })).collect::<Vec<_>>(),
        "discovered_devices": snap.discovered_devices.iter().map(|d| json!({
            "device_id": d.device_id,
            "address": d.address,
            "status": d.status,
        })).collect::<Vec<_>>(),
        "conflicts": snap.conflicts,
    })
}

fn pin_err(e: PinError) -> OpError {
    match e {
        PinError::PinActive { pid } => OpError::new(
            "pin-active",
            format!("a pinned session (pid {pid}) already holds this folder"),
            "stop or release it first",
        ),
        PinError::Corrupt { path, reason } => OpError::new(
            "pin-state-corrupt",
            format!("{}: {reason}", path.display()),
            "fix or delete .ferry/pin-state.json",
        ),
        PinError::LedgerCorrupt { path, line, reason } => OpError::new(
            "held-ledger-corrupt",
            format!("{} near line {line}: {reason}", path.display()),
            "run `ferry pin status` for detail",
        ),
        PinError::Io { source, .. } => OpError::new("io", source.to_string(), "check permissions"),
        other => OpError::new("internal", other.to_string(), "check stderr log"),
    }
}

fn folder_err(e: ferry_folder::FolderError) -> OpError {
    OpError::new(e.code, e.message, e.hint)
}

fn pairing_ritual(home: PathBuf, identity: DeviceIdentity) -> PairingRitual {
    PairingRitual::with_shared(home, identity, ferry_folder::pairing::shared_rendezvous())
}

fn expires_rfc3339(t: std::time::SystemTime) -> String {
    let secs = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_else(|_| {
        std::time::Duration::from_secs(ferry_platform::time::now_unix().0 as u64)
    });
    ferry_platform::time::fmt_rfc3339(secs.as_secs() as i64)
}

fn log_err(e: ferry_sync_engine::LogError) -> OpError {
    match e {
        ferry_sync_engine::LogError::Corrupt { path, reason, .. } => OpError::new(
            "conflict-log",
            format!("{}: {reason}", path.display()),
            "fix or archive .ferry/conflicts.jsonl",
        ),
        ferry_sync_engine::LogError::Io { source, .. } => {
            OpError::new("io", source.to_string(), "check permissions")
        }
    }
}

fn ferry_home_for_backend() -> PathBuf {
    ferry_folder::inventory::ferry_home()
}

fn backend_inventory() -> ferry_folder::inventory::FolderInventory {
    ferry_folder::inventory::FolderInventory::new(&ferry_folder::inventory::ferry_home())
}

fn resolve_identity(identity: Option<&DeviceIdentity>) -> DeviceIdentity {
    if let Some(id) = identity {
        return id.clone();
    }
    let home = if let Some(v) = std::env::var_os("FERRY_HOME") {
        let p = PathBuf::from(&v);
        if p.as_os_str().is_empty() {
            let h = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map_or_else(|| PathBuf::from("."), PathBuf::from);
            h.join(".ferry")
        } else {
            p
        }
    } else {
        let h = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map_or_else(|| PathBuf::from("."), PathBuf::from);
        h.join(".ferry")
    };
    ferry_crypto::identity::load_or_create(&home.join("identity"))
        .unwrap_or_else(|_| DeviceIdentity::generate())
}

fn share_initiate_blocking(
    root: PathBuf,
    identity: DeviceIdentity,
    i_know: bool,
) -> Result<ShareOffer, OpError> {
    let opened = open_folder(&root, &identity).map_err(folder_err)?;
    let rules = load_rules(&opened.root, &opened.settings).map_err(folder_err)?;
    let warnings_raw = ferry_ignore::secrets::scan_for_secrets(&rules, &opened.root);
    let mut secret_warnings = Vec::new();
    for w in &warnings_raw {
        secret_warnings.push(format!(
            "{}: line {:?} [{}]",
            w.path.join("/"),
            w.line,
            w.class.label()
        ));
    }
    if !warnings_raw.is_empty() && !i_know {
        let warnings_val: Vec<Value> = warnings_raw
            .iter()
            .map(|w| {
                json!({
                    "path": w.path.join("/"),
                    "line": w.line,
                    "class": w.class.label(),
                    "preview": w.preview,
                })
            })
            .collect();
        return Err(OpError::new(
            "secrets-found",
            format!("{} secret risk(s) detected", warnings_raw.len()),
            "exclude paths or pass i_know to bypass",
        )
        .with_detail(json!({ "warnings": warnings_val })));
    }
    let ritual = pairing_ritual(ferry_home_for_backend(), identity.clone());
    let pending = ritual.create_offer(&opened).map_err(folder_err)?;
    let dot = dot_dir(&opened.root);
    let _ = std::fs::remove_file(dot.join(RESPONSE_SUFFIX));
    let _ = std::fs::remove_file(dot.join(GRANT_SUFFIX));
    pending.write_payload().map_err(folder_err)?;
    let qr = pending.qr_payload();
    Ok(ShareOffer {
        folder: opened.root.display().to_string(),
        token: pending.short_code.clone(),
        payload_path: Some(pending.payload_path),
        qr_payload: Some(qr),
        expires_at: Some(expires_rfc3339(pending.expires_at)),
        secret_warnings,
    })
}

fn share_status_blocking(root: PathBuf, identity: DeviceIdentity) -> Result<ShareStatus, OpError> {
    let opened = open_folder(&root, &identity).map_err(folder_err)?;
    let dot = dot_dir(&opened.root);
    let offer_path = dot.join(OFFER_SUFFIX);
    let response_path = dot.join(RESPONSE_SUFFIX);
    if !offer_path.exists() {
        return Ok(ShareStatus {
            folder: opened.root.display().to_string(),
            status: "none".to_string(),
            active: false,
            peer_device_id: None,
            offer: None,
        });
    }
    let bytes = std::fs::read(&offer_path)
        .map_err(|e| OpError::new("io", e.to_string(), "cannot read offer"))?;
    let envelope = ferry_folder::pairing::parse_payload_envelope(&String::from_utf8_lossy(&bytes))
        .ok_or_else(|| {
            OpError::new(
                "bad-offer",
                "the offer file is not a FERRY1 pairing envelope",
                "re-run share to mint a fresh offer",
            )
        })?;
    let short_code = Some(envelope.code);
    let offer = Some(ShareOffer {
        folder: opened.root.display().to_string(),
        token: short_code.clone().unwrap_or_default(),
        payload_path: Some(offer_path),
        qr_payload: short_code,
        expires_at: None,
        secret_warnings: Vec::new(),
    });
    if !response_path.exists() {
        return Ok(ShareStatus {
            folder: opened.root.display().to_string(),
            status: "pending".to_string(),
            active: true,
            peer_device_id: None,
            offer,
        });
    }
    let ritual = pairing_ritual(ferry_home_for_backend(), identity);
    match ritual.poll_offer(&opened).map_err(folder_err)? {
        Some(completed) => Ok(ShareStatus {
            folder: opened.root.display().to_string(),
            status: "completed".to_string(),
            active: false,
            peer_device_id: Some(hex_str(&completed.peer_device_id)),
            offer,
        }),
        None => Ok(ShareStatus {
            folder: opened.root.display().to_string(),
            status: "pending".to_string(),
            active: true,
            peer_device_id: None,
            offer,
        }),
    }
}

fn pair_accept_blocking(
    code_or_payload: String,
    dir: Option<PathBuf>,
    identity: DeviceIdentity,
) -> Result<PairResult, OpError> {
    let ritual = pairing_ritual(ferry_home_for_backend(), identity.clone());
    let pending = ritual
        .accept_offer(&code_or_payload, dir.as_deref())
        .map_err(folder_err)?;
    let accepted = pending.complete(120).map_err(folder_err)?;
    Ok(PairResult {
        folder_id: hex_str(&accepted.folder_id),
        device_id: hex_str(identity.public()),
        folder_path: accepted.folder,
        status: "completed".to_string(),
        message: Some("pairing completed successfully".to_string()),
    })
}

fn pin_start_blocking(
    state_dir: PathBuf,
    folder: PathBuf,
    paths: Vec<String>,
    identity: &DeviceIdentity,
    folder_id: [u8; 16],
) -> Result<PinRecord, OpError> {
    let mut base_agreements = BTreeMap::new();
    for (dev, rec) in AgreementLedger::new(&state_dir)
        .list_folder(&folder_id)
        .unwrap_or_default()
    {
        base_agreements.insert(hex_str(&dev), hex_str(&rec.manifest_id));
    }
    let mgr = PinManager::new(&state_dir);
    let pid = std::process::id();
    let dev_hex = hex_str(identity.public());
    let record = mgr
        .start_session(paths, pid, &dev_hex, base_agreements)
        .map_err(pin_err)?;
    Ok(PinRecord {
        folder: folder.display().to_string(),
        paths: record.paths,
        status: "active".to_string(),
        expires_at: None,
        message: Some("session pin active".to_string()),
    })
}

pub trait StateSource: Send + Sync + 'static {
    fn open_folder(
        &self,
        path: Option<PathBuf>,
    ) -> Result<ferry_folder::folder::OpenFolder, OpError>;
    fn list_folder(
        &self,
        path: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<ListDirectoryResponse, OpError>>;
    fn pin_state(&self) -> Result<ferry_sync_engine::pin::HeldSummary, OpError>;
    fn event_stream(&self) -> BoxFuture<'_, Result<UiEventStream, OpError>>;

    fn folder_root(&self) -> PathBuf;
    fn identity(&self) -> DeviceIdentity;
    fn state_dir(&self) -> PathBuf;
    fn folder_id(&self) -> Option<[u8; 16]>;
    fn device_hex(&self) -> String;

    fn snapshot(&self) -> BoxFuture<'_, Result<EngineSnapshot, OpError>>;
    fn trigger_scan(&self) -> BoxFuture<'_, Result<(), OpError>>;
    fn list_folders(&self) -> BoxFuture<'_, Result<Vec<ferry_ipc::FolderRecord>, OpError>>;
    fn register_folder(
        &self,
        path: PathBuf,
    ) -> BoxFuture<'_, Result<ferry_ipc::FolderRecord, OpError>>;
    fn remove_folder(&self, folder_id: String) -> BoxFuture<'_, Result<(), OpError>>;
    fn create_pairing_session(
        &self,
        req: ferry_ipc::pairing::CreatePairingRequest,
    ) -> BoxFuture<'_, Result<ferry_ipc::pairing::CreatePairingResponse, OpError>>;
    fn join_pairing_session(
        &self,
        req: ferry_ipc::pairing::JoinPairingRequest,
    ) -> BoxFuture<'_, Result<PairResult, OpError>>;
}

#[derive(Clone, Debug)]
pub struct FsStateSource {
    folder: PathBuf,
    identity: Option<DeviceIdentity>,
    event_tx: tokio::sync::broadcast::Sender<UiEvent>,
    pairing_store: ferry_folder::pairing::SharedRendezvous,
}

impl FsStateSource {
    #[must_use]
    pub fn new(folder: impl Into<PathBuf>) -> Self {
        let (event_tx, _) = tokio::sync::broadcast::channel(64);
        Self {
            folder: folder.into(),
            identity: None,
            event_tx,
            pairing_store: ferry_folder::pairing::shared_rendezvous(),
        }
    }
    #[must_use]
    pub fn with_identity(mut self, identity: DeviceIdentity) -> Self {
        self.identity = Some(identity);
        self
    }
    #[must_use]
    pub fn with_fallback(mut self, dir: PathBuf) -> Self {
        self.folder = dir;
        self
    }
    fn resolved_identity(&self) -> DeviceIdentity {
        resolve_identity(self.identity.as_ref())
    }
}

impl StateSource for FsStateSource {
    fn open_folder(
        &self,
        path: Option<PathBuf>,
    ) -> Result<ferry_folder::folder::OpenFolder, OpError> {
        let root = path.unwrap_or_else(|| self.folder.clone());
        let id = self.resolved_identity();
        open_folder(&root, &id).map_err(folder_err)
    }
    fn list_folder(
        &self,
        path: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<ListDirectoryResponse, OpError>> {
        Box::pin(async move {
            let validated = validate_path(path)?;
            let validated_clone = validated.clone();
            tokio::task::spawn_blocking(move || {
                backend_inventory()
                    .inspect_dir(Some(validated_clone))
                    .map_err(OpError::from)
            })
            .await
            .map_err(|e| OpError::new("internal", e.to_string(), "check worker stderr"))?
        })
    }
    fn pin_state(&self) -> Result<ferry_sync_engine::pin::HeldSummary, OpError> {
        let opened = self
            .open_folder(None)
            .map_or_else(|_| dot_dir(&self.folder), |o| dot_dir(&o.root));
        PinManager::new(&opened).summary().map_err(pin_err)
    }
    fn event_stream(&self) -> BoxFuture<'_, Result<UiEventStream, OpError>> {
        let rx = self.event_tx.subscribe();
        Box::pin(async move { Ok(UiEventStream::new(rx)) })
    }
    fn folder_root(&self) -> PathBuf {
        self.folder.clone()
    }
    fn identity(&self) -> DeviceIdentity {
        self.resolved_identity()
    }
    fn state_dir(&self) -> PathBuf {
        dot_dir(&self.folder)
    }
    fn folder_id(&self) -> Option<[u8; 16]> {
        self.open_folder(None).ok().map(|o| o.folder_id)
    }
    fn device_hex(&self) -> String {
        hex_str(self.identity().public())
    }
    fn snapshot(&self) -> BoxFuture<'_, Result<EngineSnapshot, OpError>> {
        let folder = self.folder.clone();
        let identity = self.resolved_identity();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let opened = open_folder(&folder, &identity).map_err(folder_err)?;
                let state_dir = dot_dir(&opened.root);
                let rules = load_rules(&opened.root, &opened.settings).map_err(folder_err)?;
                let poly =
                    ferry_store::chunker::ValidatedPoly::try_from(opened.poly).map_err(|e| {
                        OpError::new(
                            "poly-error",
                            e.to_string(),
                            "the folder polynomial record is corrupt",
                        )
                    })?;
                let handle = ferry_scan::StoreHandle {
                    store: opened.store.clone(),
                    poly,
                    folder_id: opened.folder_id,
                    device_id: *identity.public(),
                };
                let engine = ferry_scan::ScanEngine::watch_with(
                    &opened.root,
                    handle,
                    ferry_scan::ScanConfig::default(),
                    Arc::new(rules),
                )
                .map_err(|e| OpError::new("scan-error", e.to_string(), "check directory"))?;
                let (manifest_id, stats) = if let Some(current) = engine.current() {
                    (
                        Some(hex_str(&current.manifest_id)),
                        ScanStatsView::new(
                            current.stats.files as u64,
                            current.stats.dirs as u64,
                            current.stats.symlinks as u64,
                            current.stats.bytes_chunked,
                        ),
                    )
                } else {
                    (None, ScanStatsView::default())
                };
                let records = AgreementLedger::new(&state_dir)
                    .list_folder(&opened.folder_id)
                    .unwrap_or_default();
                let mut peers = Vec::new();
                for (dev, rec) in records {
                    let mut p = PeerStatusView::new(hex_str(&dev), "offline");
                    p.last_agreed_manifest_id = Some(hex_str(&rec.manifest_id));
                    p.agreed_at = Some(ferry_platform::time::fmt_rfc3339(rec.agreed_sec));
                    peers.push(p);
                }
                peers.sort_by(|a, b| a.device_id.cmp(&b.device_id));
                let pin_summary = PinManager::new(&state_dir).summary().map_err(pin_err)?;
                let conflicts = ferry_sync_engine::list_conflicts(&state_dir)
                    .map_err(log_err)?
                    .len();
                let mut snap = EngineSnapshot::new(
                    opened.root.display().to_string(),
                    hex_str(&opened.folder_id),
                    hex_str(identity.public()),
                    "idle",
                );
                snap.manifest_id = manifest_id;
                snap.scanned = stats;
                snap.pin = if pin_summary.holding {
                    PinView::active(pin_summary.paths)
                } else {
                    PinView::none()
                };
                snap.held_changes = pin_summary.total_held_paths;
                snap.held_by_peer = pin_summary.held_by_peer.into_iter().collect();
                snap.peers = peers;
                snap.conflicts = conflicts;
                Ok(snap)
            })
            .await
            .map_err(|e| OpError::new("internal", e.to_string(), "check worker stderr"))?
        })
    }
    fn trigger_scan(&self) -> BoxFuture<'_, Result<(), OpError>> {
        let folder = self.folder.clone();
        let identity = self.resolved_identity();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let opened = open_folder(&folder, &identity).map_err(folder_err)?;
                let rules = load_rules(&opened.root, &opened.settings).map_err(folder_err)?;
                let poly = ferry_store::chunker::ValidatedPoly::try_from(opened.poly)
                    .map_err(|e| OpError::new("poly-error", e.to_string(), "corrupt poly"))?;
                let handle = ferry_scan::StoreHandle {
                    store: opened.store.clone(),
                    poly,
                    folder_id: opened.folder_id,
                    device_id: *identity.public(),
                };
                let _ = ferry_scan::ScanEngine::watch_with(
                    &opened.root,
                    handle,
                    ferry_scan::ScanConfig::default(),
                    Arc::new(rules),
                )
                .map_err(|e| OpError::new("scan-error", e.to_string(), "check directory"))?;
                Ok(())
            })
            .await
            .map_err(|e| OpError::new("internal", e.to_string(), "check worker stderr"))?
        })
    }
    fn list_folders(&self) -> BoxFuture<'_, Result<Vec<ferry_ipc::FolderRecord>, OpError>> {
        Box::pin(async move { backend_inventory().list().map_err(OpError::from) })
    }
    fn register_folder(
        &self,
        path: PathBuf,
    ) -> BoxFuture<'_, Result<ferry_ipc::FolderRecord, OpError>> {
        Box::pin(async move { backend_inventory().register(&path).map_err(OpError::from) })
    }
    fn remove_folder(&self, folder_id: String) -> BoxFuture<'_, Result<(), OpError>> {
        Box::pin(async move {
            backend_inventory()
                .unregister(&folder_id)
                .map_err(OpError::from)
        })
    }
    fn create_pairing_session(
        &self,
        req: ferry_ipc::pairing::CreatePairingRequest,
    ) -> BoxFuture<'_, Result<ferry_ipc::pairing::CreatePairingResponse, OpError>> {
        let home = ferry_home_for_backend();
        let identity = self.resolved_identity();
        let folder = self.folder.clone();
        let store = self.pairing_store.clone();
        Box::pin(async move {
            let ritual = PairingRitual::with_shared(home, identity, store);
            ritual.register_folder_path(req.folder_id.clone(), folder);
            tokio::task::spawn_blocking(move || {
                ritual
                    .create_offer_for_folder(&req.folder_id)
                    .map(|pending| {
                        ferry_ipc::pairing::CreatePairingResponse::new(
                            pending.short_code,
                            expires_rfc3339(pending.expires_at),
                        )
                    })
                    .map_err(folder_err)
            })
            .await
            .map_err(|e| OpError::new("internal", e.to_string(), "check worker stderr"))?
        })
    }
    fn join_pairing_session(
        &self,
        req: ferry_ipc::pairing::JoinPairingRequest,
    ) -> BoxFuture<'_, Result<PairResult, OpError>> {
        let home = ferry_home_for_backend();
        let identity = self.resolved_identity();
        let store = self.pairing_store.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let ritual = PairingRitual::with_shared(home, identity.clone(), store);
                let pending = ritual
                    .accept_offer(&req.code, Some(&req.target_dir))
                    .map_err(folder_err)?;
                let accepted = pending.complete(0).map_err(folder_err)?;
                Ok(PairResult {
                    folder_id: hex_str(&accepted.folder_id),
                    device_id: hex_str(identity.public()),
                    folder_path: accepted.folder,
                    status: "paired".to_string(),
                    message: Some("pairing completed over in-band transport".to_string()),
                })
            })
            .await
            .map_err(|e| OpError::new("internal", e.to_string(), "check worker stderr"))?
        })
    }
}

pub struct EngineStateSource {
    state: Arc<UiState>,
}

impl EngineStateSource {
    #[must_use]
    pub fn new(state: Arc<UiState>) -> Self {
        Self { state }
    }
}

impl StateSource for EngineStateSource {
    fn open_folder(
        &self,
        path: Option<PathBuf>,
    ) -> Result<ferry_folder::folder::OpenFolder, OpError> {
        let root = path.as_deref().unwrap_or(self.state.tree_dir());
        open_folder(root, self.state.identity()).map_err(folder_err)
    }
    fn list_folder(
        &self,
        _path: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<ListDirectoryResponse, OpError>> {
        Box::pin(async {
            Err(OpError::new(
                "not-implemented",
                "directory listing is not served by the embedded direct backend",
                "run `ferry daemon` for the full IPC-backed dashboard",
            ))
        })
    }
    fn pin_state(&self) -> Result<ferry_sync_engine::pin::HeldSummary, OpError> {
        PinManager::new(self.state.state_dir())
            .summary()
            .map_err(pin_err)
    }
    fn event_stream(&self) -> BoxFuture<'_, Result<UiEventStream, OpError>> {
        Box::pin(async move {
            let (_tx, rx) = tokio::sync::broadcast::channel(16);
            Ok(UiEventStream::new(rx))
        })
    }
    fn folder_root(&self) -> PathBuf {
        self.state.tree_dir().to_path_buf()
    }
    fn identity(&self) -> DeviceIdentity {
        self.state.identity().clone()
    }
    fn state_dir(&self) -> PathBuf {
        self.state.state_dir()
    }
    fn folder_id(&self) -> Option<[u8; 16]> {
        Some(self.state.folder_id())
    }
    fn device_hex(&self) -> String {
        self.state.device_hex().to_string()
    }
    fn snapshot(&self) -> BoxFuture<'_, Result<EngineSnapshot, OpError>> {
        let st = Arc::clone(&self.state);
        Box::pin(async move {
            let Some(manifest_id) = st.handle().current_manifest_id() else {
                return Err(OpError::new(
                    "warming-up",
                    "the engine has not completed its first poll tick",
                    "retry shortly",
                ));
            };
            let counts = st.handle().scan_counts().unwrap_or_default();
            let records = AgreementLedger::new(st.state_dir())
                .list_folder(&st.folder_id())
                .map_err(|e| OpError::new("agreement-state", e.to_string(), "check permissions"))?;
            let mut peers = Vec::new();
            for (dev_bytes, rec) in records {
                let mut p = PeerStatusView::new(
                    hex_str(&dev_bytes),
                    st.handle().peer_connectivity(&dev_bytes),
                );
                p.last_agreed_manifest_id = Some(hex_str(&rec.manifest_id));
                p.agreed_at = Some(ferry_platform::time::fmt_rfc3339(rec.agreed_sec));
                peers.push(p);
            }
            peers.sort_by(|a, b| a.device_id.cmp(&b.device_id));
            let summary = PinManager::new(st.state_dir()).summary().map_err(pin_err)?;
            let conflicts = ferry_sync_engine::list_conflicts(&st.state_dir())
                .map_err(log_err)?
                .len();
            let mut snap = EngineSnapshot::new(
                st.tree_dir().display().to_string(),
                hex_str(&st.folder_id()),
                st.device_hex(),
                "idle",
            );
            snap.manifest_id = Some(hex_str(&manifest_id));
            snap.scanned = ScanStatsView::new(
                counts.files as u64,
                counts.dirs as u64,
                counts.symlinks as u64,
                counts.bytes_chunked,
            );
            snap.pending_changes = st.handle().pending_changes();
            snap.pin = if summary.holding {
                PinView::active(summary.paths)
            } else {
                PinView::none()
            };
            snap.held_changes = summary.total_held_paths;
            snap.held_by_peer = summary.held_by_peer.into_iter().collect();
            snap.peers = peers;
            snap.conflicts = conflicts;
            Ok(snap)
        })
    }
    fn trigger_scan(&self) -> BoxFuture<'_, Result<(), OpError>> {
        let st = Arc::clone(&self.state);
        Box::pin(async move {
            st.handle().trigger_scan();
            Ok(())
        })
    }
    fn list_folders(&self) -> BoxFuture<'_, Result<Vec<ferry_ipc::FolderRecord>, OpError>> {
        Box::pin(async {
            Err(OpError::new(
                "not-implemented",
                "folder listing is not served by the embedded direct backend",
                "run `ferry daemon` for the full IPC-backed dashboard",
            ))
        })
    }
    fn register_folder(
        &self,
        _path: PathBuf,
    ) -> BoxFuture<'_, Result<ferry_ipc::FolderRecord, OpError>> {
        Box::pin(async {
            Err(OpError::new(
                "not-implemented",
                "folder registration is not served by the embedded direct backend",
                "run `ferry daemon` for the full IPC-backed dashboard",
            ))
        })
    }
    fn remove_folder(&self, _folder_id: String) -> BoxFuture<'_, Result<(), OpError>> {
        Box::pin(async {
            Err(OpError::new(
                "not-implemented",
                "folder removal is not served by the embedded direct backend",
                "run `ferry daemon` for the full IPC-backed dashboard",
            ))
        })
    }
    fn create_pairing_session(
        &self,
        _req: ferry_ipc::pairing::CreatePairingRequest,
    ) -> BoxFuture<'_, Result<ferry_ipc::pairing::CreatePairingResponse, OpError>> {
        Box::pin(async {
            Err(OpError::new(
                "not-implemented",
                "pairing sessions are not served by the embedded direct backend",
                "run `ferry daemon` or use the CLI pairing commands",
            ))
        })
    }
    fn join_pairing_session(
        &self,
        _req: ferry_ipc::pairing::JoinPairingRequest,
    ) -> BoxFuture<'_, Result<PairResult, OpError>> {
        Box::pin(async {
            Err(OpError::new(
                "not-implemented",
                "pairing sessions are not served by the embedded direct backend",
                "run `ferry daemon` or use the CLI pairing commands",
            ))
        })
    }
}

pub struct FolderBackend<S: StateSource> {
    source: S,
}

impl<S: StateSource> FolderBackend<S> {
    #[must_use]
    pub fn from_source(source: S) -> Self {
        Self { source }
    }
    #[must_use]
    pub fn source(&self) -> &S {
        &self.source
    }
    #[must_use]
    pub fn into_source(self) -> S {
        self.source
    }
}

impl FolderBackend<FsStateSource> {
    #[must_use]
    pub fn new(folder: impl Into<PathBuf>) -> Self {
        Self::from_source(FsStateSource::new(folder))
    }
    #[must_use]
    pub fn with_identity(self, identity: DeviceIdentity) -> Self {
        let src = self.into_source().with_identity(identity);
        Self::from_source(src)
    }
    #[must_use]
    pub fn with_fallback(self, dir: PathBuf) -> Self {
        let src = self.into_source().with_fallback(dir);
        Self::from_source(src)
    }
}

impl FolderBackend<EngineStateSource> {
    #[must_use]
    pub fn new(state: Arc<UiState>) -> Self {
        Self::from_source(EngineStateSource::new(state))
    }
    #[must_use]
    pub fn state(&self) -> &Arc<UiState> {
        &self.source.state
    }
}

impl<S: StateSource> StatusDomain for FolderBackend<S> {
    fn get_status(&self) -> BoxFuture<'_, Result<EngineSnapshot, OpError>> {
        self.source.snapshot()
    }
    fn list_conflicts(&self) -> BoxFuture<'_, Result<Vec<ConflictEntry>, OpError>> {
        let state_dir = self.source.state_dir();
        Box::pin(async move {
            let list = tokio::task::spawn_blocking(move || {
                ferry_sync_engine::list_conflicts(&state_dir).map_err(log_err)
            })
            .await
            .map_err(|e| OpError::new("internal", e.to_string(), "check worker stderr"))??;
            let mut entries = Vec::new();
            for c in list {
                entries.push(ConflictEntry {
                    ts: c.ts,
                    folder_id: c.folder_id,
                    path: c.path,
                    kind: c.kind,
                    winner: DeviceStamp {
                        device: c.winner.device,
                        mtime_sec: c.winner.mtime_sec,
                        mtime_nsec: c.winner.mtime_nsec,
                    },
                    loser: DeviceStamp {
                        device: c.loser.device,
                        mtime_sec: c.loser.mtime_sec,
                        mtime_nsec: c.loser.mtime_nsec,
                    },
                    quarantined_as: c.quarantined_as,
                });
            }
            Ok(entries)
        })
    }
    fn trigger_scan(&self) -> BoxFuture<'_, Result<(), OpError>> {
        self.source.trigger_scan()
    }
    fn subscribe_events(&self) -> BoxFuture<'_, Result<UiEventStream, OpError>> {
        self.source.event_stream()
    }
}

impl<S: StateSource> InventoryDomain for FolderBackend<S> {
    fn list_directory(
        &self,
        path: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<ListDirectoryResponse, OpError>> {
        self.source.list_folder(path)
    }
    fn list_folders(&self) -> BoxFuture<'_, Result<Vec<ferry_ipc::FolderRecord>, OpError>> {
        self.source.list_folders()
    }
    fn register_folder(
        &self,
        path: PathBuf,
    ) -> BoxFuture<'_, Result<ferry_ipc::FolderRecord, OpError>> {
        self.source.register_folder(path)
    }
    fn remove_folder(&self, folder_id: String) -> BoxFuture<'_, Result<(), OpError>> {
        self.source.remove_folder(folder_id)
    }
}

impl<S: StateSource> SessionDomain for FolderBackend<S> {
    fn start_pin(
        &self,
        paths: Vec<String>,
        _hours: Option<u64>,
    ) -> BoxFuture<'_, Result<PinRecord, OpError>> {
        let state_dir = self.source.state_dir();
        let folder = self.source.folder_root();
        let identity = self.source.identity();
        let folder_id = self.source.folder_id();
        Box::pin(async move {
            let fid = if let Some(id) = folder_id {
                id
            } else {
                let opened = open_folder(&folder, &identity).map_err(folder_err)?;
                opened.folder_id
            };
            tokio::task::spawn_blocking(move || {
                pin_start_blocking(state_dir, folder, paths, &identity, fid)
            })
            .await
            .map_err(|e| OpError::new("internal", e.to_string(), "check worker stderr"))?
        })
    }
    fn stop_pin(&self) -> BoxFuture<'_, Result<PinStopSummary, OpError>> {
        let state_dir = self.source.state_dir();
        let folder = self.source.folder_root();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let mgr = PinManager::new(&state_dir);
                let _ = mgr.stop_session().map_err(pin_err)?;
                Ok(PinStopSummary {
                    folder: folder.display().to_string(),
                    status: "stopped".to_string(),
                    message: Some("pin stopped".to_string()),
                })
            })
            .await
            .map_err(|e| OpError::new("internal", e.to_string(), "check worker stderr"))?
        })
    }
    fn release_pin(&self) -> BoxFuture<'_, Result<PinReleaseSummary, OpError>> {
        let state_dir = self.source.state_dir();
        let folder = self.source.folder_root();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let mgr = PinManager::new(&state_dir);
                let summary = mgr.summary().map_err(pin_err)?;
                if summary.total_held_paths > 0 {
                    return Err(OpError::new(
                        "not-implemented",
                        format!(
                            "{} held changes require reconciliation",
                            summary.total_held_paths
                        ),
                        "run `ferry pin release` on the command line",
                    ));
                }
                let _ = mgr.stop_session().map_err(pin_err)?;
                Ok(PinReleaseSummary {
                    folder: folder.display().to_string(),
                    released_changes: 0,
                    status: "released".to_string(),
                    message: Some("pin released".to_string()),
                })
            })
            .await
            .map_err(|e| OpError::new("internal", e.to_string(), "check worker stderr"))?
        })
    }
    fn share_initiate(
        &self,
        folder: Option<PathBuf>,
        i_know: bool,
    ) -> BoxFuture<'_, Result<ShareOffer, OpError>> {
        let root = folder.unwrap_or_else(|| self.source.folder_root());
        let identity = self.source.identity();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || share_initiate_blocking(root, identity, i_know))
                .await
                .map_err(|e| OpError::new("internal", e.to_string(), "check worker stderr"))?
        })
    }
    fn share_status(&self, folder: Option<PathBuf>) -> BoxFuture<'_, Result<ShareStatus, OpError>> {
        let root = folder.unwrap_or_else(|| self.source.folder_root());
        let identity = self.source.identity();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || share_status_blocking(root, identity))
                .await
                .map_err(|e| OpError::new("internal", e.to_string(), "check worker stderr"))?
        })
    }
    fn pair_accept(
        &self,
        code_or_payload: String,
        dir: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<PairResult, OpError>> {
        let identity = self.source.identity();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                pair_accept_blocking(code_or_payload, dir, identity)
            })
            .await
            .map_err(|e| OpError::new("internal", e.to_string(), "check worker stderr"))?
        })
    }
    fn create_pairing_session(
        &self,
        req: ferry_ipc::pairing::CreatePairingRequest,
    ) -> BoxFuture<'_, Result<ferry_ipc::pairing::CreatePairingResponse, OpError>> {
        self.source.create_pairing_session(req)
    }
    fn join_pairing_session(
        &self,
        req: ferry_ipc::pairing::JoinPairingRequest,
    ) -> BoxFuture<'_, Result<PairResult, OpError>> {
        self.source.join_pairing_session(req)
    }
}

pub type FsBackend = FolderBackend<FsStateSource>;
pub type EngineBackend = FolderBackend<EngineStateSource>;

pub type InProcessAdapter = FsBackend;
pub type DirectBackend = EngineBackend;

pub type IpcBackend = FolderBackend<FsStateSource>;
pub type AutoBackend = ferry_ipc::backend::AutoBackend;

#[must_use]
pub fn fs_backend(folder: impl Into<PathBuf>) -> FsBackend {
    FsBackend::new(folder)
}

#[must_use]
pub fn engine_backend(state: Arc<UiState>) -> EngineBackend {
    EngineBackend::new(state)
}
