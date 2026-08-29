//! The command surface (clap 4 derive). Parsing is unit-tested as a table
//! in `tests/cli_parse.rs`; every command maps onto a plain function that
//! takes explicit parameters, so tests never shell out.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Long-form help shown by `ferry --help`. clap renders the derived doc
/// comment; this constant backs the `--help` epilog with the five-minute
/// path.
pub const AFTER_HELP: &str = "\
Two-minute path (zero-config):
  ferry                         launch UI (auto-detects GUI/Web/TUI, bootstraps daemon)
  ferry share [FOLDER]          create a share code for FOLDER (prints code + QR)
  ferry join <CODE> [DEST]      join a shared folder at DEST using CODE
Five-minute path (legacy):
  ferry init            inside your project
  ferry pair            on this machine; follow the printed steps on the other device
  ferry daemon --listen 127.0.0.1:44001        (device A)
  ferry daemon --peer-url 127.0.0.1:44001      (device B)
Every command accepts --json for stable machine-readable output
(schemas: docs/cli-json.md).";

/// Long-form help shown by `ferry daemon --help`.
pub const DAEMON_AFTER_HELP: &str = "\
Web dashboard (v0):
  ferry daemon --ui [HOST:PORT]        (default 127.0.0.1:8098)
--ui serves the local web dashboard over HTTP while syncing runs.
v0 stance: LOOPBACK BIND ONLY and no auth token — anyone who can reach
the port can read folder state, so keep the default 127.0.0.1 address.
Design notes: .scratch/web-dashboard/spec.md.";

#[derive(Debug, Parser)]
#[command(
    name = "ferry",
    version = VERSION,
    about = "Encrypted peer-to-peer sync for developer directories",
    long_about = "Ferry syncs developer project directories between your machines, \
end-to-end encrypted and peer-to-peer. Git stays in charge of source history; \
Ferry carries everything else.",
    after_help = AFTER_HELP
)]
pub struct Cli {
    /// Emit a single stable JSON document instead of human text.
    #[arg(long, global = true)]
    pub json: bool,

