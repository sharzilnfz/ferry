#![allow(warnings, clippy::all, clippy::pedantic)]

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use ferry_cli::cli::{Cli, Command};
use ferry_cli::error::CliError;
use ferry_cli::out;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(&cli) {
        Ok(output) => {
            let code = output.exit_code;
            if cli.json {
                println!("{}", output.json);
            } else {
                print!("{}", output.human);
                let _ = std::io::stdout().flush();
            }
            ExitCode::from(code)
        }
        Err(e) => {
            report_error(&e, cli.json);
            ExitCode::from(e.exit_code())
        }
    }
}

fn report_error(e: &CliError, json_mode: bool) {
    let stderr = std::io::stderr();
    let mut lock = stderr.lock();
    if json_mode {
        let mut doc = serde_json::json!({
            "error": e.message,
            "code": e.code,
            "hint": e.hint,
        });
        if let (Some(obj), Some(detail)) = (doc.as_object_mut(), &e.detail) {
            for (k, v) in detail.as_object().into_iter().flatten() {
                obj.insert(k.clone(), v.clone());
            }
        }
        let _ = writeln!(lock, "{doc}");
    } else {
        let _ = writeln!(lock, "{}", out::error_text(&e.code, &e.message, &e.hint));
    }
}

fn dispatch(cli: &Cli) -> Result<out::Output, CliError> {
    let cmd = match &cli.command {
        Some(c) => c,
        None => {
            let has_display = std::env::var_os("DISPLAY").is_some()
                || std::env::var_os("WAYLAND_DISPLAY").is_some();
            if has_display {
                #[cfg(feature = "gui")]
                {
                    return ferry_cli::commands::ui::run(ferry_cli::commands::ui::UiArgs {
                        folder: None,
                        gui: true,
                        web: false,
                        tui: false,
                        host: "127.0.0.1",
                        port: 0,
                        no_open: false,
                        test: false,
                    });
                }
            }
            #[cfg(feature = "web-ui")]
            {
                return ferry_cli::commands::ui::run(ferry_cli::commands::ui::UiArgs {
                    folder: None,
                    gui: false,
                    web: true,
                    tui: false,
                    host: "127.0.0.1",
                    port: 0,
                    no_open: false,
                    test: false,
                });
            }
            #[cfg(all(not(feature = "web-ui"), feature = "tui"))]
            {
                return ferry_cli::commands::ui::run(ferry_cli::commands::ui::UiArgs {
                    folder: None,
                    gui: false,
                    web: false,
                    tui: true,
                    host: "127.0.0.1",
                    port: 0,
                    no_open: false,
                    test: false,
                });
            }

            return ferry_cli::commands::ui::run(ferry_cli::commands::ui::UiArgs {
                folder: None,
                gui: false,
                web: false,
                tui: false,
                host: "127.0.0.1",
                port: 0,
                no_open: false,
                test: false,
            });
        }
    };
    match cmd {
        Command::Init { path } => {
            let p: PathBuf = path.clone().unwrap_or_else(|| PathBuf::from("."));
            ferry_cli::commands::init::run(&p)
        }
        Command::Pair {
            accept,
            dir,
            timeout_secs,
        } => match accept {
            None => {
                let p = dir.clone().unwrap_or_else(|| PathBuf::from("."));
                let opened = ferry_cli::folder::open_folder(&p)?;
                let identity = ferry_cli::ensure_identity()?;
                ferry_cli::commands::pairing::initiate(&opened, &identity, *timeout_secs)
            }
            Some(offer_file) => {
                let identity = ferry_cli::ensure_identity()?;
                ferry_cli::commands::pairing::accept(
                    &identity,
                    offer_file,
                    dir.as_deref(),
                    *timeout_secs,
                )
            }
        },
        Command::Share {
            folder,
            i_know,
            timeout_secs,
        } => {
            let f = folder.clone().unwrap_or_else(|| PathBuf::from("."));
            ferry_cli::commands::share::run(&f, *i_know, *timeout_secs)
        }
        Command::Join { code, dest } => ferry_cli::commands::join::run(code, dest.as_deref()),
        Command::Status { folder } => {
            let f = folder.clone().unwrap_or_else(|| PathBuf::from("."));
            ferry_cli::commands::status::run(&f)
        }
        Command::Pin { action } => match action {
            ferry_cli::cli::PinAction::Start {
                paths,
                hours,
                folder,
            } => {
                let f = folder.clone().unwrap_or_else(|| PathBuf::from("."));
                ferry_cli::commands::pin::start(&f, paths, *hours)
            }
            ferry_cli::cli::PinAction::Stop { folder } => {
                let f = folder.clone().unwrap_or_else(|| PathBuf::from("."));
                ferry_cli::commands::pin::stop(&f)
            }
            ferry_cli::cli::PinAction::Release { folder } => {
                let f = folder.clone().unwrap_or_else(|| PathBuf::from("."));
                ferry_cli::commands::pin::release(&f)
            }
            ferry_cli::cli::PinAction::Status { folder } => {
                let f = folder.clone().unwrap_or_else(|| PathBuf::from("."));
                ferry_cli::commands::pin::status(&f)
            }
        },
        Command::Conflicts { action } => match action {
            ferry_cli::cli::ConflictsAction::List => {
                ferry_cli::commands::conflicts::run(&PathBuf::from("."))
            }
        },
        Command::Ignore {
            pattern,
            preset,
            list,
            folder,
        } => {
            let (target_folder, target_pattern) = if *list || preset.is_some() {
                if let Some(f) = folder {
                    (f.clone(), None)
                } else if let Some(p) = pattern {
                    (PathBuf::from(p), None)
                } else {
                    (PathBuf::from("."), None)
                }
            } else if let Some(f) = folder {
                (f.clone(), pattern.as_deref())
            } else {
                (PathBuf::from("."), pattern.as_deref())
            };
            ferry_cli::commands::ignore_cmd::run(
                &target_folder,
                target_pattern,
                preset.as_deref(),
                *list,
            )
        }
        Command::Store { action } => match action {
            ferry_cli::cli::StoreAction::Gc {
                dry_run,
                grace_secs,
                folder,
            } => {
                let f = folder.clone().unwrap_or_else(|| PathBuf::from("."));
                ferry_cli::commands::store::run(ferry_cli::commands::store::GcArgs {
                    folder: &f,
                    dry_run: *dry_run,
                    grace_secs: *grace_secs,
                })
            }
        },
        Command::Daemon {
            action,
            folders,
            listen,
            peer_url,
            transport,
            interval_secs,
        } => match action {
            Some(ferry_cli::cli::DaemonAction::Stop) => ferry_cli::commands::daemon::stop(),
            Some(ferry_cli::cli::DaemonAction::Status) => ferry_cli::commands::daemon::status(),
            None => ferry_cli::commands::daemon::run(ferry_cli::commands::daemon::DaemonArgs {
                folders,
                listen: listen.as_deref(),
                peer_url: peer_url.as_deref(),
                transport,
                interval_secs: *interval_secs,
                json: cli.json,
            }),
        },
        Command::Sync {
            folder,
            peer_url,
            timeout_secs,
            transport,
        } => ferry_cli::commands::sync::run(ferry_cli::commands::sync::SyncArgs {
            folder: folder.as_deref(),
            peer_url: peer_url.as_deref(),
            timeout_secs: *timeout_secs,
            transport,
        }),
        #[cfg(feature = "tui")]
        Command::Tui { folder } => ferry_cli::commands::tui::run(folder.as_deref()),
        Command::Ui {
            subcommand,
            folder,
            gui,
            web,
            tui,
            host,
            port,
            no_open,
            test,
        } => match subcommand {
            Some(ferry_cli::cli::UiSubcommand::Token {
                folder: sub_folder,
            }) => {
                let target = sub_folder.as_deref().or(folder.as_deref());
                ferry_cli::commands::ui::run_token(target)
            }
            None => ferry_cli::commands::ui::run(ferry_cli::commands::ui::UiArgs {
                folder: folder.as_deref(),
                gui: *gui,
                web: *web,
                tui: *tui,
                host,
                port: *port,
                no_open: *no_open,
                test: *test,
            }),
        },
    }
}
