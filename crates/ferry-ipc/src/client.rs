use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, mpsc, oneshot, watch, Mutex};

use crate::backend::{
    BoxFuture, InventoryDomain, OpError, PairResult, PinRecord, PinReleaseSummary, PinStopSummary,
    SessionDomain, ShareOffer, ShareStatus, StatusDomain, UiEvent, UiEventStream,
    DAEMON_UNREACHABLE,
};
use crate::error::IpcError;
use crate::framing::{IpcReceiver, IpcSender};
use crate::pairing::{CreatePairingRequest, CreatePairingResponse, JoinPairingRequest};
use crate::protocol::{
    ClientCommand, ConflictEntry, DaemonMessage, DiscoveredDeviceView, EngineSnapshot,
};
use crate::{validate_path, FolderRecord, ListDirectoryResponse};

const SUPERVISOR_POLL: Duration = Duration::from_millis(500);

const EVENT_CHANNEL_CAPACITY: usize = 256;

#[derive(Debug, Clone, Copy)]
pub struct ReconnectPolicy {
    pub attempts: u32,

    pub base_delay: Duration,

    pub max_delay: Duration,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            attempts: 2,
            base_delay: Duration::from_millis(50),
            max_delay: Duration::from_millis(400),
        }
    }
}

#[derive(Clone)]
struct LiveConn {
    cmd_tx: mpsc::Sender<Outbound>,
    pending_tx: mpsc::Sender<Pending>,
    closed: watch::Receiver<bool>,
    gen: u64,
}

struct Outbound {
    command: ClientCommand,
}

struct Pending {
    command: ClientCommand,
    reply: oneshot::Sender<Result<DaemonMessage, OpError>>,
}

struct Inner {
    socket_path: PathBuf,
    policy: ReconnectPolicy,
    conn: Mutex<Option<LiveConn>>,
    event_tx: broadcast::Sender<UiEvent>,

    conn_gen: watch::Sender<u64>,
    supervisor_started: AtomicBool,
}

#[derive(Clone)]
pub struct DaemonClient {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for DaemonClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonClient")
            .field("socket_path", &self.inner.socket_path)
            .finish()
    }
}

fn unreachable_err(detail: impl std::fmt::Display) -> OpError {
    OpError::new(
        DAEMON_UNREACHABLE,
        detail.to_string(),
        "start the daemon or check the socket path",
    )
}

fn unexpected_response(msg: &DaemonMessage) -> OpError {
    OpError::new("protocol", format!("unexpected response {msg:?}"), "retry")
}

#[cfg(unix)]
type ConnHalves = (
    IpcSender<tokio::io::WriteHalf<tokio::net::UnixStream>>,
    IpcReceiver<tokio::io::ReadHalf<tokio::net::UnixStream>>,
);

#[cfg(windows)]
type ConnHalves = (
    IpcSender<tokio::io::WriteHalf<tokio::net::windows::named_pipe::NamedPipeClient>>,
    IpcReceiver<tokio::io::ReadHalf<tokio::net::windows::named_pipe::NamedPipeClient>>,
);

#[cfg(unix)]
async fn platform_connect(path: &Path) -> Result<ConnHalves, IpcError> {
    Ok(crate::transport::unix::IpcClient::connect(path)
        .await?
        .split())
}

#[cfg(windows)]
async fn platform_connect(path: &Path) -> Result<ConnHalves, IpcError> {
    Ok(crate::transport::windows::IpcClient::connect(path)
        .await?
        .split())
}

