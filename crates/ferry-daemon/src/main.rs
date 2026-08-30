//! `ferry-sync` CLI: run one M0 daemon over iroh (default) or loopback TCP,
//! or generate a folder polynomial.
//!
//! Daemon pairs (iroh mode, the ADR-0003 path):
//!
//! ```text
//! # Node A (listener): announces its public endpoint id
//! ferry-sync daemon --role listen \
//!     --store /tmp/node-a/store --tree /tmp/node-a/tree --tag node-a --poly <hex16> \
//!     [--relay http://relay.example:3340] [--discovery mdns]
//! # -> prints `ENDPOINT <hex64>` once the QUIC endpoint is up
//!
//! # Node B (connector): dials A BY PUBLIC KEY
//! ferry-sync daemon --role connect --peer <A's hex64> \
//!     --store /tmp/node-b/store --tree /tmp/node-b/tree --tag node-b --poly <hex16> \
//!     [--relay http://relay.example:3340] ...
//! ```
//!
//! TCP mode (`--transport tcp`) keeps the M0 localhost shape: `--addr`
//! HOST:PORT means an actual socket again.
//!
//! Both print machine-greppable `STATE root=<hex> agreed=<hex|none>` lines;
//! the connector drives sessions, the listener serves them and relies on
//! the peer's opportunistic dials to discover its changes.
//!
//! Status lines go to stdout; errors to stderr.

use std::process::ExitCode;
use std::str::FromStr as _;
use std::sync::Arc;
use std::time::Duration;

use rand::rngs::StdRng;
use rand::SeedableRng;

use ferry_crypto::identity as crypto_identity;
#[cfg(feature = "web-ui")]
use ferry_daemon::ui;
use ferry_store::format::{hex, unhex};
use ferry_sync::{EngineConfig, SyncEngine};

/// Port 0 makes the transport mint a unique alias.
const IROH_BIND_ALIAS: &str = "127.0.0.1:0";

const USAGE: &str = "\
ferry-sync — M0 walking skeleton

USAGE:
    ferry-sync genpoly [--seed N]
        Print a random irreducible chunker polynomial as 16 hex digits.

    ferry-sync daemon [--transport iroh|tcp] (default iroh)
                      --role listen|connect --store DIR --tree DIR
                      --tag NAME --poly HEX16

IROH TRANSPORT (default):
    ferry-sync daemon --role listen  [--relay URL]... [--discovery mdns]
                      [--force-relay] [--folder-id HEX32] [--poll-ms MS]
    ferry-sync daemon --role connect --peer HEX64 [--relay URL]... [...]
    Peers are addressed by their public endpoint id (derived from the
    device identity stored under <store>/.ferry/identity). Listeners print
    `ENDPOINT <hex64>`; connectors pass it back via --peer. Relays are
    operator-run (`ferry-relay` binary); without one, use --discovery mdns
    on shared LANs.

TCP TRANSPORT (--transport tcp; M0 throwaway, tests only):
    ferry-sync daemon --role listen  --addr HOST:PORT
    ferry-sync daemon --role connect --addr HOST:PORT

IPC SERVER:
    The daemon always runs an IPC server on the folder socket
    (<store>/.ferry/daemon.sock or --socket PATH) broadcasting live
    snapshots, engine state changes, transfers, and conflicts.

WEB DASHBOARD (optional):
    ferry-sync daemon ... [--ui [HOST:PORT]]   (default 127.0.0.1:8098)
    Loopback binds only, no auth (v0 stance); serves the embedded UI plus
    /api/status, /api/conflicts, /api/share, /api/pair/accept and
    /api/pin/start|stop|release per .scratch/web-dashboard/spec.md.

