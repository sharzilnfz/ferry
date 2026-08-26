use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{json, Value};

use ferry_pin::PinManager;
use ferry_store::agreement::AgreementLedger;
use ferry_store::format::hex as hex_str;

use super::{OpError, UiState};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Pluggable backend seam powering `DashboardServer`.
pub trait DashboardBackend: Send + Sync + 'static {
    fn get_status(&self) -> BoxFuture<'_, Result<Value, OpError>>;
    fn list_conflicts(&self) -> BoxFuture<'_, Result<Value, OpError>>;
    fn start_pin(&self, paths: Option<Vec<String>>) -> BoxFuture<'_, Result<Value, OpError>>;
    fn stop_pin(&self) -> BoxFuture<'_, Result<Value, OpError>>;
    fn release_pin(&self) -> BoxFuture<'_, Result<Value, OpError>>;
    fn share(&self, folder: Option<PathBuf>, i_know: bool)
        -> BoxFuture<'_, Result<Value, OpError>>;
    fn share_status(&self, folder: Option<PathBuf>) -> BoxFuture<'_, Result<Value, OpError>>;
    fn pair_accept(
        &self,
        payload_path: PathBuf,
        dir: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<Value, OpError>>;
}

/// Direct backend adapter wrapping cached in-memory daemon state (`Arc<UiState>`).
pub struct DirectBackend {
    state: Arc<UiState>,
}

impl DirectBackend {
    #[must_use]
    pub fn new(state: Arc<UiState>) -> Self {
        Self { state }
    }

    #[must_use]
    pub fn state(&self) -> &Arc<UiState> {
        &self.state
    }
}

impl DashboardBackend for DirectBackend {
    fn get_status(&self) -> BoxFuture<'_, Result<Value, OpError>> {
        let st = Arc::clone(&self.state);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || super::status::status_doc(&st))
                .await
                .map_err(|e| {
                    OpError::new(
                        "internal",
                        format!("ui worker: {e}"),
                        "check the daemon's stderr log",
                    )
                })?
        })
    }

    fn list_conflicts(&self) -> BoxFuture<'_, Result<Value, OpError>> {
        let st = Arc::clone(&self.state);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || super::status::conflicts_doc(&st))
                .await
                .map_err(|e| {
                    OpError::new(
                        "internal",
                        format!("ui worker: {e}"),
                        "check the daemon's stderr log",
                    )
                })?
        })
    }

    fn start_pin(&self, paths: Option<Vec<String>>) -> BoxFuture<'_, Result<Value, OpError>> {
        let st = Arc::clone(&self.state);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || super::actions::pin_start(&st, paths))
                .await
                .map_err(|e| {
                    OpError::new(
                        "internal",
                        format!("ui worker: {e}"),
                        "check the daemon's stderr log",
                    )
                })?
        })
    }

    fn stop_pin(&self) -> BoxFuture<'_, Result<Value, OpError>> {
        let st = Arc::clone(&self.state);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || super::actions::pin_stop(&st))
                .await
                .map_err(|e| {
                    OpError::new(
                        "internal",
                        format!("ui worker: {e}"),
                        "check the daemon's stderr log",
                    )
                })?
        })
    }

    fn release_pin(&self) -> BoxFuture<'_, Result<Value, OpError>> {
        let st = Arc::clone(&self.state);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || super::actions::pin_release(&st))
                .await
                .map_err(|e| {
                    OpError::new(
                        "internal",
                        format!("ui worker: {e}"),
                        "check the daemon's stderr log",
                    )
                })?
        })
    }

    fn share(
        &self,
        folder: Option<PathBuf>,
        i_know: bool,
    ) -> BoxFuture<'_, Result<Value, OpError>> {
        let st = Arc::clone(&self.state);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                super::actions::share(&st, folder.as_deref(), i_know)
            })
            .await
            .map_err(|e| {
                OpError::new(
                    "internal",
                    format!("ui worker: {e}"),
                    "check the daemon's stderr log",
                )
            })?
        })
    }

    fn share_status(&self, folder: Option<PathBuf>) -> BoxFuture<'_, Result<Value, OpError>> {
        let st = Arc::clone(&self.state);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                super::actions::share_status(&st, folder.as_deref())
            })
            .await
            .map_err(|e| {
                OpError::new(
                    "internal",
                    format!("ui worker: {e}"),
                    "check the daemon's stderr log",
                )
            })?
        })
    }

    fn pair_accept(
        &self,
        payload_path: PathBuf,
        dir: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<Value, OpError>> {
        let st = Arc::clone(&self.state);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                super::actions::pair_accept(&st, &payload_path, dir.as_deref())
            })
            .await
            .map_err(|e| {
                OpError::new(
                    "internal",
                    format!("ui worker: {e}"),
                    "check the daemon's stderr log",
                )
            })?
        })
    }
}

