//! `ferry daemon`: watch folders, snapshot continuously, exchange with one
//! peer over TCP in the background.
//!
//! # What the daemon honestly does today (v0)
//!
//! - Watches each folder with ferry-scan's `ScanEngine` (native events +
//!   poll fallback + audits) driving snapshots into the encrypted store.
//! - Exchanges with EXACTLY ONE peer address (`--peer-url`), over plain
//!   localhost/LAN TCP using the M0 message inventory. No discovery, no
//!   NAT traversal, no relay: those land with T-009/T-014 (iroh QUIC).
//!   `--transport` accepts only `tcp`; other values fail cleanly.
//! - Applies three-way reconciliation (ferry-sync-engine) with conflict
//!   quarantine per ADR-0004; rounds repeat until roots match, then the
//!   agreement pointer is recorded.
//!
//! Roles: run one side with `--listen` (it serves sessions) and the other
//! with `--peer-url` (it dials every interval and drives rounds). A single
//! daemon may pass both flags.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// Lock a mutex, tolerating poisoning (T-04): a panicked session thread
/// must not turn into a daemon-wide crash on the next lock.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

use ferry_scan::{ScanEngine, StoreHandle};
use ferry_store::format::hex;
use ferry_sync::transport::Transport;

use crate::error::{CliError, CliResult};
use crate::exchange::{run_round, scan_snapshot, FolderSession};
use crate::folder::{self, OpenFolder};
use crate::out::Output;

/// One watched folder inside a running daemon.
struct WatchedFolder {
    opened: OpenFolder,
    session: FolderSession,
    engine: ScanEngine,
    /// Serializes exchange rounds per folder.
    lock: Arc<Mutex<()>>,
}

pub struct DaemonArgs<'a> {
    pub folders: &'a [PathBuf],
    pub listen: Option<&'a str>,
    pub peer_url: Option<&'a str>,
    pub transport: &'a str,
    pub interval_secs: u64,
    pub json: bool,
}

pub fn run(args: DaemonArgs<'_>) -> CliResult<Output> {
    check_transport(args.transport)?;

    let listen_addr: Option<SocketAddr> = match args.listen {
        Some(s) => Some(s.parse().map_err(|_| {
            CliError::new(
                "bad-address",
                format!("--listen {s:?} is not HOST:PORT"),
                "example: 127.0.0.1:44001",
            )
        })?),
        None => None,
    };
    let peer_addr: Option<SocketAddr> = match args.peer_url {
        Some(s) => Some(s.parse().map_err(|_| {
            CliError::new(
                "bad-address",
                format!("--peer-url {s:?} is not HOST:PORT"),
                "example: 127.0.0.1:44001",
            )
        })?),
        None => None,
    };
    if listen_addr.is_none() && peer_addr.is_none() {
        return Err(CliError::new(
            "usage",
            "the daemon needs --listen and/or --peer-url",
            "run one side with --listen 127.0.0.1:44001 and point the other side's --peer-url at it",
        ));
    }

    let paths: Vec<PathBuf> = if args.folders.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        args.folders.to_vec()
    };

    let mut watched = Vec::with_capacity(paths.len());
    for p in &paths {
        let opened = folder::open_folder(p)?;
        let rules = Arc::new(folder::load_rules(&opened.root, &opened.settings)?);
        let device_id = current_device_id();
        let handle = StoreHandle {
            store: opened.store.clone(),
            poly: opened.poly,
            folder_id: opened.folder_id,
            device_id,
        };
        let engine = ScanEngine::watch_with(
            opened.root.clone(),
            handle,
            ferry_scan::ScanConfig::default(),
            Arc::clone(&rules) as Arc<dyn ferry_scan::IgnorePolicy>,
        )
        .map_err(|e| {
            CliError::new(
                "watch",
                e.to_string(),
                "check the folder exists and is readable",
            )
        })?;
        let session = FolderSession {
            state_dir: opened.state_dir(),
            tree_root: opened.root.clone(),
            store: opened.store.clone(),
            folder_id: opened.folder_id,
            device_id,
            poly: opened.poly,
            ignore: rules,
        };
        eprintln!(
            "watching {} (folder {})",
            opened.root.display(),
            ferry_store::format::hex(&opened.folder_id)
        );
        watched.push(Arc::new(WatchedFolder {
            opened,
            session,
            engine,
            lock: Arc::new(Mutex::new(())),
        }));
    }

    // Listener thread: serve incoming sessions.
    let transport = Arc::new(ferry_sync::TcpTransport);
    let mut listener = match listen_addr {
        Some(addr) => Some(Transport::listen(transport.as_ref(), addr).map_err(|e| {
            CliError::new(
                "bind",
                format!("cannot bind {addr}: {e}"),
                "pick another port or free the existing listener",
            )
        })?),
        None => None,
    };
    if let Some(lst) = listener.take() {
        let addr = lst
            .local_addr()
            .map_err(|e| CliError::new("bind", e.to_string(), "retry"))?;
        // Machine-greppable line scripts rely on (human mode).
        println!("LISTENING {addr}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        spawn_accept_loop(lst, &watched);
    }

    // Dialer loop: drive rounds against the peer every interval.
    let dialer_handles: Vec<_> = watched.iter().map(|w| (Arc::clone(w), peer_addr)).collect();
    if let Some(_peer) = peer_addr {
        spawn_dial_loop(
            transport.clone(),
            dialer_handles,
            args.interval_secs,
            args.json,
        )?;
    }

    // Park forever; termination is a process signal (std-only v0).
    loop {
        std::thread::sleep(Duration::from_hours(1));
    }
}