impl DaemonClient {
    #[must_use]
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self::with_policy(socket_path, ReconnectPolicy::default())
    }

    #[must_use]
    pub fn with_policy(socket_path: impl Into<PathBuf>, policy: ReconnectPolicy) -> Self {
        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let (conn_gen, _) = watch::channel(0);
        Self {
            inner: Arc::new(Inner {
                socket_path: socket_path.into(),
                policy,
                conn: Mutex::new(None),
                event_tx,
                conn_gen,
                supervisor_started: AtomicBool::new(false),
            }),
        }
    }

    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.inner.socket_path
    }

    pub fn subscribe(&self) -> broadcast::Receiver<UiEvent> {
        self.inner.event_tx.subscribe()
    }

    pub async fn call(&self, command: ClientCommand) -> Result<DaemonMessage, OpError> {
        let conn = self.ensure_connected().await?;
        let (reply_tx, reply_rx) = oneshot::channel();

        let pending = Pending {
            command: command.clone(),
            reply: reply_tx,
        };
        if conn.pending_tx.send(pending).await.is_err() {
            self.mark_conn_lost(conn.gen).await;
            return Err(unreachable_err("daemon connection closed"));
        }
        if conn.cmd_tx.send(Outbound { command }).await.is_err() {
            self.mark_conn_lost(conn.gen).await;
            return Err(unreachable_err("daemon connection closed"));
        }
        match reply_rx.await {
            Ok(res) => res,
            Err(_) => {
                self.mark_conn_lost(conn.gen).await;
                Err(unreachable_err("daemon connection closed"))
            }
        }
    }

    pub async fn event_stream(&self) -> Result<UiEventStream, OpError> {
        self.ensure_connected().await?;
        Ok(UiEventStream::new(self.inner.event_tx.subscribe()))
    }

    async fn live_conn(&self) -> Option<LiveConn> {
        let guard = self.inner.conn.lock().await;
        guard.clone().filter(|c| !*c.closed.borrow())
    }

    async fn ensure_connected(&self) -> Result<LiveConn, OpError> {
        let policy = self.inner.policy;
        let mut delay = policy.base_delay;
        let mut last_err: Option<OpError> = None;
        for attempt in 0..policy.attempts {
            if let Some(conn) = self.live_conn().await {
                return Ok(conn);
            }
            match self.connect_once().await {
                Ok(conn) => return Ok(conn),
                Err(e) => {
                    last_err = Some(e);
                    if attempt + 1 < policy.attempts {
                        tokio::time::sleep(delay).await;
                        delay = (delay * 2).min(policy.max_delay);
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| unreachable_err("daemon socket unreachable")))
    }

    async fn connect_once(&self) -> Result<LiveConn, OpError> {
        let mut guard = self.inner.conn.lock().await;
        if let Some(c) = guard.as_ref() {
            if !*c.closed.borrow() {
                return Ok(c.clone());
            }
        }
        let (sender, mut receiver) = platform_connect(&self.inner.socket_path)
            .await
            .map_err(unreachable_err)?;

        match receiver.recv_message().await {
            Ok(Some(msg)) => {
                if let Some(event) = push_event_of(&msg) {
                    let _ = self.inner.event_tx.send(event);
                }
            }
            Ok(None) => return Err(unreachable_err("daemon closed connection immediately")),
            Err(e) => return Err(unreachable_err(e)),
        }
        let gen = self.inner.conn_gen.borrow().wrapping_add(1);
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let (pending_tx, pending_rx) = mpsc::channel(64);
        let (closed_tx, closed_rx) = watch::channel(false);
        let live = LiveConn {
            cmd_tx,
            pending_tx,
            closed: closed_rx,
            gen,
        };
        *guard = Some(live.clone());
        drop(guard);
        let _ = self.inner.conn_gen.send(gen);

        tokio::spawn(writer_task(sender, cmd_rx));
        let weak = Arc::downgrade(&self.inner);
        let event_tx = self.inner.event_tx.clone();
        tokio::spawn(reader_task(
            receiver, pending_rx, event_tx, closed_tx, weak, gen,
        ));
        self.spawn_supervisor();
        Ok(live)
    }

    async fn mark_conn_lost(&self, gen: u64) {
        let mut guard = self.inner.conn.lock().await;
        if let Some(c) = guard.as_ref() {
            if c.gen == gen {
                *guard = None;
            }
        }
        drop(guard);
        let _ = self.inner.conn_gen.send(gen.wrapping_add(1));
    }

    fn spawn_supervisor(&self) {
        if self.inner.supervisor_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            loop {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                let mut gen_rx = inner.conn_gen.subscribe();
                drop(inner);
                if gen_rx.changed().await.is_err() {
                    return;
                }
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                let has_live = {
                    let guard = inner.conn.lock().await;
                    guard.as_ref().is_some_and(|c| !*c.closed.borrow())
                };
                drop(inner);
                if has_live {
                    continue;
                }
                loop {
                    let Some(inner) = weak.upgrade() else {
                        return;
                    };
                    let client = DaemonClient { inner };
                    if client.ensure_connected().await.is_ok() {
                        break;
                    }
                    tokio::time::sleep(SUPERVISOR_POLL).await;
                }
            }
        });
    }

    pub async fn list_discovered_devices(&self) -> Result<Vec<DiscoveredDeviceView>, OpError> {
        let snap = self.get_status().await?;
        Ok(snap.discovered_devices)
    }
}