/// IPC client adapter querying a running daemon over IPC, with disk fallback.
#[derive(Clone, Debug)]
pub struct IpcBackend {
    socket_path: PathBuf,
    fallback_dir: Option<PathBuf>,
}

impl IpcBackend {
    #[must_use]
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            fallback_dir: None,
        }
    }

    #[must_use]
    pub fn with_fallback(mut self, dir: PathBuf) -> Self {
        self.fallback_dir = Some(dir);
        self
    }

    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    #[must_use]
    pub fn fallback_dir(&self) -> Option<&Path> {
        self.fallback_dir.as_deref()
    }
}

async fn query_daemon_status(
    socket_path: &Path,
) -> Result<ferry_ipc::EngineSnapshot, ferry_ipc::IpcError> {
    let mut conn = ferry_ipc::IpcClient::connect(socket_path).await?;
    let initial = conn.recv_message().await?;
    match initial {
        Some(ferry_ipc::DaemonMessage::Snapshot(snap)) => Ok(snap),
        _ => {
            conn.send_command(&ferry_ipc::ClientCommand::GetStatus)
                .await?;
            let resp = conn.recv_message().await?.ok_or_else(|| {
                ferry_ipc::IpcError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "daemon disconnected",
                ))
            })?;
            match resp {
                ferry_ipc::DaemonMessage::Snapshot(snap) => Ok(snap),
                ferry_ipc::DaemonMessage::Error { code, message } => {
                    Err(ferry_ipc::IpcError::Protocol(format!("{code}: {message}")))
                }
                other => Err(ferry_ipc::IpcError::Protocol(format!(
                    "unexpected response: {other:?}"
                ))),
            }
        }
    }
}

async fn query_daemon_conflicts(socket_path: &Path) -> Result<Vec<Value>, ferry_ipc::IpcError> {
    let mut conn = ferry_ipc::IpcClient::connect(socket_path).await?;
    let _initial = conn.recv_message().await?;
    conn.send_command(&ferry_ipc::ClientCommand::ListConflicts)
        .await?;
    let resp = conn.recv_message().await?.ok_or_else(|| {
        ferry_ipc::IpcError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "daemon disconnected",
        ))
    })?;
    match resp {
        ferry_ipc::DaemonMessage::Ack {
            message: Some(json_str),
            ..
        } => {
            let entries: Vec<Value> = serde_json::from_str(&json_str).unwrap_or_default();
            Ok(entries)
        }
        ferry_ipc::DaemonMessage::Ack { message: None, .. } => Ok(Vec::new()),
        ferry_ipc::DaemonMessage::Error { code, message } => {
            Err(ferry_ipc::IpcError::Protocol(format!("{code}: {message}")))
        }
        other => Err(ferry_ipc::IpcError::Protocol(format!(
            "unexpected response: {other:?}"
        ))),
    }
}

async fn query_daemon_command(
    socket_path: &Path,
    cmd: ferry_ipc::ClientCommand,
) -> Result<ferry_ipc::DaemonMessage, ferry_ipc::IpcError> {
    let mut conn = ferry_ipc::IpcClient::connect(socket_path).await?;
    let _initial = conn.recv_message().await?;
    conn.send_command(&cmd).await?;
    let resp = conn.recv_message().await?.ok_or_else(|| {
        ferry_ipc::IpcError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "daemon disconnected",
        ))
    })?;
    Ok(resp)
}

/// Format an `EngineSnapshot` into the standard `status` JSON document shape.
#[must_use]
pub fn snapshot_to_status_doc(snap: &ferry_ipc::EngineSnapshot) -> Value {
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
        "conflicts": snap.conflicts,
    })
}

