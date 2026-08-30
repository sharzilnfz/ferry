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

use std::collections::HashMap;

use ferry_folder::inventory::{
    ferry_home, sort_entries, validate_path, DirectoryEntry, FolderInventory, FolderRecord,
    ListDirectoryResponse,
};

use crate::pairing::{CreatePairingRequest, CreatePairingResponse, JoinPairingRequest};
use crate::protocol::{ConflictEntry, EngineSnapshot, ScanStatsView, TransferDirection};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Error code used when the daemon socket cannot be reached or the persistent
/// connection drops. `AutoBackend` routes on exactly this code to fall back to
/// the in-process adapter; daemon-originated domain errors never trigger it.
pub const DAEMON_UNREACHABLE: &str = "daemon-unreachable";

/// Coded folder-inventory failures flow into the frontend error taxonomy
/// unchanged (same `code`/`message`/`hint` discipline).
impl From<ferry_folder::FolderError> for OpError {
    fn from(e: ferry_folder::FolderError) -> Self {
        Self::new(e.code, e.message, e.hint)
    }
}

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

    /// True when the error is a transport-level failure (daemon unreachable),
    /// as opposed to a domain error reported by the daemon or local logic.
    #[must_use]
    pub fn is_transport(&self) -> bool {
        self.code == DAEMON_UNREACHABLE
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
    FolderRegistered {
        path: String,
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

/// Status and telemetry domain: engine snapshots, conflict listings, manual
/// rescans, and the push-event stream.
pub trait StatusDomain: Send + Sync + 'static {
    fn get_status(&self) -> BoxFuture<'_, Result<EngineSnapshot, OpError>>;
    fn list_conflicts(&self) -> BoxFuture<'_, Result<Vec<ConflictEntry>, OpError>>;
    fn trigger_scan(&self) -> BoxFuture<'_, Result<(), OpError>>;
    fn subscribe_events(&self) -> BoxFuture<'_, Result<UiEventStream, OpError>>;
}

/// Folder inventory domain: directory inspection and `$FERRY_HOME` registry
/// operations.
pub trait InventoryDomain: Send + Sync + 'static {
    fn list_directory(
        &self,
        path: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<ListDirectoryResponse, OpError>>;
    fn list_folders(&self) -> BoxFuture<'_, Result<Vec<FolderRecord>, OpError>>;
    fn register_folder(&self, path: PathBuf) -> BoxFuture<'_, Result<FolderRecord, OpError>>;
    fn remove_folder(&self, folder_id: String) -> BoxFuture<'_, Result<(), OpError>>;
}

/// Session domain: pairing and pinning lifecycle (pins, share offers, pair
/// acceptance, and rendezvous pairing sessions).
pub trait SessionDomain: Send + Sync + 'static {
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
    /// Accept an incoming pairing offer given EITHER form: a 6-character
    /// code or a `.ferry-pair` payload file path / `FERRY1:` envelope. The
    /// backend picks the transport; callers never branch.
    fn pair_accept(
        &self,
        code_or_payload: String,
        dir: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<PairResult, OpError>>;
    fn create_pairing_session(
        &self,
        req: CreatePairingRequest,
    ) -> BoxFuture<'_, Result<CreatePairingResponse, OpError>>;
    fn join_pairing_session(
        &self,
        req: JoinPairingRequest,
    ) -> BoxFuture<'_, Result<PairResult, OpError>>;
}

/// The unified asynchronous UI backend contract: the three cohesive session
/// domains (`status`, `inventory`, `session`) composed into one seam. Every
/// frontend consumes `Arc<dyn UiBackend>`; every adapter implements the three
/// domain traits and gets this seam for free.
pub trait UiBackend: StatusDomain + InventoryDomain + SessionDomain {}

impl<T: StatusDomain + InventoryDomain + SessionDomain> UiBackend for T {}

#[derive(Debug, Clone)]
struct InMemPairingSession {
    folder_id: String,
    expires_at: std::time::SystemTime,
}

/// In-memory fake backend for deterministic testing across frontends.
#[derive(Clone)]
pub struct FakeBackend {
    snapshot: Arc<RwLock<EngineSnapshot>>,
    conflicts: Arc<RwLock<Vec<ConflictEntry>>>,
    active_pin: Arc<RwLock<Option<PinRecord>>>,
    active_share: Arc<RwLock<Option<ShareOffer>>>,
    event_tx: broadcast::Sender<UiEvent>,
    fs_fixture: Arc<RwLock<HashMap<PathBuf, Vec<DirectoryEntry>>>>,
    pairing_sessions: Arc<std::sync::Mutex<HashMap<String, InMemPairingSession>>>,
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
            event_tx,
            fs_fixture: Arc::new(RwLock::new(HashMap::new())),
            pairing_sessions: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Test helper: force a pairing code to be expired.
    pub fn expire_pairing_code(&self, code: &str) {
        let key = code.to_ascii_uppercase();
        if let Ok(mut m) = self.pairing_sessions.lock() {
            if let Some(s) = m.get_mut(&key) {
                s.expires_at = std::time::SystemTime::now() - std::time::Duration::from_secs(1);
            }
        }
    }

    /// Insert or replace the in-memory directory tree used by `list_directory`.
    pub async fn set_fs_fixture(&self, fixture: HashMap<PathBuf, Vec<DirectoryEntry>>) {
        *self.fs_fixture.write().await = fixture;
    }

    /// Insert entries for a single directory into the in-memory fixture.
    pub async fn insert_fs_dir(&self, dir: PathBuf, entries: Vec<DirectoryEntry>) {
        self.fs_fixture.write().await.insert(dir, entries);
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

impl StatusDomain for FakeBackend {
    fn get_status(&self) -> BoxFuture<'_, Result<EngineSnapshot, OpError>> {
        let snap = Arc::clone(&self.snapshot);
        Box::pin(async move { Ok(snap.read().await.clone()) })
    }

    fn list_conflicts(&self) -> BoxFuture<'_, Result<Vec<ConflictEntry>, OpError>> {
        let confs = Arc::clone(&self.conflicts);
        Box::pin(async move { Ok(confs.read().await.clone()) })
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
}

impl SessionDomain for FakeBackend {
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
        code_or_payload: String,
        dir: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<PairResult, OpError>> {
        Box::pin(async move {
            Ok(PairResult {
                folder_id: "0123456789abcdef0123456789abcdef".to_string(),
                device_id: "peer-device-id".to_string(),
                folder_path: dir.unwrap_or_else(|| PathBuf::from("/test/folder")),
                status: "paired".to_string(),
                message: Some(format!("paired with {code_or_payload}")),
            })
        })
    }

    fn create_pairing_session(
        &self,
        req: CreatePairingRequest,
    ) -> BoxFuture<'_, Result<CreatePairingResponse, OpError>> {
        let sessions = Arc::clone(&self.pairing_sessions);
        Box::pin(async move {
            // Validate folder_id shape (32 hex chars, like the unified ritual)
            let folder_id = req.folder_id.clone();
            if folder_id.len() < 32 {
                return Err(OpError::new(
                    "bad-request",
                    "invalid folder_id",
                    "folder_id must be 32 hex chars",
                ));
            }
            let mut rng = rand::thread_rng();
            let code = {
                use crate::pairing::PairingCode;
                PairingCode::generate(&mut rng).0
            };
            let key = code.to_ascii_uppercase();
            let expires_at = std::time::SystemTime::now() + std::time::Duration::from_secs(300);
            let expires_at_str = {
                let secs = expires_at
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                format!("2026-08-29T00:00:00Z#{secs}")
            };
            let sess = InMemPairingSession {
                folder_id: folder_id.clone(),
                expires_at,
            };
            {
                let mut m = sessions.lock().unwrap();
                m.insert(key.clone(), sess);
            }
            Ok(CreatePairingResponse::new(code, expires_at_str))
        })
    }

    fn join_pairing_session(
        &self,
        req: JoinPairingRequest,
    ) -> BoxFuture<'_, Result<PairResult, OpError>> {
        let sessions = Arc::clone(&self.pairing_sessions);
        Box::pin(async move {
            let key = req.code.trim().to_ascii_uppercase().replace(['-', ' '], "");
            let sess = {
                let mut m = sessions.lock().unwrap();
                let sess = m.get(&key).cloned();
                match sess {
                    Some(s) => {
                        if std::time::SystemTime::now()
                            .duration_since(s.expires_at)
                            .is_ok()
                        {
                            m.remove(&key);
                            return Err(OpError::new(
                                "pairing-expired",
                                format!("pairing code {} expired", req.code),
                                "ask the sharing device to create a new code",
                            ));
                        }
                        m.remove(&key);
                        s
                    }
                    None => {
                        return Err(OpError::new(
                            "pairing-not-found",
                            format!("pairing code {} not found", req.code),
                            "check the code and try again",
                        ))
                    }
                }
            };
            // No file at $FERRY_HOME/pair-* is ever touched here — in-memory rendezvous only.
            Ok(PairResult {
                folder_id: sess.folder_id,
                device_id: "peer-device-id".to_string(),
                folder_path: req.target_dir,
                status: "paired".to_string(),
                message: Some("paired via in-memory rendezvous".to_string()),
            })
        })
    }
}