Roles: exactly one side runs `listen`; the other runs `connect` and drives
all sessions.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("genpoly") => cmd_genpoly(&args[1..]),
        Some("daemon") => cmd_daemon(&args[1..]),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn flags(args: &[String], name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == name {
            if let Some(v) = args.get(i + 1) {
                out.push(v.clone());
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    out
}

fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn require(args: &[String], name: &str) -> Result<String, String> {
    flag(args, name).ok_or(format!("missing required flag {name}"))
}

fn cmd_genpoly(args: &[String]) -> ExitCode {
    let seed: u64 = flag(args, "--seed")
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| {
            use rand::RngCore;
            rand::rngs::OsRng.next_u64()
        });
    let poly = ferry_store::chunker::generate_polynomial(&mut StdRng::seed_from_u64(seed));
    println!("{poly:016x}");
    ExitCode::SUCCESS
}

fn ferry_home() -> std::path::PathBuf {
    if let Some(v) = std::env::var_os("FERRY_HOME") {
        let p = std::path::PathBuf::from(&v);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    if let Some(home) = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
    {
        if !home.as_os_str().is_empty() {
            return home.join(".ferry");
        }
    }
    std::path::PathBuf::from("/tmp/.ferry")
}

fn cmd_daemon(args: &[String]) -> ExitCode {
    // Central supervisor mode: `ferry daemon` without legacy flags runs Supervisor.
    // Preserve `--listen` single-folder deprecated wrapper by routing to legacy when --store/--role present.
    let is_legacy = has_flag(args, "--store")
        || has_flag(args, "--tree")
        || has_flag(args, "--role")
        || has_flag(args, "--transport");
    if !is_legacy {
        match run_central_daemon(args) {
            Ok(()) => return ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    // Legacy single-folder path (also used for `ferry daemon --listen` deprecated wrapper)
    if has_flag(args, "--listen") {
        eprintln!(
            "warning: --listen is deprecated; use `ferry daemon` without args for device daemon"
        );
        // If --listen is given with a path arg, register it before falling through to legacy
        if let Some(p) = flag(args, "--listen") {
            let home = ferry_home();
            let _ = std::fs::create_dir_all(&home);
            if let Ok(rec) = ferry_folder::inventory::FolderInventory::new(&home)
                .register(&std::path::PathBuf::from(&p))
            {
                eprintln!(
                    "registered folder {} -> {}",
                    rec.path.display(),
                    rec.folder_id
                );
            }
        }
    }
    match parse_and_run_daemon(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_central_daemon(args: &[String]) -> Result<(), String> {
    let home = ferry_home();
    std::fs::create_dir_all(&home).map_err(|e| format!("home {}: {e}", home.display()))?;

    let _lock = ferry_platform::DaemonLock::acquire(&home).map_err(|e| match e {
        ferry_platform::DaemonLockError::AlreadyRunning(pid) => {
            let pid_str = pid.map(|p| format!(" (PID {p})")).unwrap_or_default();
            format!("A Ferry daemon is already running{pid_str}. Run `ferry daemon stop` first.")
        }
        ferry_platform::DaemonLockError::Io(err) => {
            format!("Failed to acquire daemon lock: {err}")
        }
    })?;

    // Device identity persisted under $FERRY_HOME/identity (or legacy $FERRY_HOME)
    let identity = ferry_crypto::identity::load_or_create(&home.join("identity"))
        .or_else(|_| {
            ferry_crypto::identity::load_or_create(&home.join("identity").join("device.key"))
        })
        .map_err(|e| format!("device identity: {e}"))?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    let mut supervisor = ferry_daemon::supervisor::Supervisor::new(home.clone(), identity.clone());
    // Register any folder args before spawning (e.g., `ferry daemon /tmp/a /tmp/b`)
    for arg in args {
        if arg.starts_with('-') {
            continue;
        }
        if arg == "daemon" {
            continue;
        }
        let p = std::path::PathBuf::from(arg);
        if p.as_os_str().is_empty() {
            continue;
        }
        let abs = if p.is_relative() {
            std::env::current_dir()
                .map(|cwd| cwd.join(&p))
                .unwrap_or(p.clone())
        } else {
            p.clone()
        };
        // handle_register may need tokio runtime for engine spawn; run inside runtime
        let rec = rt.block_on(async { supervisor.handle_register(abs.clone()) });
        match rec {
            Ok(r) => eprintln!("registered {} -> {}", r.path.display(), r.folder_id),
            Err(e) if e.code == "already-synced" => {
                eprintln!("already-synced {}: {}", p.display(), e.message);
            }
            Err(e) => return Err(format!("register {}: {}", p.display(), e.message)),
        }
    }
    rt.block_on(async {
        supervisor
            .spawn_engines()
            .map_err(|e| format!("spawn engines: {}: {}", e.code, e.message))
    })?;
    let socket_path = ferry_ipc::paths::default_socket_path();
    if let Some(parent) = socket_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let supervisor_arc = std::sync::Arc::new(tokio::sync::Mutex::new(supervisor));
    let sup_for_ipc = std::sync::Arc::clone(&supervisor_arc);
    let ipc_handle = rt
        .block_on(async {
            ferry_daemon::ipc::spawn_supervisor_ipc_server(socket_path.clone(), sup_for_ipc)
        })
        .map_err(|e| format!("ipc server: {e}"))?;
    eprintln!("ferry device daemon listening at {}", socket_path.display());

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    let s_tx = shutdown_tx.clone();
    rt.spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = s_tx.send(true);
    });
    #[cfg(unix)]
    {
        let s_tx2 = shutdown_tx.clone();
        rt.spawn(async move {
            if let Ok(mut sig) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                sig.recv().await;
                let _ = s_tx2.send(true);
            }
        });
    }

    // Supervision loop with backoff — runs until shutdown signal
    rt.block_on(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let mut sup = supervisor_arc.lock().await;
                    sup.tick();
                }
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        eprintln!("Shutting down ferry daemon cleanly...");
                        break;
                    }
                }
            }
        }
    });
    ipc_handle.shutdown();
    Ok(())
}

