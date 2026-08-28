//! Core `UiBackend` trait and typed domain models for all Ferry frontends.
//!
//! Provides the single unified seam powering the Headless CLI, Terminal TUI,
//! Web SPA Dashboard, and Native Pure-Rust Desktop GUI.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};
use tokio_stream::Stream;

use crate::protocol::{ConflictEntry, EngineSnapshot, ScanStatsView, TransferDirection};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Domain error taxonomy with error codes, human messages, and actionable hints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct OpError {
    pub code: String,
    pub message: String,
    pub hint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

impl OpError {
    #[must_use]
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            hint: hint.into(),
            detail: None,
        }
    }

    #[must_use]
    pub fn with_detail(mut self, detail: serde_json::Value) -> Self {
        self.detail = Some(detail);
        self
    }

    #[must_use]
    pub fn not_found(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new("not-found", message, hint)
    }

    #[must_use]
    pub fn internal(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new("internal", message, hint)
    }

    #[must_use]
    pub fn bad_request(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new("bad-request", message, hint)
    }
}

impl From<std::io::Error> for OpError {
    fn from(e: std::io::Error) -> Self {
        Self::new(
            "io",
            e.to_string(),
            "check folder permissions and disk space",
        )
    }
}

/// Result of starting a session pin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinRecord {
    pub folder: String,
    pub paths: Vec<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Result of stopping an active pin without releasing held changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinStopSummary {
    pub folder: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Result of releasing an active pin and reconciling held changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinReleaseSummary {
    pub folder: String,
    pub released_changes: usize,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Pairing offer generated when initiating a share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareOffer {
    pub folder: String,
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qr_payload: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub secret_warnings: Vec<String>,
}

/// Active share status of a folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareStatus {
    pub folder: String,
    pub status: String,
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_device_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offer: Option<ShareOffer>,
}

/// Result of accepting an incoming pairing payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairResult {
    pub folder_id: String,
    pub device_id: String,
    pub folder_path: PathBuf,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Entry in a filesystem directory listing for in-UI folder pickers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub is_git_repo: bool,
    pub is_already_synced: bool,
}

/// Directory listing payload for folder explorers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryListing {
    pub current_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_path: Option<PathBuf>,
    pub entries: Vec<FsEntry>,
}

/// Folder registration metadata for multi-folder daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderInfo {
    pub id: String,
    pub path: PathBuf,
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

/// In-band pairing session state negotiated over mDNS/Iroh.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingSession {
    pub session_id: String,
    pub code: String,
    pub folder_id: String,
    pub role: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Persistent record for active in-band pairing sessions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingSessionRecord {
    pub session_id: String,
    pub code: String,
    pub folder_id: String,
    pub device_id: String,
    pub listen_addr: String,
    pub poly: u64,
    pub fmk_hex: String,
    pub created_sec: i64,
    #[serde(default)]
    pub sync_listen_addr: Option<String>,
}

pub const WORDLIST: &[&str] = &[
    "beacon", "river", "falcon", "ember", "drift", "summit", "cedar", "harbor", "meadow",
    "glacier", "breeze", "canyon", "orbit", "pulse", "timber", "quartz", "echo", "solace",
    "valley", "zenith", "aurora", "cliff", "dune", "forest", "haven", "island", "jungle",
    "lagoon", "mountain", "oasis", "prairie", "ridge", "safari", "tundra", "voyage", "willow",
    "cove", "delta", "frost", "grove", "inlet", "mesa", "ocean", "pinnacle", "reef", "stream",
    "trail", "vista", "cascade", "crag", "fjord", "geyser", "hollow", "knoll", "ledge",
    "plateau", "ravine", "steppe", "thicket", "volcano", "watershed", "alpha", "bravo",
];

#[must_use]
pub fn generate_6word_code() -> String {
    use rand::seq::SliceRandom;
    let mut rng = rand::thread_rng();
    let words: Vec<&str> = WORDLIST.choose_multiple(&mut rng, 6).copied().collect();
    words.join("-")
}

#[must_use]
pub fn normalize_code(code: &str) -> String {
    code.trim().to_lowercase().replace(' ', "-")
}

#[must_use]
pub fn pairing_store_dir() -> PathBuf {
    std::env::temp_dir().join(".ferry_pairing")
}

pub fn save_pairing_record(record: &PairingSessionRecord) -> std::io::Result<()> {
    let dir = pairing_store_dir();
    std::fs::create_dir_all(&dir)?;
    let norm = normalize_code(&record.code);
    let path = dir.join(format!("{norm}.json"));
    let content = serde_json::to_string_pretty(record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, content)?;
    Ok(())
}