async fn writer_task<W: AsyncWrite + Unpin + Send + 'static>(
    mut sender: IpcSender<W>,
    mut cmd_rx: mpsc::Receiver<Outbound>,
) {
    while let Some(out) = cmd_rx.recv().await {
        if sender.send_command(&out.command).await.is_err() {
            break;
        }
    }
}

async fn reader_task<R: AsyncRead + Unpin + Send + 'static>(
    mut receiver: IpcReceiver<R>,
    mut pending_rx: mpsc::Receiver<Pending>,
    event_tx: broadcast::Sender<UiEvent>,
    closed_tx: watch::Sender<bool>,
    weak: Weak<Inner>,
    gen: u64,
) {
    let mut queue: std::collections::VecDeque<Pending> = std::collections::VecDeque::new();
    while let Ok(Some(msg)) = receiver.recv_message().await {
        while let Ok(pending) = pending_rx.try_recv() {
            queue.push_back(pending);
        }
        route_message(msg, &mut queue, &event_tx);
    }
    let _ = closed_tx.send(true);
    while let Some(pending) = queue.pop_front() {
        let _ = pending
            .reply
            .send(Err(unreachable_err("daemon connection closed")));
    }
    while let Ok(pending) = pending_rx.try_recv() {
        let _ = pending
            .reply
            .send(Err(unreachable_err("daemon connection closed")));
    }
    if let Some(inner) = weak.upgrade() {
        let mut guard = inner.conn.lock().await;
        if guard.as_ref().is_some_and(|c| c.gen == gen) {
            *guard = None;
        }
        drop(guard);
        let _ = inner.conn_gen.send(gen.wrapping_add(1));
    }
}

fn route_message(
    msg: DaemonMessage,
    queue: &mut std::collections::VecDeque<Pending>,
    event_tx: &broadcast::Sender<UiEvent>,
) {
    let pure_push = matches!(
        &msg,
        DaemonMessage::StateChanged { .. }
            | DaemonMessage::TransferProgress { .. }
            | DaemonMessage::ConflictRecorded { .. }
    );
    if pure_push {
        if let Some(event) = push_event_of(&msg) {
            let _ = event_tx.send(event);
        }
        return;
    }

    let snapshot_for_other = matches!(msg, DaemonMessage::Snapshot(_))
        && !queue
            .front()
            .is_some_and(|p| matches!(p.command, ClientCommand::GetStatus));
    if snapshot_for_other {
        if let Some(event) = push_event_of(&msg) {
            let _ = event_tx.send(event);
        }
        return;
    }
    match queue.pop_front() {
        Some(pending) => {
            let _ = pending.reply.send(Ok(msg));
        }
        None => {
            if let Some(event) = push_event_of(&msg) {
                let _ = event_tx.send(event);
            }
        }
    }
}

fn push_event_of(msg: &DaemonMessage) -> Option<UiEvent> {
    match msg {
        DaemonMessage::StateChanged {
            state,
            manifest_id,
            agreed_id,
            pending_changes,
            stats,
        } => Some(UiEvent::StateChanged {
            state: state.clone(),
            manifest_id: manifest_id.clone(),
            agreed_id: agreed_id.clone(),
            pending_changes: *pending_changes,
            stats: *stats,
        }),
        DaemonMessage::TransferProgress {
            bytes_transferred,
            total_bytes,
            current_path,
            chunks_transferred,
            total_chunks,
            peer_device_id,
            direction,
        } => Some(UiEvent::TransferProgress {
            bytes_transferred: *bytes_transferred,
            total_bytes: *total_bytes,
            current_path: current_path.clone(),
            chunks_transferred: *chunks_transferred,
            total_chunks: *total_chunks,
            peer_device_id: peer_device_id.clone(),
            direction: *direction,
        }),
        DaemonMessage::ConflictRecorded {
            path,
            conflict_path,
            timestamp,
            quarantined_as,
        } => Some(UiEvent::ConflictRecorded {
            path: path.clone(),
            conflict_path: conflict_path.clone(),
            timestamp: *timestamp,
            quarantined_as: quarantined_as.clone(),
        }),
        DaemonMessage::Snapshot(snap) => Some(UiEvent::State(snap.clone())),
        DaemonMessage::Error { code, message } => Some(UiEvent::Error {
            code: code.clone(),
            message: message.clone(),
        }),
        _ => None,
    }
}