enum TransportKind {
    Iroh,
    Tcp,
}

struct DaemonArgs {
    kind: TransportKind,
    role: String,
    addr: Option<std::net::SocketAddr>,
    peer: Option<[u8; 32]>,
    relays: Vec<String>,
    mdns: bool,
    force_relay: bool,
    store_dir: std::path::PathBuf,
    tree_dir: std::path::PathBuf,
    tag: String,
    poly: u64,
    folder_id: [u8; 16],
    poll_ms: u64,
    opportunistic_every: u32,
    ui_addr: Option<std::net::SocketAddr>,
    socket_path: Option<std::path::PathBuf>,
}

/// `--ui [ADDR]`: bare flag means the documented default; any non-loopback
/// address is refused at startup (v0 auth stance: localhost only).
fn parse_ui_addr(args: &[String]) -> Result<Option<std::net::SocketAddr>, String> {
    if !has_flag(args, "--ui") {
        return Ok(None);
    }
    let raw = flag(args, "--ui").unwrap_or_else(|| "127.0.0.1:8098".to_string());
    let addr = std::net::SocketAddr::from_str(&raw).map_err(|e| format!("--ui {raw:?}: {e}"))?;
    if !addr.ip().is_loopback() {
        return Err(format!(
            "--ui {addr}: refusing non-loopback bind; the dashboard serves localhost only"
        ));
    }
    Ok(Some(addr))
}