#[must_use]
pub fn load_pairing_record(code: &str) -> Option<PairingSessionRecord> {
    let dir = pairing_store_dir();
    let norm = normalize_code(code);
    let path = dir.join(format!("{norm}.json"));
    if path.exists() {
        let content = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    } else {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                let ep = entry.path();
                if ep.extension().and_then(|s| s.to_str()) == Some("json") {
                    if let Ok(content) = std::fs::read_to_string(&ep) {
                        if let Ok(rec) = serde_json::from_str::<PairingSessionRecord>(&content) {
                            if normalize_code(&rec.code) == norm || rec.code.contains(&norm) {
                                return Some(rec);
                            }
                        }
                    }
                }
            }
        }
        None
    }
}

/// Real-time asynchronous push events emitted by `UiBackend`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum UiEvent {
    State(EngineSnapshot),
    StateChanged {
        state: String,
        manifest_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agreed_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pending_changes: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stats: Option<ScanStatsView>,
    },
    TransferProgress {
        bytes_transferred: u64,
        total_bytes: u64,
        current_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chunks_transferred: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total_chunks: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        peer_device_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        direction: Option<TransferDirection>,
    },
    ConflictRecorded {
        path: String,
        conflict_path: String,
        timestamp: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quarantined_as: Option<String>,
    },
    Error {
        code: String,
        message: String,
    },
}

use tokio_stream::wrappers::BroadcastStream;

/// Stream of `UiEvent` items backed by a broadcast receiver.
pub struct UiEventStream {
    inner: BroadcastStream<UiEvent>,
}

impl UiEventStream {
    #[must_use]
    pub fn new(rx: broadcast::Receiver<UiEvent>) -> Self {
        Self {
            inner: BroadcastStream::new(rx),
        }
    }

    pub async fn recv(&mut self) -> Result<UiEvent, broadcast::error::RecvError> {
        use tokio_stream::StreamExt;
        match self.inner.next().await {
            Some(Ok(event)) => Ok(event),
            Some(Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n))) => {
                Err(broadcast::error::RecvError::Lagged(n))
            }
            None => Err(broadcast::error::RecvError::Closed),
        }
    }
}