fn check_transport(kind: &str) -> CliResult<()> {
    match kind {
        "tcp" => Ok(()),
        other => Err(CliError::new(
            "transport-unavailable",
            format!("transport {other:?} is not implemented yet"),
            "use --transport tcp today; iroh QUIC P2P lands with tickets T-009/T-014",
        )),
    }
}

fn current_device_id() -> [u8; 32] {
    let Ok(home) = crate::home::ferry_home() else {
        return [0u8; 32]; // open_folder would have failed already
    };
    match ferry_crypto::identity::load_or_create(&crate::home::identity_root(&home)) {
        Ok(id) => *id.public(),
        Err(_) => [0u8; 32],
    }
}

fn spawn_accept_loop(
    lst: Box<dyn ferry_sync::transport::Listener>,
    watched: &[Arc<WatchedFolder>],
) {
    // Incoming HELLO tags name the DIALER's device+folder
    // (`ferry-<dev8>-<folder8>`); route on the folder half, since both
    // devices share the folder id but never the device id.
    let by_folder: std::collections::HashMap<String, Arc<WatchedFolder>> = watched
        .iter()
        .map(|w| (hex(&w.session.folder_id)[..8].to_string(), Arc::clone(w)))
        .collect();
    std::thread::Builder::new()
        .name("ferry-accept".into())
        .spawn(move || loop {
            match lst.accept() {
                Ok(mut conn) => {
                    // Read the HELLO here to route to the right folder.
                    match ferry_sync::proto::recv_hello(conn.as_mut()) {
                        Ok(h) => {
                            let folder_key = h.device_tag.split('-').nth(2).unwrap_or("");
                            let Some(w) = by_folder.get(folder_key).cloned() else {
                                ferry_sync::proto::send_error(
                                    conn.as_mut(),
                                    &format!("no local folder matches tag {}", h.device_tag),
                                );
                                continue;
                            };
                            let _guard = lock(&w.lock);
                            serve_session(&w, conn.as_mut(), h.device_tag);
                        }
                        Err(e) => {
                            eprintln!("accept: bad hello: {e}");
                        }
                    }
                }
                Err(e) => {
                    eprintln!("accept error: {e}");
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        })
        .expect("spawn accept loop");
}

/// Serve one inbound round. The HELLO was already consumed for routing, so
/// the session continues from the OFFER phase.
fn serve_session(
    w: &WatchedFolder,
    conn: &mut dyn ferry_sync::transport::Connection,
    peer_tag: String,
) {
    // Drain queued watcher events so our offer reflects right-now state.
    let _ = w.engine.scan_once();
    let snap = match scan_snapshot(&w.session) {
        Ok(s) => s,
        Err(e) => {
            ferry_sync::proto::send_error(conn, &format!("local scan failed: {e}"));
            return;
        }
    };
    match run_round(conn, false, &w.session, &snap, Some(peer_tag)) {
        Ok(report) => log_round(w, &report),
        Err(e) => {
            ferry_sync::proto::send_error(conn, &format!("{e}"));
            eprintln!("[{}] session failed: {e}", display_root(w));
        }
    }
}

fn spawn_dial_loop(
    transport: Arc<ferry_sync::TcpTransport>,
    targets: Vec<(Arc<WatchedFolder>, Option<SocketAddr>)>,
    interval_secs: u64,
    json_mode: bool,
) -> CliResult<()> {
    let interval = Duration::from_secs(interval_secs.max(1));
    std::thread::Builder::new()
        .name("ferry-dial".into())
        .spawn(move || {
            let mut n: u64 = 0;
            loop {
                for (w, addr) in &targets {
                    let addr = addr.expect("checked at startup");
                    n += 1;
                    let _ = w.engine.scan_once();
                    let snap = match scan_snapshot(&w.session) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("[{}] scan failed: {e}", display_root(w));
                            continue;
                        }
                    };
                    match transport.dial(addr) {
                        Ok(mut conn) => match run_round(&mut conn, true, &w.session, &snap, None) {
                            Ok(report) => {
                                if let Some(peer) = &report.peer_device_id {
                                    remember_peer_addr(&w.session, peer, addr);
                                }
                                log_round(w, &report);
                                if json_mode {
                                    emit_event_json(w, &report);
                                }
                            }
                            Err(e) => {
                                eprintln!("[{}] round failed: {e}", display_root(w));
                            }
                        },
                        Err(e) => {
                            if n <= targets.len() as u64 {
                                eprintln!("[{}] peer not reachable yet ({e})", display_root(w));
                            }
                        }
                    }
                }
                std::thread::sleep(interval);
            }
        })
        .map_err(|e| CliError::new("thread", e.to_string(), "retry"))?;
    Ok(())
}

