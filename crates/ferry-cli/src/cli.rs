use clap::{Parser, Subcommand};
use std::path::PathBuf;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

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
    #[arg(long, global = true)]
    pub json: bool,

    #[arg(short = 'v', long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Init {
        path: Option<PathBuf>,
    },

    Pair {
        #[arg(long, value_name = "PAYLOAD_FILE")]
        accept: Option<PathBuf>,

        dir: Option<PathBuf>,

        #[arg(long, default_value_t = 120, value_name = "SECONDS")]
        timeout_secs: u64,
    },

    Share {
        folder: Option<PathBuf>,

        #[arg(long)]
        i_know: bool,

        #[arg(long, default_value_t = 120, value_name = "SECONDS")]
        timeout_secs: u64,
    },

    Join {
        code: String,

        dest: Option<PathBuf>,
    },

    Status {
        folder: Option<PathBuf>,
    },

    Pin {
        #[command(subcommand)]
        action: PinAction,
    },

    Conflicts {
        #[command(subcommand)]
        action: ConflictsAction,
    },

    Ignore {
        pattern: Option<String>,

        #[arg(long, value_name = "NAME")]
        preset: Option<String>,

        #[arg(long)]
        list: bool,

        #[arg(value_name = "FOLDER")]
        folder: Option<PathBuf>,
    },

    Store {
        #[command(subcommand)]
        action: StoreAction,
    },

    Daemon {
        #[command(subcommand)]
        action: Option<DaemonAction>,

        folders: Vec<PathBuf>,

        #[arg(long, value_name = "ADDR")]
        listen: Option<String>,

        #[arg(long, value_name = "URL", alias = "peer")]
        peer_url: Option<String>,

        #[arg(long, default_value = "tcp", value_name = "KIND")]
        transport: String,

        #[arg(long, default_value_t = 1, value_name = "SECONDS")]
        interval_secs: u64,
    },

    Sync {
        folder: Option<PathBuf>,

        #[arg(long, value_name = "URL", alias = "peer")]
        peer_url: Option<String>,

        #[arg(long, default_value_t = 30, value_name = "SECONDS")]
        timeout_secs: u64,

        #[arg(long, default_value = "tcp", value_name = "KIND")]
        transport: String,
    },
    #[cfg(feature = "tui")]
    Tui {
        folder: Option<PathBuf>,
    },

    Ui {
        #[command(subcommand)]
        subcommand: Option<UiSubcommand>,

        folder: Option<PathBuf>,

        #[arg(long)]
        gui: bool,

        #[arg(long)]
        web: bool,

        #[arg(long)]
        tui: bool,

        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        #[arg(short, long, default_value_t = 0)]
        port: u16,

        #[arg(long)]
        no_open: bool,

        #[arg(long, hide = true)]
        test: bool,
    },
}

#[derive(Debug, Subcommand, Clone, PartialEq, Eq)]
pub enum UiSubcommand {
    Token { folder: Option<PathBuf> },
}

#[derive(Debug, Subcommand)]
pub enum ConflictsAction {
    List,
}

#[derive(Debug, Subcommand)]
pub enum StoreAction {
    Gc {
        #[arg(long)]
        dry_run: bool,

        #[arg(long, default_value_t = 24 * 60 * 60, value_name = "SECONDS")]
        grace_secs: u64,

        folder: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
pub enum PinAction {
    Start {
        #[arg(long = "paths", value_name = "GLOB")]
        paths: Vec<String>,

        #[arg(long, default_value_t = 8, value_name = "HOURS")]
        hours: u64,

        folder: Option<PathBuf>,
    },

    Stop {
        folder: Option<PathBuf>,
    },

    Release {
        folder: Option<PathBuf>,
    },

    Status {
        folder: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand, Clone, PartialEq, Eq)]
pub enum DaemonAction {
    Stop,

    Status,
}