impl InventoryDomain for FakeBackend {
    fn list_directory(
        &self,
        path: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<ListDirectoryResponse, OpError>> {
        let fixture = Arc::clone(&self.fs_fixture);
        Box::pin(async move {
            let validated = validate_path(path)?;
            let map = fixture.read().await;
            // Preserve wave 0 stub for tests that never configure a fixture.
            if map.is_empty() && !map.contains_key(&validated) {
                return Err(OpError::not_found("not-implemented", "wave 0 stub"));
            }
            match map.get(&validated) {
                Some(entries) => {
                    let mut out = entries.clone();
                    sort_entries(&mut out);
                    Ok(ListDirectoryResponse::new(out, validated))
                }
                None => Err(OpError::new(
                    "not-found",
                    format!("no such directory: {}", validated.display()),
                    "check path",
                )),
            }
        })
    }

    fn list_folders(&self) -> BoxFuture<'_, Result<Vec<FolderRecord>, OpError>> {
        Box::pin(async { Err(OpError::not_found("not-implemented", "wave 0 stub")) })
    }

    fn register_folder(&self, _path: PathBuf) -> BoxFuture<'_, Result<FolderRecord, OpError>> {
        Box::pin(async { Err(OpError::not_found("not-implemented", "wave 0 stub")) })
    }

    fn remove_folder(&self, _folder_id: String) -> BoxFuture<'_, Result<(), OpError>> {
        Box::pin(async { Err(OpError::not_found("not-implemented", "wave 0 stub")) })
    }
}