fn parse_and_run_daemon(args: &[String]) -> Result<(), String> {
    if has_flag(args, "--help") || has_flag(args, "-h") {
        println!("{USAGE}");
        return Ok(());
    }
    let kind = match flag(args, "--transport").as_deref() {
        None | Some("iroh") => TransportKind::Iroh,
        Some("tcp") => TransportKind::Tcp,
        Some(other) => return Err(format!("--transport must be iroh|tcp, got {other:?}")),
    };

    let store_dir = std::path::PathBuf::from(require(args, "--store")?);
    let parsed = DaemonArgs {
        kind,
        role: require(args, "--role")?,
        // TCP mode demands --addr; iroh mode ignores it (fixed labels below).
        addr: match flag(args, "--addr") {
            Some(a) => {
                Some(std::net::SocketAddr::from_str(&a).map_err(|e| format!("--addr: {e}"))?)
            }
            None => None,
        },
        peer: match flag(args, "--peer") {
            Some(h) => Some(
                unhex::<32>(&h.replace(' ', ""))
                    .ok_or("--peer expects 64 hex digits (a 32-byte endpoint id)")?,
            ),
            None => None,
        },
        relays: flags(args, "--relay"),
        mdns: has_flag(args, "--discovery-mdns"),
        force_relay: has_flag(args, "--force-relay"),
        store_dir: store_dir.clone(),
        tree_dir: std::path::PathBuf::from(require(args, "--tree")?),
        tag: require(args, "--tag")?,
        poly: u64::from_str_radix(require(args, "--poly")?.trim_start_matches("0x"), 16)
            .map_err(|e| format!("--poly expects 16 hex digits: {e}"))?,
        folder_id: match flag(args, "--folder-id") {
            Some(h) => unhex::<16>(&h).ok_or("--folder-id expects 32 hex digits")?,
            None => ferry_sync::DEFAULT_FOLDER_ID,
        },
        poll_ms: flag(args, "--poll-ms")
            .and_then(|s| s.parse().ok())
            .unwrap_or(200),
        opportunistic_every: flag(args, "--opportunistic-every")
            .and_then(|s| s.parse().ok())
            .unwrap_or(ferry_sync::engine::DEFAULT_OPPORTUNISTIC_EVERY),
        ui_addr: parse_ui_addr(args)?,
        socket_path: flag(args, "--socket").map(std::path::PathBuf::from),
    };
    run_daemon(parsed)
}

fn validate_tag(tag: &str) -> Result<(), String> {
    if tag.is_empty() || tag.len() > 64 || tag.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err("tag must be 1..64 non-whitespace chars".to_string());
    }
    Ok(())
}