fn listing_error_hint(code: &str) -> &'static str {
    match code {
        "permission-denied" => "check folder permissions",
        "path-traversal" => "path escapes allowed root",
        "bad-path" => "use absolute path",
        _ => "check daemon",
    }
}

impl StatusDomain for DaemonClient {
    fn get_status(&self) -> BoxFuture<'_, Result<EngineSnapshot, OpError>> {
        Box::pin(async move {
            match self.call(ClientCommand::GetStatus).await? {
                DaemonMessage::Snapshot(snap) => Ok(snap),
                DaemonMessage::Error { code, message } => {
                    Err(OpError::new(code, message, "check daemon"))
                }
                other => Err(unexpected_response(&other)),
            }
        })
    }

    fn list_conflicts(&self) -> BoxFuture<'_, Result<Vec<ConflictEntry>, OpError>> {
        Box::pin(async move {
            match self.call(ClientCommand::ListConflicts).await? {
                DaemonMessage::Ack {
                    message: Some(json_str),
                    ..
                } => Ok(serde_json::from_str(&json_str).unwrap_or_default()),
                DaemonMessage::Ack { message: None, .. } => Ok(Vec::new()),
                DaemonMessage::Error { code, message } => {
                    Err(OpError::new(code, message, "check daemon"))
                }
                other => Err(unexpected_response(&other)),
            }
        })
    }

    fn trigger_scan(&self) -> BoxFuture<'_, Result<(), OpError>> {
        Box::pin(async move {
            match self.call(ClientCommand::TriggerScan).await? {
                DaemonMessage::Ack { .. } => Ok(()),
                DaemonMessage::Error { code, message } => {
                    Err(OpError::new(code, message, "check daemon"))
                }
                other => Err(unexpected_response(&other)),
            }
        })
    }

    fn subscribe_events(&self) -> BoxFuture<'_, Result<UiEventStream, OpError>> {
        Box::pin(async move { self.event_stream().await })
    }

    fn list_discovered_devices(&self) -> BoxFuture<'_, Result<Vec<DiscoveredDeviceView>, OpError>> {
        Box::pin(async move {
            let snap = self.get_status().await?;
            Ok(snap.discovered_devices)
        })
    }
}

impl InventoryDomain for DaemonClient {
    fn list_directory(
        &self,
        path: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<ListDirectoryResponse, OpError>> {
        Box::pin(async move {
            let validated = validate_path(path)?;
            match self
                .call(ClientCommand::ListDirectory {
                    path: Some(validated),
                })
                .await?
            {
                DaemonMessage::DirectoryListing {
                    entries,
                    absolute_path,
                } => Ok(ListDirectoryResponse::new(entries, absolute_path)),
                DaemonMessage::Error { code, message } => {
                    let hint = listing_error_hint(&code);
                    Err(OpError::new(code, message, hint))
                }
                other => Err(unexpected_response(&other)),
            }
        })
    }

    fn list_folders(&self) -> BoxFuture<'_, Result<Vec<FolderRecord>, OpError>> {
        Box::pin(async move {
            match self.call(ClientCommand::ListFolders).await? {
                DaemonMessage::FolderList { folders } => Ok(folders),
                DaemonMessage::Error { code, message } => {
                    Err(OpError::new(code, message, "check daemon"))
                }
                other => Err(unexpected_response(&other)),
            }
        })
    }

    fn register_folder(&self, path: PathBuf) -> BoxFuture<'_, Result<FolderRecord, OpError>> {
        Box::pin(async move {
            match self.call(ClientCommand::RegisterFolder { path }).await? {
                DaemonMessage::FolderRegistered { folder } => Ok(folder),
                DaemonMessage::Error { code, message } => {
                    Err(OpError::new(code, message, "check daemon"))
                }
                other => Err(unexpected_response(&other)),
            }
        })
    }

    fn remove_folder(&self, folder_id: String) -> BoxFuture<'_, Result<(), OpError>> {
        Box::pin(async move {
            match self
                .call(ClientCommand::RemoveFolder {
                    folder_id: folder_id.clone(),
                })
                .await?
            {
                DaemonMessage::FolderRemoved { .. } | DaemonMessage::Ack { .. } => Ok(()),
                DaemonMessage::Error { code, message } => {
                    Err(OpError::new(code, message, "check daemon"))
                }
                other => Err(unexpected_response(&other)),
            }
        })
    }
}