/// Composite automatic backend: talks to the daemon over the persistent
/// multiplexed IPC connection (`DaemonClient`) when reachable, and transparently routes
/// to in-process fallback on transport failure (`daemon-unreachable`).
/// Domain errors from the daemon (e.g. `pin-active`) are returned as-is.
#[derive(Clone)]
pub struct AutoBackend {
    client: crate::client::DaemonClient,
    folder_path: Option<PathBuf>,
    fallback: Option<Arc<dyn UiBackend>>,
}

impl std::fmt::Debug for AutoBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AutoBackend")
            .field("client", &self.client)
            .field("folder_path", &self.folder_path)
            .finish()
    }
}

impl AutoBackend {
    #[must_use]
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            client: crate::client::DaemonClient::new(socket_path),
            folder_path: None,
            fallback: None,
        }
    }

    #[must_use]
    pub fn with_client(client: crate::client::DaemonClient) -> Self {
        Self {
            client,
            folder_path: None,
            fallback: None,
        }
    }

    #[must_use]
    pub fn with_fallback(mut self, folder: impl Into<PathBuf>) -> Self {
        self.folder_path = Some(folder.into());
        self
    }

    #[must_use]
    pub fn with_fallback_backend(mut self, fallback: Arc<dyn UiBackend>) -> Self {
        self.fallback = Some(fallback);
        self
    }

    #[must_use]
    pub fn client(&self) -> &crate::client::DaemonClient {
        &self.client
    }

    #[must_use]
    pub fn folder_path(&self) -> Option<&PathBuf> {
        self.folder_path.as_ref()
    }
}

/// Unified factory constructing an `AutoBackend` for the given socket path and optional folder path.
#[must_use]
pub fn connect_auto(
    socket_path: impl Into<PathBuf>,
    folder_path: impl Into<Option<PathBuf>>,
) -> AutoBackend {
    let mut auto = AutoBackend::new(socket_path);
    if let Some(folder) = folder_path.into() {
        auto = auto.with_fallback(folder);
    }
    auto
}

