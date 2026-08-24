//! `ferry sync`: single-shot exchange rounds until both sides agree.
//!
//! Exit contract (per ticket): 0 when converged, 1 when the timeout hit
//! first ("best-effort"). Every round is one full session; the peer must be
//! listening (`ferry daemon --listen ...` or another `ferry sync` cannot
//! dial AND listen at once in v0).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ferry_sync::transport::Transport;
use serde_json::json;

use crate::error::{CliError, CliResult};
use crate::exchange::{run_round, scan_snapshot, FolderSession};
use crate::folder;
use crate::out::Output;

pub struct SyncArgs<'a> {
    pub folder: Option<&'a Path>,
    pub peer_url: Option<&'a str>,
    pub timeout_secs: u64,
    pub transport: &'a str,
}

pub fn run(args: SyncArgs<'_>) -> CliResult<Output> {
    if args.transport != "tcp" {
        return Err(CliError::new(
            "transport-unavailable",
            format!("transport {:?} is not implemented yet", args.transport),
            "use --transport tcp today; iroh QUIC P2P lands with tickets T-009/T-014",
        ));
    }
    let peer: SocketAddr = args
        .peer_url
        .ok_or_else(|| {
            CliError::new(
                "usage",
                "ferry sync needs --peer-url",
                "point it at a listening device, e.g. --peer-url 127.0.0.1:44001",
            )
        })?
        .parse()
        .map_err(|_| {
            CliError::new(
                "bad-address",
                format!("--peer-url {:?} is not HOST:PORT", args.peer_url.unwrap()),
                "example: 127.0.0.1:44001",
            )
        })?;

    let folder_path: PathBuf = args
        .folder
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let opened = folder::open_folder(&folder_path)?;
    let transport = ferry_sync::TcpTransport;
    let ignore: Arc<dyn ferry_scan::IgnorePolicy> =
        Arc::new(folder::load_rules(&opened.root, &opened.settings)?);

    let session = FolderSession {
        state_dir: opened.state_dir(),
        tree_root: opened.root.clone(),
        store: opened.store.clone(),
        folder_id: opened.folder_id,
        device_id: current_device_id(),
        poly: opened.poly,
        ignore,
    };

    let deadline = Instant::now() + Duration::from_secs(args.timeout_secs);
    let mut rounds = 0u64;
    let mut totals = RoundTotals::default();
    let mut converged = false;
    let mut peer_device: Option<String> = None;

    while Instant::now() < deadline {
        rounds += 1;
        let snap = scan_snapshot(&session)?;
        match transport.dial(peer) {
            Ok(mut conn) => match run_round(&mut conn, true, &session, &snap, None) {
                Ok(report) => {
                    if let Some(p) = &report.peer_device_id {
                        remember_addr(&session, p, peer);
                        peer_device = Some(p.clone());
                    }
                    totals.add(&report);
                    if report.agreed {
                        converged = true;
                        break;
                    }
                }
                Err(e) => eprintln!("round {rounds} failed: {e}"),
            },
            Err(e) => eprintln!(
                "peer not reachable ({e}); retrying until {}s elapse",
                args.timeout_secs
            ),
        }
        std::thread::sleep(Duration::from_millis(300));
    }

    // A final scan so status output after `sync` reflects applied changes
    // even when we timed out mid-flight.
    let _ = scan_snapshot(&session);

    let json_doc = json!({
        "command": "sync",
        "folder": opened.root.display().to_string(),
        "folder_id": ferry_store::format::hex(&opened.folder_id),
        "device_id": ferry_store::format::hex(&session.device_id),
        "peer_device_id": peer_device,
        "converged": converged,
        "rounds": rounds,
        "chunks_sent": totals.sent,
        "chunks_received": totals.received,
        "ops_applied": totals.applied,
        "quarantined": totals.quarantined,
        "conflicts_recorded": totals.conflicts,
        "held": totals.held,
    });

    let human = if converged {
        format!(
            "Converged after {rounds} round(s): sent {} chunk(s), received {}, applied {} change(s), {} conflict(s).\n",
            totals.sent, totals.received, totals.applied, totals.conflicts
        )
    } else {
        format!(
            "NOT converged within {}s after {rounds} round(s) (best effort: sent {}, received {}, applied {}).",
            args.timeout_secs, totals.sent, totals.received, totals.applied
        )
    };

    let mut out = Output::new(json_doc, human);
    if !converged {
        out.exit_code = 1;
    }
    Ok(out)
}

#[derive(Default)]
struct RoundTotals {
    sent: u64,
    received: u64,
    applied: u64,
    quarantined: u64,
    conflicts: u64,
    held: u64,
}

impl RoundTotals {
    fn add(&mut self, r: &crate::exchange::RoundReport) {
        self.sent += r.chunks_sent as u64;
        self.received += r.chunks_received as u64;
        self.applied += r.ops_applied as u64;
        self.quarantined += r.quarantined as u64;
        self.conflicts += r.conflicts_recorded as u64;
        self.held += r.held as u64;
    }
}

fn current_device_id() -> [u8; 32] {
    let Ok(home) = crate::home::ferry_home() else {
        return [0u8; 32];
    };
    match ferry_crypto::identity::load_or_create(&crate::home::identity_root(&home)) {
        Ok(id) => *id.public(),
        Err(_) => [0u8; 32],
    }
}

fn remember_addr(session: &FolderSession, peer_hex: &str, addr: SocketAddr) {
    if peer_hex.len() != 64 {
        return;
    }
    let dir = session.state_dir.join("peers");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(format!("{peer_hex}.addr")), addr.to_string());
}