impl SessionDomain for DaemonClient {
    fn start_pin(
        &self,
        paths: Vec<String>,
        hours: Option<u64>,
    ) -> BoxFuture<'_, Result<PinRecord, OpError>> {
        let client = self.clone();
        Box::pin(async move {
            match client
                .call(ClientCommand::StartPin {
                    paths: paths.clone(),
                    duration_hours: hours,
                })
                .await?
            {
                DaemonMessage::Ack { command, message } => Ok(PinRecord {
                    folder: String::new(),
                    paths,
                    status: command,
                    expires_at: None,
                    message,
                }),
                DaemonMessage::Error { code, message } => {
                    Err(OpError::new(code, message, "stop existing pin"))
                }
                other => Err(unexpected_response(&other)),
            }
        })
    }

    fn stop_pin(&self) -> BoxFuture<'_, Result<PinStopSummary, OpError>> {
        let client = self.clone();
        Box::pin(async move {
            match client.call(ClientCommand::ReleasePin).await? {
                DaemonMessage::Ack { command, message } => Ok(PinStopSummary {
                    folder: String::new(),
                    status: command,
                    message,
                }),
                DaemonMessage::Error { code, message } => {
                    Err(OpError::new(code, message, "check daemon"))
                }
                other => Err(unexpected_response(&other)),
            }
        })
    }

    fn release_pin(&self) -> BoxFuture<'_, Result<PinReleaseSummary, OpError>> {
        let client = self.clone();
        Box::pin(async move {
            match client.call(ClientCommand::ReleasePin).await? {
                DaemonMessage::Ack { command, message } => Ok(PinReleaseSummary {
                    folder: String::new(),
                    released_changes: 0,
                    status: command,
                    message,
                }),
                DaemonMessage::Error { code, message } => {
                    Err(OpError::new(code, message, "check daemon"))
                }
                other => Err(unexpected_response(&other)),
            }
        })
    }

    fn share_initiate(
        &self,
        folder: Option<PathBuf>,
        _i_know: bool,
    ) -> BoxFuture<'_, Result<ShareOffer, OpError>> {
        let dir = folder.unwrap_or_else(|| PathBuf::from("."));
        Box::pin(async move {
            Err(OpError::new(
                "not-supported",
                format!(
                    "share_initiate for {} requires in-process or daemon ritual",
                    dir.display()
                ),
                "use AutoBackend with fallback or run ferry share",
            ))
        })
    }

    fn share_status(&self, folder: Option<PathBuf>) -> BoxFuture<'_, Result<ShareStatus, OpError>> {
        let dir = folder.unwrap_or_else(|| PathBuf::from("."));
        Box::pin(async move {
            Err(OpError::new(
                "not-supported",
                format!(
                    "share_status for {} requires in-process or daemon ritual",
                    dir.display()
                ),
                "use AutoBackend with fallback or run ferry share",
            ))
        })
    }

    fn pair_accept(
        &self,
        code_or_payload: String,
        dir: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<PairResult, OpError>> {
        let folder = dir.unwrap_or_else(|| PathBuf::from("."));
        Box::pin(async move {
            Err(OpError::new(
                "not-supported",
                format!(
                    "pair_accept {code_or_payload} for {} requires in-process ritual",
                    folder.display()
                ),
                "use AutoBackend with fallback or run ferry pair",
            ))
        })
    }

    fn create_pairing_session(
        &self,
        req: CreatePairingRequest,
    ) -> BoxFuture<'_, Result<CreatePairingResponse, OpError>> {
        let client = self.clone();
        Box::pin(async move {
            match client
                .call(ClientCommand::CreatePairingSession { req })
                .await?
            {
                DaemonMessage::PairingCreated { response } => Ok(response),
                DaemonMessage::Error { code, message } => {
                    Err(OpError::new(code, message, "check daemon"))
                }
                other => Err(unexpected_response(&other)),
            }
        })
    }

    fn join_pairing_session(
        &self,
        req: JoinPairingRequest,
    ) -> BoxFuture<'_, Result<PairResult, OpError>> {
        let client = self.clone();
        Box::pin(async move {
            match client
                .call(ClientCommand::JoinPairingSession { req })
                .await?
            {
                DaemonMessage::PairingJoined { result } => Ok(result),
                DaemonMessage::Error { code, message } => {
                    Err(OpError::new(code, message, "check daemon"))
                }
                other => Err(unexpected_response(&other)),
            }
        })
    }
}