impl Stream for UiEventStream {
    type Item = UiEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            match Pin::new(&mut self.inner).poll_next(cx) {
                Poll::Ready(Some(Ok(event))) => return Poll::Ready(Some(event)),
                Poll::Ready(Some(Err(_lagged))) => {}
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// The unified asynchronous UI backend contract.
pub trait UiBackend: Send + Sync + 'static {
    fn get_status(&self) -> BoxFuture<'_, Result<EngineSnapshot, OpError>>;
    fn list_conflicts(&self) -> BoxFuture<'_, Result<Vec<ConflictEntry>, OpError>>;
    fn start_pin(
        &self,
        paths: Vec<String>,
        hours: Option<u64>,
    ) -> BoxFuture<'_, Result<PinRecord, OpError>>;
    fn stop_pin(&self) -> BoxFuture<'_, Result<PinStopSummary, OpError>>;
    fn release_pin(&self) -> BoxFuture<'_, Result<PinReleaseSummary, OpError>>;
    fn share_initiate(
        &self,
        folder: Option<PathBuf>,
        i_know: bool,
    ) -> BoxFuture<'_, Result<ShareOffer, OpError>>;
    fn share_status(&self, folder: Option<PathBuf>) -> BoxFuture<'_, Result<ShareStatus, OpError>>;
    fn pair_accept(
        &self,
        payload: PathBuf,
        dir: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<PairResult, OpError>>;
    fn trigger_scan(&self) -> BoxFuture<'_, Result<(), OpError>>;
    fn subscribe_events(&self) -> BoxFuture<'_, Result<UiEventStream, OpError>>;

    /// List directory contents for filesystem browser modals.
    fn list_directory(
        &self,
        path: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<DirectoryListing, OpError>>;

    /// List all registered folders in the device daemon.
    fn list_folders(&self) -> BoxFuture<'_, Result<Vec<FolderInfo>, OpError>>;

    /// Register a new folder to sync.
    fn register_folder(&self, path: PathBuf) -> BoxFuture<'_, Result<FolderInfo, OpError>>;

    /// Unregister a folder by id.
    fn unregister_folder(&self, folder_id: String) -> BoxFuture<'_, Result<(), OpError>>;

    /// Switch active folder context for status view.
    fn switch_folder(&self, folder_id: String) -> BoxFuture<'_, Result<EngineSnapshot, OpError>>;

    /// Create an in-band pairing session generating a 6-word code.
    fn create_pairing_session(
        &self,
        folder_id: Option<String>,
    ) -> BoxFuture<'_, Result<PairingSession, OpError>>;

    /// Join an in-band pairing session via 6-word code and destination folder.
    fn join_pairing_session(
        &self,
        code: String,
        target_dir: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<PairResult, OpError>>;
}

/// In-memory fake backend for deterministic testing across frontends.
#[derive(Clone)]
pub struct FakeBackend {
    snapshot: Arc<RwLock<EngineSnapshot>>,
    conflicts: Arc<RwLock<Vec<ConflictEntry>>>,
    active_pin: Arc<RwLock<Option<PinRecord>>>,
    active_share: Arc<RwLock<Option<ShareOffer>>>,
    folders: Arc<RwLock<Vec<FolderInfo>>>,
    pairing_sessions: Arc<RwLock<Vec<PairingSession>>>,
    event_tx: broadcast::Sender<UiEvent>,
}

impl Default for FakeBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeBackend {
    #[must_use]
    pub fn new() -> Self {
        let (event_tx, _) = broadcast::channel(64);
        let default_folder = FolderInfo {
            id: "0123456789abcdef0123456789abcdef".to_string(),
            path: PathBuf::from("/test/folder"),
            active: true,
            state: Some("idle".to_string()),
        };
        Self {
            snapshot: Arc::new(RwLock::new(EngineSnapshot::new(
                "/test/folder",
                "0123456789abcdef0123456789abcdef",
                "test-device-id",
                "idle",
            ))),
            conflicts: Arc::new(RwLock::new(Vec::new())),
            active_pin: Arc::new(RwLock::new(None)),
            active_share: Arc::new(RwLock::new(None)),
            folders: Arc::new(RwLock::new(vec![default_folder])),
            pairing_sessions: Arc::new(RwLock::new(Vec::new())),
            event_tx,
        }
    }

    #[must_use]
    pub fn with_snapshot(snapshot: EngineSnapshot) -> Self {
        let fake = Self::new();
        *fake.snapshot.try_write().expect("lock snapshot") = snapshot;
        fake
    }

    pub async fn set_snapshot(&self, snapshot: EngineSnapshot) {
        *self.snapshot.write().await = snapshot.clone();
        let _ = self.event_tx.send(UiEvent::State(snapshot));
    }

    pub async fn add_conflict(&self, conflict: ConflictEntry) {
        self.conflicts.write().await.push(conflict.clone());
        let _ = self.event_tx.send(UiEvent::ConflictRecorded {
            path: conflict.path,
            conflict_path: conflict.quarantined_as.clone().unwrap_or_default(),
            timestamp: 0,
            quarantined_as: conflict.quarantined_as,
        });
    }

    pub fn emit_event(&self, event: UiEvent) {
        let _ = self.event_tx.send(event);
    }
}

impl UiBackend for FakeBackend {
    fn get_status(&self) -> BoxFuture<'_, Result<EngineSnapshot, OpError>> {
        let snap = Arc::clone(&self.snapshot);
        Box::pin(async move { Ok(snap.read().await.clone()) })
    }

    fn list_conflicts(&self) -> BoxFuture<'_, Result<Vec<ConflictEntry>, OpError>> {
        let confs = Arc::clone(&self.conflicts);
        Box::pin(async move { Ok(confs.read().await.clone()) })
    }

    fn start_pin(
        &self,
        paths: Vec<String>,
        _hours: Option<u64>,
    ) -> BoxFuture<'_, Result<PinRecord, OpError>> {
        let active_pin = Arc::clone(&self.active_pin);
        let snap = Arc::clone(&self.snapshot);
        let tx = self.event_tx.clone();
        Box::pin(async move {
            let record = PinRecord {
                folder: snap.read().await.folder.clone(),
                paths: paths.clone(),
                status: "active".to_string(),
                expires_at: None,
                message: Some("session pin active".to_string()),
            };
            *active_pin.write().await = Some(record.clone());
            {
                let mut st = snap.write().await;
                st.pin = crate::protocol::PinView::active(paths);
                let _ = tx.send(UiEvent::State(st.clone()));
            }
            Ok(record)
        })
    }

    fn stop_pin(&self) -> BoxFuture<'_, Result<PinStopSummary, OpError>> {
        let active_pin = Arc::clone(&self.active_pin);
        let snap = Arc::clone(&self.snapshot);
        let tx = self.event_tx.clone();
        Box::pin(async move {
            *active_pin.write().await = None;
            let folder = {
                let mut st = snap.write().await;
                st.pin = crate::protocol::PinView::none();
                let f = st.folder.clone();
                let _ = tx.send(UiEvent::State(st.clone()));
                f
            };
            Ok(PinStopSummary {
                folder,
                status: "stopped".to_string(),
                message: Some("pin stopped".to_string()),
            })
        })
    }