impl DashboardBackend for IpcBackend {
    fn get_status(&self) -> BoxFuture<'_, Result<Value, OpError>> {
        let socket_path = self.socket_path.clone();
        let fallback_dir = self.fallback_dir.clone();
        Box::pin(async move {
            if let Ok(snap) = query_daemon_status(&socket_path).await {
                return Ok(snapshot_to_status_doc(&snap));
            }

            if let Some(dir) = fallback_dir {
                tokio::task::spawn_blocking(move || read_status_from_disk(&dir))
                    .await
                    .map_err(|e| {
                        OpError::new(
                            "internal",
                            format!("disk status worker: {e}"),
                            "check stderr",
                        )
                    })?
            } else {
                Err(OpError::new(
                    "not-found",
                    "daemon is not running and no fallback folder provided",
                    "run ferry daemon or specify folder",
                ))
            }
        })
    }

    fn list_conflicts(&self) -> BoxFuture<'_, Result<Value, OpError>> {
        let socket_path = self.socket_path.clone();
        let fallback_dir = self.fallback_dir.clone();
        Box::pin(async move {
            if let Ok(entries) = query_daemon_conflicts(&socket_path).await {
                let folder = fallback_dir
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                return Ok(json!({
                    "command": "conflicts",
                    "folder": folder,
                    "entries": entries,
                }));
            }

            if let Some(dir) = fallback_dir {
                tokio::task::spawn_blocking(move || read_conflicts_from_disk(&dir))
                    .await
                    .map_err(|e| {
                        OpError::new(
                            "internal",
                            format!("disk conflicts worker: {e}"),
                            "check stderr",
                        )
                    })?
            } else {
                Err(OpError::new(
                    "not-found",
                    "daemon is not running and no fallback folder provided",
                    "run ferry daemon or specify folder",
                ))
            }
        })
    }

    fn start_pin(&self, paths: Option<Vec<String>>) -> BoxFuture<'_, Result<Value, OpError>> {
        let socket_path = self.socket_path.clone();
        let fallback_dir = self.fallback_dir.clone();
        Box::pin(async move {
            let cmd = ferry_ipc::ClientCommand::StartPin {
                paths: paths.clone().unwrap_or_default(),
            };
            if let Ok(resp) = query_daemon_command(&socket_path, cmd).await {
                match resp {
                    ferry_ipc::DaemonMessage::Ack { command, message } => {
                        let folder = fallback_dir
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default();
                        return Ok(json!({
                            "command": "pin",
                            "action": "start",
                            "folder": folder,
                            "paths": paths.unwrap_or_else(|| vec!["*".to_string()]),
                            "status": command,
                            "message": message,
                        }));
                    }
                    ferry_ipc::DaemonMessage::Error { message, .. } => {
                        return Err(OpError::new(
                            "pin-active",
                            message,
                            "stop or release existing pin first",
                        ));
                    }
                    _ => {}
                }
            }

            if let Some(dir) = fallback_dir {
                tokio::task::spawn_blocking(move || pin_start_disk(&dir, paths))
                    .await
                    .map_err(|e| {
                        OpError::new(
                            "internal",
                            format!("disk pin start worker: {e}"),
                            "check stderr",
                        )
                    })?
            } else {
                Err(OpError::new(
                    "not-found",
                    "daemon is not running and no fallback folder provided",
                    "run ferry daemon or specify folder",
                ))
            }
        })
    }

    fn stop_pin(&self) -> BoxFuture<'_, Result<Value, OpError>> {
        let fallback_dir = self.fallback_dir.clone();
        Box::pin(async move {
            if let Some(dir) = fallback_dir {
                tokio::task::spawn_blocking(move || pin_stop_disk(&dir))
                    .await
                    .map_err(|e| {
                        OpError::new(
                            "internal",
                            format!("disk pin stop worker: {e}"),
                            "check stderr",
                        )
                    })?
            } else {
                Err(OpError::new(
                    "not-found",
                    "no fallback folder provided for pin stop",
                    "specify folder",
                ))
            }
        })
    }

    fn release_pin(&self) -> BoxFuture<'_, Result<Value, OpError>> {
        let socket_path = self.socket_path.clone();
        let fallback_dir = self.fallback_dir.clone();
        Box::pin(async move {
            let cmd = ferry_ipc::ClientCommand::ReleasePin;
            if let Ok(resp) = query_daemon_command(&socket_path, cmd).await {
                match resp {
                    ferry_ipc::DaemonMessage::Ack { command, message } => {
                        let folder = fallback_dir
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default();
                        return Ok(json!({
                            "command": "pin",
                            "action": "release",
                            "folder": folder,
                            "status": command,
                            "message": message,
                        }));
                    }
                    ferry_ipc::DaemonMessage::Error { message, .. } => {
                        return Err(OpError::new(
                            "not-implemented",
                            message,
                            "reconciliation error",
                        ));
                    }
                    _ => {}
                }
            }

            if let Some(dir) = fallback_dir {
                tokio::task::spawn_blocking(move || pin_release_disk(&dir))
                    .await
                    .map_err(|e| {
                        OpError::new(
                            "internal",
                            format!("disk pin release worker: {e}"),
                            "check stderr",
                        )
                    })?
            } else {
                Err(OpError::new(
                    "not-found",
                    "daemon is not running and no fallback folder provided",
                    "run ferry daemon or specify folder",
                ))
            }
        })
    }

    fn share(
        &self,
        folder: Option<PathBuf>,
        i_know: bool,
    ) -> BoxFuture<'_, Result<Value, OpError>> {
        let target = folder.or_else(|| self.fallback_dir.clone());
        Box::pin(async move {
            if let Some(dir) = target {
                tokio::task::spawn_blocking(move || share_folder_disk(&dir, i_know))
                    .await
                    .map_err(|e| {
                        OpError::new("internal", format!("share worker: {e}"), "check stderr")
                    })?
            } else {
                Err(OpError::new(
                    "bad-request",
                    "folder path is required",
                    "specify a folder to share",
                ))
            }
        })
    }

    fn share_status(&self, folder: Option<PathBuf>) -> BoxFuture<'_, Result<Value, OpError>> {
        let target = folder.or_else(|| self.fallback_dir.clone());
        Box::pin(async move {
            if let Some(dir) = target {
                tokio::task::spawn_blocking(move || share_status_disk(&dir))
                    .await
                    .map_err(|e| {
                        OpError::new(
                            "internal",
                            format!("share status worker: {e}"),
                            "check stderr",
                        )
                    })?
            } else {
                Err(OpError::new(
                    "bad-request",
                    "folder path is required",
                    "specify a folder to check share status",
                ))
            }
        })
    }

    fn pair_accept(
        &self,
        payload_path: PathBuf,
        dir: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<Value, OpError>> {
        let target_dir = dir.or_else(|| self.fallback_dir.clone());
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                pair_accept_disk(&payload_path, target_dir.as_deref())
            })
            .await
            .map_err(|e| OpError::new("internal", format!("pair worker: {e}"), "check stderr"))?
        })
    }
}

