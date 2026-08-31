





use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ferry_store::agreement::AgreementLedger;
use ferry_store::format::hex;
use ferry_sync::{EngineConfig, SyncEngine};
use serde_json::json;

use crate::error::{CliError, CliResult};
use crate::folder::{self, OpenFolder};
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
    let device_id = current_device_id();
    let poly = ferry_store::chunker::ValidatedPoly::try_from(opened.poly).map_err(|e| {
        CliError::new(
            "poly-invalid",
            e.to_string(),
            format!(
                "the polynomial record for {} is corrupt; restore the store from a known-good backup",
                opened.root.display()
            ),
        )
    })?;

    let tag = format!("ferry-{}", &hex(&device_id)[..8]);
    let cfg = EngineConfig {
        tag,
        store_dir: opened.root.clone(),
        tree_dir: opened.root.clone(),
        poly,
        folder_id: opened.folder_id,
        poll_interval: Duration::from_millis(50),
        opportunistic_every: 1,
        bind_addr: None,
        connect_to: Some(peer),
        allow_trust_on_first_use: false,
        pin_state_dir: Some(opened.state_dir()),
        quiet: true,
    };

    let transport = Arc::new(ferry_sync::TcpTransport);
    let mut engine =
        SyncEngine::with_store(cfg, transport, Arc::clone(&opened.store)).map_err(|e| {
            CliError::new(
                "engine-init",
                e.to_string(),
                "check folder permissions and network configuration",
            )
        })?;
    
    
    
    
    if let Ok(identity) = crate::home::load_device_identity() {
        engine.set_identity(identity);
    }
    engine.set_ignore_policy(opened.ignore_policy());

    let handle = engine.start();
    let deadline = Instant::now() + Duration::from_secs(args.timeout_secs);
    let mut converged = false;

    while Instant::now() < deadline {
        let stats = handle.stats();
        if handle.agreed_id().is_some() && stats.sessions_ok > 0 {
            converged = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let stats = handle.stats();
    handle.shutdown();

    let mut peer_device = None;
    if let Ok(ledger_entries) =
        AgreementLedger::new(opened.state_dir()).list_folder(&opened.folder_id)
    {
        if let Some((dev, _)) = ledger_entries.first() {
            let p_hex = hex(dev);
            remember_addr(&opened, &p_hex, peer);
            peer_device = Some(p_hex);
        }
    }

    let total_rounds = stats.sessions_ok + stats.sessions_failed;
    let json_doc = json!({
        "command": "sync",
        "folder": opened.root.display().to_string(),
        "folder_id": hex(&opened.folder_id),
        "device_id": hex(&device_id),
        "peer_device_id": peer_device,
        "converged": converged,
        "rounds": total_rounds,
        "chunks_sent": 0,
        "chunks_received": 0,
        "ops_applied": 0,
        "quarantined": 0,
        "conflicts_recorded": 0,
        "held": 0,
    });

    let human = if converged {
        format!("Converged after {total_rounds} round(s).\n")
    } else {
        format!(
            "NOT converged within {}s after {total_rounds} round(s) (best effort).\n",
            args.timeout_secs
        )
    };

    let mut out = Output::new(json_doc, human);
    if !converged {
        out.exit_code = 1;
    }
    Ok(out)
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

fn remember_addr(opened: &OpenFolder, peer_hex: &str, addr: SocketAddr) {
    if peer_hex.len() != 64 {
        return;
    }
    let dir = opened.state_dir().join("peers");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(format!("{peer_hex}.addr")), addr.to_string());
}