impl StatusDomain for AutoBackend {
    fn get_status(&self) -> BoxFuture<'_, Result<EngineSnapshot, OpError>> {
        let client = self.client.clone();
        let fallback = self.fallback.clone();
        let folder_path = self.folder_path.clone();
        Box::pin(async move {
            match client.get_status().await {
                Ok(snap) => Ok(snap),
                Err(e) if e.is_transport() => {
                    if let Some(fb) = fallback {
                        fb.get_status().await
                    } else {
                        let folder_str = folder_path
                            .map_or_else(|| ".".to_string(), |p| p.display().to_string());
                        Ok(EngineSnapshot::new(folder_str, "", "", "offline"))
                    }
                }
                Err(e) => Err(e),
            }
        })
    }

    fn list_conflicts(&self) -> BoxFuture<'_, Result<Vec<ConflictEntry>, OpError>> {
        let client = self.client.clone();
        let fallback = self.fallback.clone();
        Box::pin(async move {
            match client.list_conflicts().await {
                Ok(list) => Ok(list),
                Err(e) if e.is_transport() => {
                    if let Some(fb) = fallback {
                        fb.list_conflicts().await
                    } else {
                        Ok(Vec::new())
                    }
                }
                Err(e) => Err(e),
            }
        })
    }

    fn trigger_scan(&self) -> BoxFuture<'_, Result<(), OpError>> {
        let client = self.client.clone();
        let fallback = self.fallback.clone();
        Box::pin(async move {
            match client.trigger_scan().await {
                Ok(()) => Ok(()),
                Err(e) if e.is_transport() => {
                    if let Some(fb) = fallback {
                        fb.trigger_scan().await
                    } else {
                        Ok(())
                    }
                }
                Err(e) => Err(e),
            }
        })
    }

    fn subscribe_events(&self) -> BoxFuture<'_, Result<UiEventStream, OpError>> {
        let client = self.client.clone();
        let fallback = self.fallback.clone();
        Box::pin(async move {
            match client.subscribe_events().await {
                Ok(stream) => Ok(stream),
                Err(e) if e.is_transport() => {
                    if let Some(fb) = fallback {
                        fb.subscribe_events().await
                    } else {
                        let (_tx, rx) = broadcast::channel(16);
                        Ok(UiEventStream::new(rx))
                    }
                }
                Err(e) => Err(e),
            }
        })
    }
}

impl InventoryDomain for AutoBackend {
    fn list_directory(
        &self,
        path: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<ListDirectoryResponse, OpError>> {
        let client = self.client.clone();
        let fallback = self.fallback.clone();
        Box::pin(async move {
            match client.list_directory(path.clone()).await {
                Ok(resp) => Ok(resp),
                Err(e) if e.is_transport() => {
                    if let Some(fb) = fallback {
                        fb.list_directory(path).await
                    } else {
                        let validated = validate_path(path)?;
                        let validated_clone = validated.clone();
                        tokio::task::spawn_blocking(move || {
                            FolderInventory::new(&ferry_home())
                                .inspect_dir(Some(validated_clone))
                                .map_err(OpError::from)
                        })
                        .await
                        .map_err(|e| {
                            OpError::new("internal", e.to_string(), "inspect worker failed")
                        })?
                    }
                }
                Err(e) => Err(e),
            }
        })
    }

    fn list_folders(&self) -> BoxFuture<'_, Result<Vec<FolderRecord>, OpError>> {
        let client = self.client.clone();
        let fallback = self.fallback.clone();
        Box::pin(async move {
            match client.list_folders().await {
                Ok(folders) => Ok(folders),
                Err(e) if e.is_transport() => {
                    if let Some(fb) = fallback {
                        fb.list_folders().await
                    } else {
                        tokio::task::spawn_blocking(|| {
                            FolderInventory::new(&ferry_home())
                                .list()
                                .map_err(OpError::from)
                        })
                        .await
                        .map_err(|e| OpError::new("internal", e.to_string(), "list worker failed"))?
                    }
                }
                Err(e) => Err(e),
            }
        })
    }

    fn register_folder(&self, path: PathBuf) -> BoxFuture<'_, Result<FolderRecord, OpError>> {
        let client = self.client.clone();
        let fallback = self.fallback.clone();
        Box::pin(async move {
            match client.register_folder(path.clone()).await {
                Ok(record) => Ok(record),
                Err(e) if e.is_transport() => {
                    if let Some(fb) = fallback {
                        fb.register_folder(path).await
                    } else {
                        tokio::task::spawn_blocking(move || {
                            FolderInventory::new(&ferry_home())
                                .register(&path)
                                .map_err(OpError::from)
                        })
                        .await
                        .map_err(|e| {
                            OpError::new("internal", e.to_string(), "register worker failed")
                        })?
                    }
                }
                Err(e) => Err(e),
            }
        })
    }

    fn remove_folder(&self, folder_id: String) -> BoxFuture<'_, Result<(), OpError>> {
        let client = self.client.clone();
        let fallback = self.fallback.clone();
        Box::pin(async move {
            match client.remove_folder(folder_id.clone()).await {
                Ok(()) => Ok(()),
                Err(e) if e.is_transport() => {
                    if let Some(fb) = fallback {
                        fb.remove_folder(folder_id).await
                    } else {
                        tokio::task::spawn_blocking(move || {
                            FolderInventory::new(&ferry_home())
                                .unregister(&folder_id)
                                .map_err(OpError::from)
                        })
                        .await
                        .map_err(|e| {
                            OpError::new("internal", e.to_string(), "remove worker failed")
                        })?
                    }
                }
                Err(e) => Err(e),
            }
        })
    }
}