// ---------------------------------------------------------------------------
// Disk fallback operations
// ---------------------------------------------------------------------------

const PAIR_TIMEOUT_SECS: u64 = 120;

fn resolve_ferry_home() -> Result<PathBuf, OpError> {
    if let Some(v) = std::env::var_os("FERRY_HOME") {
        let p = PathBuf::from(&v);
        if !p.as_os_str().is_empty() {
            return Ok(p);
        }
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            OpError::new(
                "no-home",
                "cannot locate a home directory",
                "set HOME, or point FERRY_HOME at a directory to hold Ferry state",
            )
        })?;
    Ok(home.join(".ferry"))
}

fn load_device_identity() -> Result<ferry_crypto::identity::DeviceIdentity, OpError> {
    let home = resolve_ferry_home()?;
    ferry_crypto::identity::load_or_create(&home.join("identity"))
        .map_err(|e| OpError::new("identity-corrupt", e.to_string(), "restore device.key"))
}

fn one_shot_scan(
    opened: &ferry_folder::folder::OpenFolder,
    device_id: [u8; 32],
) -> Result<(String, ferry_scan::walk::PassStats), OpError> {
    let poly = ferry_store::chunker::ValidatedPoly::try_from(opened.poly).map_err(|e| {
        OpError::new(
            "poly-invalid",
            e.to_string(),
            "the folder's polynomial record is corrupt; restore this store from a known-good backup",
        )
    })?;
    let handle = ferry_scan::StoreHandle {
        store: opened.store.clone(),
        poly,
        folder_id: opened.folder_id,
        device_id,
    };
    let rules = std::sync::Arc::new(
        ferry_folder::folder::load_rules(&opened.root, &opened.settings)
            .map_err(super::actions::folder_err)?,
    );
    let engine = ferry_scan::ScanEngine::watch_with(
        &opened.root,
        handle,
        ferry_scan::ScanConfig::default(),
        rules,
    )
    .map_err(|e| {
        OpError::new(
            "scan",
            e.to_string(),
            "check the folder exists and is readable",
        )
    })?;
    let current = engine.current().ok_or_else(|| {
        OpError::new(
            "scan",
            "scanner produced no initial state",
            "retry the command",
        )
    })?;
    let manifest_id = hex_str(&current.manifest_id);
    let stats = current.stats.clone();
    drop(engine);
    Ok((manifest_id, stats))
}