    /// More progress detail on stderr.
    #[arg(short = 'v', long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create a Ferry folder here: identity, encrypted store, ignore rules.
    Init {
        /// Folder to initialize (default: current directory).
        path: Option<PathBuf>,
    },
    /// Add another synced folder under the same identity.
    Add {
        /// Folder to initialize.
        path: PathBuf,
    },
    /// Pair devices and share this folder via an out-of-band payload file.
    ///
    /// `ferry pair` prints a short code + QR and writes an offer file;
    /// `ferry pair --accept <file>` (run on the other device) completes the
    /// exchange. The payload FILE stands in for QR-camera transport across
    /// machines: move it however you move secrets (`AirDrop`, scp, USB).
    Pair {
        /// Accept mode: path to the offer file written by the other device.
        #[arg(long, value_name = "PAYLOAD_FILE")]
        accept: Option<PathBuf>,
        /// Accept mode only: folder to create/adopt (default: current directory).
        dir: Option<PathBuf>,
        /// Seconds to wait for the other side's file before giving up.
        #[arg(long, default_value_t = 120, value_name = "SECONDS")]
        timeout_secs: u64,
    },
    /// Prepare this folder for another device: secret-scan first, then emit
    /// a share payload.
    Share {
        /// Folder to share (default: current directory).
        folder: Option<PathBuf>,
        /// Confirm you have read the secret warnings and want to proceed.
        #[arg(long)]
        i_know: bool,
        /// Seconds to wait for the accepting device's response.
        #[arg(long, default_value_t = 120, value_name = "SECONDS")]
        timeout_secs: u64,
    },
    /// Join a shared folder using a pairing code.
    Join {
        /// Pairing code (6 chars, case-insensitive, dashes/spaces ignored).
        code: String,
        /// Destination folder (default: current directory).
        dest: Option<PathBuf>,
    },
    /// Show what Ferry knows about a folder right now.
    Status {
        /// Folder to inspect (default: current directory).
        folder: Option<PathBuf>,
    },
    /// Session pinning: declare this device the active writer for a while.
    ///
    /// While pinned, competing remote edits to the pinned paths are HELD
    /// and surfaced instead of racing your tree; `ferry pin release`
    /// reconciles them through the ordinary three-way engine (ADR-0004:
    /// winners live, losers quarantined, nothing merged, nothing lost).
    Pin {
        #[command(subcommand)]
        action: PinAction,
    },
    /// Work with the structured conflict report.
    Conflicts {
        #[command(subcommand)]
        action: ConflictsAction,
    },
    /// Manage selective rules: append patterns, apply presets, list layers.
    Ignore {
        /// gitignore-syntax pattern line to append to ferry.ignore.
        pattern: Option<String>,
        /// Apply a built-in agent-state preset (`claude`, `opencode`).
        #[arg(long, value_name = "NAME")]
        preset: Option<String>,
        /// Show the effective rule layers with precedence annotations.
        #[arg(long)]
        list: bool,
        /// Folder to target (default: current directory).
        #[arg(value_name = "FOLDER")]
        folder: Option<PathBuf>,
    },
    /// Store maintenance: garbage-collect unreferenced packs (T-20).
    Store {
        #[command(subcommand)]
        action: StoreAction,
    },
    /// Watch folders and continuously exchange with one peer over TCP.
    #[command(after_help = DAEMON_AFTER_HELP)]
    Daemon {
        /// Folders to watch (default: current directory).
        folders: Vec<PathBuf>,
        /// Bind address to LISTEN on (e.g. 127.0.0.1:44001). The listener
        /// serves sessions; peers discover its changes via incoming dials.
        #[arg(long, value_name = "ADDR")]
        listen: Option<String>,
        /// Peer address to DIAL (e.g. 127.0.0.1:44001). The dialer drives
        /// exchange rounds every --interval-secs.
        #[arg(long, value_name = "URL", alias = "peer")]
        peer_url: Option<String>,
        /// Transport implementation. Only `tcp` exists today; iroh QUIC
        /// lands with T-009/T-014 and any other value fails cleanly.
        #[arg(long, default_value = "tcp", value_name = "KIND")]
        transport: String,
        /// Seconds between scan+exchange rounds (dialer role).
        #[arg(long, default_value_t = 1, value_name = "SECONDS")]
        interval_secs: u64,
    },
    /// One-shot sync: exchange rounds until both sides agree (or timeout).
    Sync {
        /// Folder to sync (default: current directory).
        folder: Option<PathBuf>,
        /// Peer address to dial (e.g. 127.0.0.1:44001).
        #[arg(long, value_name = "URL", alias = "peer")]
        peer_url: Option<String>,
        /// Give up after this many seconds (exit 1 unless converged).
        #[arg(long, default_value_t = 30, value_name = "SECONDS")]
        timeout_secs: u64,
        /// Transport implementation; only `tcp` today.
        #[arg(long, default_value = "tcp", value_name = "KIND")]
        transport: String,
    },
    #[cfg(feature = "tui")]
    /// Launch the interactive terminal user interface dashboard.
    Tui {
        /// Folder to monitor (default: current directory).
        folder: Option<PathBuf>,
    },
    /// Launch Ferry graphical, web, or terminal user interface.
    Ui {
        /// Folder to monitor/serve (default: current directory).
        folder: Option<PathBuf>,
        /// Launch native desktop graphical interface.
        #[arg(long)]
        gui: bool,
        /// Launch ephemeral web dashboard in browser.
        #[arg(long)]
        web: bool,
        /// Launch interactive terminal dashboard.
        #[arg(long)]
        tui: bool,
        /// Bind host for web UI (default: 127.0.0.1).
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Port to bind for web UI (default: 0 for random ephemeral port).
        #[arg(short, long, default_value_t = 0)]
        port: u16,
        /// Do not open the browser automatically.
        #[arg(long)]
        no_open: bool,
        /// Test mode: start server/frontend, verify startup, and exit immediately.
        #[arg(long, hide = true)]
        test: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConflictsAction {
    /// List recorded conflicts, oldest first.
    List,
}

#[derive(Debug, Subcommand)]
pub enum StoreAction {
    /// Collect packs no live manifest can reach.
    ///
    /// Liveness roots: every last-agreed manifest recorded for this folder
    /// plus every held-change manifest still awaiting `ferry pin release`.
    /// Explicit user action only — nothing is ever auto-deleted; packs
    /// younger than --grace-secs are never removed, so an in-flight writer
    /// or a just-published manifest is always safe. Quarantined conflict
    /// copies are ordinary tree files (ADR-0004) and are untouched.
    Gc {
        /// Report what would be collected without deleting anything.
        #[arg(long)]
        dry_run: bool,
        /// Never delete packs younger than this many seconds.
        #[arg(long, default_value_t = 24 * 60 * 60, value_name = "SECONDS")]
        grace_secs: u64,
        /// Folder whose store to collect (default: current directory).
        folder: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
pub enum PinAction {
    /// Begin a pinned session for this folder (per-folder, one at a time).
    Start {
        /// gitignore-style globs scoping the hold (e.g. `src/**`). Repeat
        /// the flag for several patterns; omit to pin the whole folder
        /// (equivalent to `--paths '*'`).
        #[arg(long = "paths", value_name = "GLOB")]
        paths: Vec<String>,
        /// Duration of the protection window in hours.
        #[arg(long, default_value_t = 8, value_name = "HOURS")]
        hours: u64,
        /// Folder to pin (default: current directory).
        folder: Option<PathBuf>,
    },
    /// End the session without reconciling. Held changes stay ledgered on
    /// disk; `ferry pin release` still recovers them later.
    Stop {
        /// Folder to unpin (default: current directory).
        folder: Option<PathBuf>,
    },
    /// Reconcile every held change through the three-way engine: winner
    /// live, loser quarantined `path.ferry-conflict.<device>-<ts>`, entry
    /// in conflicts.jsonl. Never merges, never discards.
    Release {
        /// Folder to release (default: current directory).
        folder: Option<PathBuf>,
    },
    /// Show the pin state and the full held set.
    Status {
        /// Folder to inspect (default: current directory).
        folder: Option<PathBuf>,
    },
}