impl SessionDomain for AutoBackend {
    fn start_pin(
        &self,
        paths: Vec<String>,
        hours: Option<u64>,
    ) -> BoxFuture<'_, Result<PinRecord, OpError>> {
        let client = self.client.clone();
        let fallback = self.fallback.clone();
        Box::pin(async move {
            match client.start_pin(paths.clone(), hours).await {
                Ok(pin) => Ok(pin),
                Err(e) if e.is_transport() => {
                    if let Some(fb) = fallback {
                        fb.start_pin(paths, hours).await
                    } else {
                        Err(e)
                    }
                }
                Err(e) => Err(e),
            }
        })
    }

    fn stop_pin(&self) -> BoxFuture<'_, Result<PinStopSummary, OpError>> {
        let client = self.client.clone();
        let fallback = self.fallback.clone();
        Box::pin(async move {
            match client.stop_pin().await {
                Ok(summary) => Ok(summary),
                Err(e) if e.is_transport() => {
                    if let Some(fb) = fallback {
                        fb.stop_pin().await
                    } else {
                        Err(e)
                    }
                }
                Err(e) => Err(e),
            }
        })
    }

    fn release_pin(&self) -> BoxFuture<'_, Result<PinReleaseSummary, OpError>> {
        let client = self.client.clone();
        let fallback = self.fallback.clone();
        Box::pin(async move {
            match client.release_pin().await {
                Ok(summary) => Ok(summary),
                Err(e) if e.is_transport() => {
                    if let Some(fb) = fallback {
                        fb.release_pin().await
                    } else {
                        Err(e)
                    }
                }
                Err(e) => Err(e),
            }
        })
    }

    fn share_initiate(
        &self,
        folder: Option<PathBuf>,
        i_know: bool,
    ) -> BoxFuture<'_, Result<ShareOffer, OpError>> {
        let client = self.client.clone();
        let fallback = self.fallback.clone();
        Box::pin(async move {
            if let Some(fb) = fallback {
                fb.share_initiate(folder, i_know).await
            } else {
                client.share_initiate(folder, i_know).await
            }
        })
    }

    fn share_status(&self, folder: Option<PathBuf>) -> BoxFuture<'_, Result<ShareStatus, OpError>> {
        let client = self.client.clone();
        let fallback = self.fallback.clone();
        Box::pin(async move {
            if let Some(fb) = fallback {
                fb.share_status(folder).await
            } else {
                client.share_status(folder).await
            }
        })
    }

    fn pair_accept(
        &self,
        code_or_payload: String,
        dir: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<PairResult, OpError>> {
        let client = self.client.clone();
        let fallback = self.fallback.clone();
        Box::pin(async move {
            if let Some(fb) = fallback {
                fb.pair_accept(code_or_payload, dir).await
            } else {
                client.pair_accept(code_or_payload, dir).await
            }
        })
    }

    fn create_pairing_session(
        &self,
        req: CreatePairingRequest,
    ) -> BoxFuture<'_, Result<CreatePairingResponse, OpError>> {
        let client = self.client.clone();
        let fallback = self.fallback.clone();
        Box::pin(async move {
            match client.create_pairing_session(req.clone()).await {
                Ok(resp) => Ok(resp),
                Err(e) if e.is_transport() => {
                    if let Some(fb) = fallback {
                        fb.create_pairing_session(req).await
                    } else {
                        Err(e)
                    }
                }
                Err(e) => Err(e),
            }
        })
    }

    fn join_pairing_session(
        &self,
        req: JoinPairingRequest,
    ) -> BoxFuture<'_, Result<PairResult, OpError>> {
        let client = self.client.clone();
        let fallback = self.fallback.clone();
        Box::pin(async move {
            match client.join_pairing_session(req.clone()).await {
                Ok(res) => Ok(res),
                Err(e) if e.is_transport() => {
                    if let Some(fb) = fallback {
                        fb.join_pairing_session(req).await
                    } else {
                        Err(e)
                    }
                }
                Err(e) => Err(e),
            }
        })
    }
}