    fn release_pin(&self) -> BoxFuture<'_, Result<PinReleaseSummary, OpError>> {
        let active_pin = Arc::clone(&self.active_pin);
        let snap = Arc::clone(&self.snapshot);
        let tx = self.event_tx.clone();
        Box::pin(async move {
            *active_pin.write().await = None;
            let folder = {
                let mut st = snap.write().await;
                st.pin = crate::protocol::PinView::none();
                st.held_changes = 0;
                let f = st.folder.clone();
                let _ = tx.send(UiEvent::State(st.clone()));
                f
            };
            Ok(PinReleaseSummary {
                folder,
                released_changes: 0,
                status: "released".to_string(),
                message: Some("pin released".to_string()),
            })
        })
    }

    fn share_initiate(
        &self,
        folder: Option<PathBuf>,
        i_know: bool,
    ) -> BoxFuture<'_, Result<ShareOffer, OpError>> {
        let snap = Arc::clone(&self.snapshot);
        let active_share = Arc::clone(&self.active_share);
        Box::pin(async move {
            let folder_str = if let Some(ref p) = folder {
                p.display().to_string()
            } else if let Ok(s) = snap.try_read() {
                s.folder.clone()
            } else {
                "/test/folder".to_string()
            };
            if !i_know && folder_str.contains("secret") {
                return Err(OpError::new(
                    "secret-detected",
                    "secrets detected in folder",
                    "pass i_know = true to bypass",
                ));
            }
            let offer = ShareOffer {
                folder: folder_str,
                token: "test-share-token-1234567890abcdef".to_string(),
                payload_path: Some(PathBuf::from("/test/folder/.ferry/pair-offer.ferry-pair")),
                qr_payload: Some("FERRY:PAIR:TEST".to_string()),
                expires_at: None,
                secret_warnings: Vec::new(),
            };
            *active_share.write().await = Some(offer.clone());
            Ok(offer)
        })
    }

    fn share_status(&self, folder: Option<PathBuf>) -> BoxFuture<'_, Result<ShareStatus, OpError>> {
        let snap = Arc::clone(&self.snapshot);
        let active_share = Arc::clone(&self.active_share);
        Box::pin(async move {
            let folder_str = if let Some(ref p) = folder {
                p.display().to_string()
            } else if let Ok(s) = snap.try_read() {
                s.folder.clone()
            } else {
                "/test/folder".to_string()
            };
            let offer = active_share.read().await.clone();
            let active = offer.is_some();
            Ok(ShareStatus {
                folder: folder_str,
                status: if active {
                    "pending".to_string()
                } else {
                    "none".to_string()
                },
                active,
                peer_device_id: None,
                offer,
            })
        })
    }

    fn pair_accept(
        &self,
        payload: PathBuf,
        dir: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<PairResult, OpError>> {
        Box::pin(async move {
            Ok(PairResult {
                folder_id: "0123456789abcdef0123456789abcdef".to_string(),
                device_id: "peer-device-id".to_string(),
                folder_path: dir.unwrap_or_else(|| PathBuf::from("/test/folder")),
                status: "paired".to_string(),
                message: Some(format!("paired with {}", payload.display())),
            })
        })
    }

    fn trigger_scan(&self) -> BoxFuture<'_, Result<(), OpError>> {
        let snap = Arc::clone(&self.snapshot);
        let tx = self.event_tx.clone();
        Box::pin(async move {
            let mut st = snap.write().await;
            st.scanned.files += 1;
            let _ = tx.send(UiEvent::State(st.clone()));
            Ok(())
        })
    }

    fn subscribe_events(&self) -> BoxFuture<'_, Result<UiEventStream, OpError>> {
        let rx = self.event_tx.subscribe();
        Box::pin(async move { Ok(UiEventStream::new(rx)) })
    }

    fn list_directory(
        &self,
        path: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<DirectoryListing, OpError>> {
        Box::pin(async move {
            let target = path.unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
            });
            if target.exists() && target.is_dir() {
                let mut entries = Vec::new();
                if let Ok(rd) = std::fs::read_dir(&target) {
                    for entry in rd.flatten() {
                        let p = entry.path();
                        let is_dir = p.is_dir();
                        let is_symlink = p.is_symlink();
                        let is_git_repo = is_dir && p.join(".git").exists();
                        let is_already_synced = is_dir && p.join(".ferry").exists();
                        let name = entry.file_name().to_string_lossy().to_string();
                        entries.push(FsEntry {
                            name,
                            path: p,
                            is_dir,
                            is_symlink,
                            is_git_repo,
                            is_already_synced,
                        });
                    }
                }
                entries.sort_by(|a, b| match (b.is_dir, a.is_dir) {
                    (true, false) => std::cmp::Ordering::Greater,
                    (false, true) => std::cmp::Ordering::Less,
                    _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                });
                let parent_path = target.parent().map(PathBuf::from);
                Ok(DirectoryListing {
                    current_path: target,
                    parent_path,
                    entries,
                })
            } else {
                Ok(DirectoryListing {
                    current_path: target.clone(),
                    parent_path: target.parent().map(PathBuf::from),
                    entries: vec![
                        FsEntry {
                            name: "project-a".to_string(),
                            path: target.join("project-a"),
                            is_dir: true,
                            is_symlink: false,
                            is_git_repo: true,
                            is_already_synced: false,
                        },
                        FsEntry {
                            name: "project-b".to_string(),
                            path: target.join("project-b"),
                            is_dir: true,
                            is_symlink: false,
                            is_git_repo: false,
                            is_already_synced: true,
                        },
                    ],
                })
            }
        })
    }

    fn list_folders(&self) -> BoxFuture<'_, Result<Vec<FolderInfo>, OpError>> {
        let folders = Arc::clone(&self.folders);
        Box::pin(async move { Ok(folders.read().await.clone()) })
    }

    fn register_folder(&self, path: PathBuf) -> BoxFuture<'_, Result<FolderInfo, OpError>> {
        let folders = Arc::clone(&self.folders);
        let snap = Arc::clone(&self.snapshot);
        Box::pin(async move {
            let id = format!("{:032x}", folders.read().await.len() + 1);
            let info = FolderInfo {
                id: id.clone(),
                path: path.clone(),
                active: true,
                state: Some("idle".to_string()),
            };
            let mut flist = folders.write().await;
            for f in flist.iter_mut() {
                f.active = false;
            }
            flist.push(info.clone());
            let mut st = snap.write().await;
            st.folder = path.display().to_string();
            st.folder_id = id;
            Ok(info)
        })
    }

    fn unregister_folder(&self, folder_id: String) -> BoxFuture<'_, Result<(), OpError>> {
        let folders = Arc::clone(&self.folders);
        Box::pin(async move {
            let mut flist = folders.write().await;
            flist.retain(|f| f.id != folder_id);
            Ok(())
        })
    }

    fn switch_folder(&self, folder_id: String) -> BoxFuture<'_, Result<EngineSnapshot, OpError>> {
        let folders = Arc::clone(&self.folders);
        let snap = Arc::clone(&self.snapshot);
        let tx = self.event_tx.clone();
        Box::pin(async move {
            let mut flist = folders.write().await;
            let mut target_path = None;
            for f in flist.iter_mut() {
                if f.id == folder_id {
                    f.active = true;
                    target_path = Some(f.path.clone());
                } else {
                    f.active = false;
                }
            }
            let path = target_path.ok_or_else(|| {
                OpError::not_found("folder not found", "register the folder first")
            })?;
            let mut st = snap.write().await;
            st.folder = path.display().to_string();
            st.folder_id = folder_id;
            let _ = tx.send(UiEvent::State(st.clone()));
            Ok(st.clone())
        })
    }

    fn create_pairing_session(
        &self,
        folder_id: Option<String>,
    ) -> BoxFuture<'_, Result<PairingSession, OpError>> {
        let sessions = Arc::clone(&self.pairing_sessions);
        let snap = Arc::clone(&self.snapshot);
        Box::pin(async move {
            let fid = folder_id.unwrap_or_else(|| snap.try_read().map_or("default".into(), |s| s.folder_id.clone()));
            let session = PairingSession {
                session_id: "sess-12345".to_string(),
                code: "beacon-river-falcon-ember-drift-summit".to_string(),
                folder_id: fid,
                role: "host".to_string(),
                status: "advertising".to_string(),
                message: Some("Pairing session active. Share the 6-word code with your peer.".to_string()),
            };
            sessions.write().await.push(session.clone());
            Ok(session)
        })
    }

    fn join_pairing_session(
        &self,
        code: String,
        target_dir: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<PairResult, OpError>> {
        Box::pin(async move {
            Ok(PairResult {
                folder_id: "0123456789abcdef0123456789abcdef".to_string(),
                device_id: "remote-peer-id".to_string(),
                folder_path: target_dir.unwrap_or_else(|| PathBuf::from("/test/target")),
                status: "paired".to_string(),
                message: Some(format!("Successfully paired via code: {code}")),
            })
        })
    }
}