fn read_status_from_disk(folder: &Path) -> Result<Value, OpError> {
    let identity = load_device_identity()?;
    let opened =
        ferry_folder::folder::open_folder(folder, &identity).map_err(super::actions::folder_err)?;
    let (manifest_id, stats) = one_shot_scan(&opened, *identity.public())?;
    let device_id = hex_str(identity.public());
    let folder_id = hex_str(&opened.folder_id);

    let ledger = AgreementLedger::new(opened.state_dir());
    let records = ledger
        .list_folder(&opened.folder_id)
        .map_err(|e| OpError::new("agreement-state", e.to_string(), "check .ferry permissions"))?;

    let mut peers = Vec::new();
    for (dev, rec) in records {
        peers.push(json!({
            "device_id": hex_str(&dev),
            "last_agreed_manifest_id": hex_str(&rec.manifest_id),
            "agreed_at": Some(ferry_platform::time::fmt_rfc3339(rec.agreed_sec)),
            "connectivity": "unknown",
        }));
    }
    peers.sort_by(|a, b| a["device_id"].as_str().cmp(&b["device_id"].as_str()));

    let pin_summary = PinManager::new(opened.state_dir())
        .summary()
        .map_err(super::actions::pin_err)?;
    let pin = json!({
        "state": pin_summary.state,
        "holding": pin_summary.holding,
        "paths": pin_summary.paths,
    });

    let mut held_by_peer = serde_json::Map::new();
    for (peer, paths) in pin_summary.held_by_peer {
        held_by_peer.insert(peer, json!(paths));
    }
    let held_total = pin_summary.total_held_paths;

    let conflicts = ferry_sync_engine::list_conflicts(&opened.state_dir()).map_err(|e| {
        OpError::new(
            "conflict-log",
            e.to_string(),
            "fix or archive .ferry/conflicts.jsonl",
        )
    })?;

    Ok(json!({
        "command": "status",
        "folder": opened.root.display().to_string(),
        "folder_id": folder_id,
        "device_id": device_id,
        "manifest_id": manifest_id,
        "scanned": {
            "files": stats.files,
            "dirs": stats.dirs,
            "symlinks": stats.symlinks,
            "bytes_chunked": stats.bytes_chunked,
        },
        "pending_changes": Value::Null,
        "pin": pin,
        "held_changes": held_total,
        "held_by_peer": Value::Object(held_by_peer),
        "peers": peers,
        "conflicts": conflicts.len(),
    }))
}