fn run_daemon(d: DaemonArgs) -> Result<(), String> {
    validate_tag(&d.tag).map_err(|e| format!("--tag: {e}"))?;
    if d.role != "listen" && d.role != "connect" {
        return Err(format!("--role must be listen|connect, got {:?}", d.role));
    }

    let mut cfg = EngineConfig {
        tag: d.tag.clone(),
        store_dir: d.store_dir.clone(),
        tree_dir: d.tree_dir.clone(),
        // T-02: validate the user-supplied --poly HERE, at config load. A
        // typo used to surface as a chunker .expect() panic mid-scan.
        poly: ferry_store::chunker::ValidatedPoly::new(d.poly)
            .map_err(|e| format!("--poly: {e}"))?,
        folder_id: d.folder_id,
        poll_interval: Duration::from_millis(d.poll_ms),
        opportunistic_every: d.opportunistic_every,
        bind_addr: None,
        connect_to: None,
        // Strict peer pinning arrives with T-007's pairing ritual; the
        // skeleton accepts whichever identity proves key possession.
        expected_peer_id: None,
        // T-06: production daemons enforce session pins at the engine's
        // execution boundary. The --store dir IS the folder root whose
        // `.ferry/` holds pin-state.json and the held ledgers.
        pin_state_dir: Some(d.store_dir.join(".ferry")),
        quiet: false,
    };

    // ONE device-identity source for every transport (ticket 12): a real
    // keypair persisted under `<store>/.device-identity`, loaded or created
    // exactly once. Tag-derived ids are test-only and unreachable here.
    // NOTE: the file lives in a SIBLING of the store's `.ferry/` — that
    // directory belongs to the store layout, and creating it early would
    // flip Store::create into Store::open on first run.
    let device = crypto_identity::load_or_create(&d.store_dir.join(".device-identity"))
        .map_err(|e| format!("device identity: {e}"))?;

    let transport: Arc<dyn ferry_sync::Transport> = match d.kind {
        TransportKind::Tcp => {
            let addr = d.addr.ok_or("--addr is required in --transport tcp mode")?;
            match d.role.as_str() {
                "listen" => cfg.bind_addr = Some(addr),
                "connect" => cfg.connect_to = Some(addr),
                _ => unreachable!("validated above"),
            }
            Arc::new(ferry_sync::TcpTransport)
        }
        TransportKind::Iroh => {
            // Stable endpoint identity derived from THIS store's device
            // identity (ferry-crypto): restart-safe public addressing.
            let mut builder = ferry_iroh::IrohConfig::builder().device_identity(&device);
            if !d.relays.is_empty() {
                builder = builder.relays(ferry_iroh::RelaySetting::Custom(d.relays.clone()));
            }
            if d.mdns {
                builder = builder.mdns(ferry_iroh::MdnsSetting {
                    service_name: "ferry-sync".into(),
                    advertise: true,
                });
            }
            if d.force_relay {
                builder = builder.force_relay(true);
            }
            let t = ferry_iroh::IrohTransport::new(builder.build())
                .map_err(|e| format!("iroh transport: {e}"))?;

            let endpoint_hex = hex(&t.endpoint_id());
            match d.role.as_str() {
                "listen" => {
                    cfg.bind_addr =
                        Some(std::net::SocketAddr::from_str(IROH_BIND_ALIAS).expect("constant"));
                }
                "connect" => {
                    let peer = d.peer.ok_or(
                        "--peer HEX64 is required to connect (it is \
                        the listener's ENDPOINT id)",
                    )?;
                    let alias = t.register_peer(peer);
                    cfg.connect_to = Some(alias);
                }
                _ => unreachable!("validated above"),
            }
            // Announce AFTER wiring config so the listener's alias exists
            // before any STATE line; ordering with LISTENING is preserved.
            println!("ENDPOINT {endpoint_hex}");
            Arc::new(t)
        }
    };

    // Store opening goes through ferry-folder, the one module that owns key
    // unwrap and cipher choice. First run on an uninitialized directory
    // initializes a real folder (fresh FMK wrapped to this device's
    // identity); an existing folder must unwrap its key or we fail loud —
    // there is no plaintext or zero-key reopen.
    let store: Arc<ferry_store::store::Store> =
        if ferry_folder::folder::dot_dir(&d.store_dir).is_dir() {
            ferry_folder::folder::open_folder(&d.store_dir, &device)
                .map_err(|e| format!("startup failed: {e}"))?
                .store
        } else {
            let (store, _fmk) =
                ferry_folder::folder::create_folder(&d.store_dir, &device, d.folder_id, d.poly)
                    .map_err(|e| format!("startup failed: {e}"))?;
            // Same ritual as `ferry init`: flush so the polynomial record
            // leaves staging and a restart can reopen through `open_folder`.
            store
                .flush()
                .map_err(|e| format!("startup failed: flush: {e}"))?;
            store
                .write_index_snapshot()
                .map_err(|e| format!("startup failed: index snapshot: {e}"))?;
            Arc::new(store)
        };

    let mut engine = SyncEngine::with_store(cfg, transport, store)
        .map_err(|e| format!("startup failed: {e}"))?;
    engine.set_identity(device.clone());
    if let Some(a) = engine.listen_addr() {
        println!("LISTENING {a}");
    }
    // Run until killed; EngineHandle shutdown happens on drop.
    let handle = engine.start();

    let (broadcast_tx, _) = tokio::sync::broadcast::channel(256);
    let daemon_state = Arc::new(ferry_daemon::state::DaemonState::new(
        handle.clone(),
        d.store_dir.clone(),
        d.tree_dir.clone(),
        d.folder_id,
        device.clone(),
        broadcast_tx,
    ));

    #[allow(deprecated)]
    let socket_path = d
        .socket_path
        .unwrap_or_else(|| ferry_ipc::paths::socket_path_for_dir(&d.store_dir));
    let ipc_handle =
        ferry_daemon::ipc::spawn_ipc_server(socket_path.clone(), Arc::clone(&daemon_state))
            .map_err(|e| format!("ipc server: {e}"))?;
    #[cfg(feature = "web-ui")]
    if let Some(addr) = d.ui_addr {
        let state = ui::UiState::new(handle.clone(), d.store_dir, d.tree_dir, d.folder_id, device);
        ui::spawn(addr, Arc::new(state)).map_err(|e| format!("--ui: {e}"))?;
    }
    #[cfg(not(feature = "web-ui"))]
    if d.ui_addr.is_some() {
        return Err(
            "web dashboard disabled in this build (compiled with --features lean)".to_string(),
        );
    }

    handle.join_until_signal();
    ipc_handle.shutdown();
    Ok(())
}