fn remember_peer_addr(session: &FolderSession, peer_hex: &str, addr: SocketAddr) {
    if peer_hex.len() != 64 {
        return;
    }
    let dir = session.state_dir.join("peers");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(format!("{peer_hex}.addr")), addr.to_string());
}

fn log_round(w: &WatchedFolder, r: &crate::exchange::RoundReport) {
    eprintln!(
        "[{}] round: {} vs {} equal={} meta+{} sent={} recv={} applied={} conflicts={} held={} agreed={}",
        display_root(w),
        r.my_root,
        r.their_root,
        r.roots_equal_at_offer,
        r.meta_fetched,
        r.chunks_sent,
        r.chunks_received,
        r.ops_applied,
        r.conflicts_recorded,
        r.held,
        r.agreed
    );
}

fn emit_event_json(w: &WatchedFolder, r: &crate::exchange::RoundReport) {
    use std::io::Write;
    let event = serde_json::json!({
        "event": "round",
        "folder": w.opened.root.display().to_string(),
        "folder_id": ferry_store::format::hex(&w.session.folder_id),
        "peer_device_id": r.peer_device_id,
        "roots_equal": r.roots_equal_at_offer,
        "meta_fetched": r.meta_fetched,
        "chunks_sent": r.chunks_sent,
        "chunks_received": r.chunks_received,
        "ops_applied": r.ops_applied,
        "quarantined": r.quarantined,
        "conflicts_recorded": r.conflicts_recorded,
        "held": r.held,
        "agreed": r.agreed,
    });
    let _ = writeln!(std::io::stdout(), "{event}");
}

fn display_root(w: &WatchedFolder) -> String {
    w.opened.root.display().to_string()
}