fn read_conflicts_from_disk(folder: &Path) -> Result<Value, OpError> {
    let state_dir = folder.join(".ferry");
    let entries = if state_dir.exists() {
        ferry_sync_engine::list_conflicts(&state_dir)
            .map_err(|e| {
                OpError::new(
                    "conflict-log",
                    e.to_string(),
                    "fix or archive .ferry/conflicts.jsonl",
                )
            })?
            .into_iter()
            .map(|e| serde_json::to_value(e).unwrap_or(Value::Null))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    Ok(json!({
        "command": "conflicts",
        "folder": folder.display().to_string(),
        "entries": entries,
    }))
}

fn share_folder_disk(target: &Path, i_know: bool) -> Result<Value, OpError> {
    let identity = load_device_identity()?;
    let opened =
        ferry_folder::folder::open_folder(target, &identity).map_err(super::actions::folder_err)?;
    let rules = ferry_folder::folder::load_rules(&opened.root, &opened.settings)
        .map_err(super::actions::folder_err)?;
    let warnings_raw = ferry_ignore::secrets::scan_for_secrets(&rules, &opened.root);
    let warnings: Vec<Value> = warnings_raw
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

    if !warnings.is_empty() && !i_know {
        let mut msg = format!(
            "{} secret risk(s) would SYNC to other devices:\n",
            warnings.len()
        );
        for w in warnings_raw.iter().take(20) {
            let loc = w.line.map(|n| format!(":{n}")).unwrap_or_default();
            let _ = writeln!(
                msg,
                "  SECRET RISK [{}] {}{} — {}",
                w.class.label(),
                w.path.join("/"),
                loc,
                w.preview
            );
        }
        if warnings_raw.len() > 20 {
            let _ = writeln!(msg, "  … and {} more", warnings_raw.len() - 20);
        }
        return Err(OpError::new(
            "secrets-found",
            msg.trim_end().to_string(),
            "review each path: exclude it (`ferry ignore '<pattern>'`) or accept the risk with --i-know",
        )
        .with_detail(json!({ "warnings": warnings })));
    }

    let pending = ferry_folder::pairing::initiate_begin(&opened, &identity)
        .map_err(super::actions::folder_err)?;
    let warnings_reviewed = !warnings.is_empty();
    let short_code = pending.short_code.clone();
    let dot = ferry_folder::folder::dot_dir(&opened.root);
    let _ = std::fs::remove_file(dot.join(ferry_folder::pairing::RESPONSE_SUFFIX));
    let _ = std::fs::remove_file(dot.join(ferry_folder::pairing::GRANT_SUFFIX));
    std::fs::write(&pending.offer_path, &pending.offer_bytes).map_err(OpError::from)?;

    Ok(json!({
        "command": "share",
        "role": "initiate",
        "status": "pending",
        "folder": opened.root.display().to_string(),
        "folder_id": hex_str(&opened.folder_id),
        "short_code": short_code,
        "offer_file": pending.offer_path.display().to_string(),
        "warnings_reviewed": warnings_reviewed,
        "warnings": warnings,
    }))
}

fn share_status_disk(target: &Path) -> Result<Value, OpError> {
    let identity = load_device_identity()?;
    let opened =
        ferry_folder::folder::open_folder(target, &identity).map_err(super::actions::folder_err)?;
    let dot = ferry_folder::folder::dot_dir(&opened.root);
    let offer_path = dot.join(ferry_folder::pairing::OFFER_SUFFIX);
    let response_path = dot.join(ferry_folder::pairing::RESPONSE_SUFFIX);

    let short_code = if let Ok(offer_bytes) = std::fs::read(&offer_path) {
        ferry_crypto::pairing::PairingOffer::parse(&offer_bytes)
            .ok()
            .map(|o| o.short_code(ferry_crypto::pairing::TransportHints(0)))
    } else {
        None
    };

    if !response_path.exists() {
        return Ok(json!({
            "command": "share",
            "role": "initiate",
            "status": "pending",
            "folder": opened.root.display().to_string(),
            "folder_id": hex_str(&opened.folder_id),
            "short_code": short_code,
            "offer_file": offer_path.display().to_string(),
        }));
    }

    match ferry_folder::pairing::initiate_check(&opened, &identity)
        .map_err(super::actions::folder_err)?
    {
        Some(completed) => Ok(json!({
            "command": "share",
            "role": "initiate",
            "status": "completed",
            "folder": opened.root.display().to_string(),
            "folder_id": hex_str(&opened.folder_id),
            "peer_device_id": hex_str(&completed.peer_device_id),
            "short_code": completed.short_code,
            "offer_file": completed.offer_path.display().to_string(),
            "grant_file": completed.grant_path.display().to_string(),
        })),
        None => Ok(json!({
            "command": "share",
            "role": "initiate",
            "status": "pending",
            "folder": opened.root.display().to_string(),
            "folder_id": hex_str(&opened.folder_id),
            "short_code": short_code,
            "offer_file": offer_path.display().to_string(),
        })),
    }
}

fn pair_accept_disk(payload_path: &Path, dir: Option<&Path>) -> Result<Value, OpError> {
    let identity = load_device_identity()?;
    let pending = ferry_folder::pairing::accept_begin(&identity, payload_path, dir)
        .map_err(super::actions::folder_err)?;
    let expected_short_code = pending.expected_short_code.clone();
    let accepted = ferry_folder::pairing::accept_complete(pending, &identity, PAIR_TIMEOUT_SECS)
        .map_err(super::actions::folder_err)?;

    Ok(json!({
        "command": "pair",
        "role": "accept",
        "status": "completed",
        "folder": accepted.folder.display().to_string(),
        "folder_id": hex_str(&accepted.folder_id),
        "device_id": hex_str(identity.public()),
        "expected_short_code": expected_short_code,
    }))
}

fn pin_start_disk(folder: &Path, paths: Option<Vec<String>>) -> Result<Value, OpError> {
    let identity = load_device_identity()?;
    let opened =
        ferry_folder::folder::open_folder(folder, &identity).map_err(super::actions::folder_err)?;

    let mut base_agreements = BTreeMap::new();
    for (dev, rec) in AgreementLedger::new(opened.state_dir())
        .list_folder(&opened.folder_id)
        .map_err(|e| OpError::new("agreement-state", e.to_string(), "check .ferry permissions"))?
    {
        base_agreements.insert(hex_str(&dev), hex_str(&rec.manifest_id));
    }

    let base_peers_recorded = base_agreements.len();
    let pin_mgr = PinManager::new(opened.state_dir());
    let pid = std::process::id();
    let record = pin_mgr
        .start_session(
            paths.unwrap_or_default(),
            pid,
            &hex_str(identity.public()),
            base_agreements,
        )
        .map_err(super::actions::pin_err)?;

    Ok(json!({
        "command": "pin",
        "action": "start",
        "folder": opened.root.display().to_string(),
        "device_id": hex_str(identity.public()),
        "pid": pid,
        "paths": record.paths,
        "started_at": ferry_platform::time::fmt_rfc3339(record.started_sec),
        "base_peers_recorded": base_peers_recorded,
    }))
}

fn pin_stop_disk(folder: &Path) -> Result<Value, OpError> {
    let identity = load_device_identity()?;
    let opened =
        ferry_folder::folder::open_folder(folder, &identity).map_err(super::actions::folder_err)?;
    let pin_mgr = PinManager::new(opened.state_dir());
    let summary = pin_mgr.summary().map_err(super::actions::pin_err)?;
    let was_pinned = summary.holding || summary.state == "active" || summary.state == "stale";
    let _ = pin_mgr.stop_session().map_err(super::actions::pin_err)?;

    let mut by_peer = serde_json::Map::new();
    for (peer, paths) in &summary.held_by_peer {
        by_peer.insert(peer.clone(), json!(paths.len()));
    }

    Ok(json!({
        "command": "pin",
        "action": "stop",
        "folder": opened.root.display().to_string(),
        "was_pinned": was_pinned,
        "held_changes": summary.total_held_paths,
        "held_by_peer": Value::Object(by_peer),
    }))
}

fn pin_release_disk(folder: &Path) -> Result<Value, OpError> {
    let identity = load_device_identity()?;
    let opened =
        ferry_folder::folder::open_folder(folder, &identity).map_err(super::actions::folder_err)?;
    let pin_mgr = PinManager::new(opened.state_dir());
    let summary = pin_mgr.summary().map_err(super::actions::pin_err)?;

    if summary.total_held_paths > 0 {
        return Err(OpError::new(
            "not-implemented",
            format!(
                "{} held change(s) need reconciliation via the CLI",
                summary.total_held_paths
            ),
            "run `ferry pin release` in this folder on the command line",
        ));
    }

    let pin_ended = pin_mgr.stop_session().map_err(super::actions::pin_err)?;
    let conflicts_total = ferry_sync_engine::list_conflicts(&opened.state_dir())
        .map_err(|e| OpError::new("conflict-log", e.to_string(), "fix .ferry/conflicts.jsonl"))?
        .len();

    Ok(json!({
        "command": "pin",
        "action": "release",
        "folder": opened.root.display().to_string(),
        "peers": [],
        "quarantined": 0,
        "conflicts_recorded": 0,
        "ops_applied": 0,
        "pin_ended": pin_ended,
        "conflicts_total": conflicts_total,
    }))
}
