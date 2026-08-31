use std::path::PathBuf;

use clap::Parser;
use ferry_cli::cli::{Cli, Command};

fn parse(args: &[&str]) -> Cli {
    Cli::try_parse_from(std::iter::once("ferry").chain(args.iter().copied()))
        .unwrap_or_else(|e| panic!("parse failed for {args:?}: {e}"))
}

fn expect_init(args: &[&str]) -> Option<PathBuf> {
    match parse(args).command.unwrap() {
        Command::Init { path } => path.clone(),
        other => panic!("expected Init, got {other:?}"),
    }
}

#[test]
fn global_flags_parse_in_any_position() {
    let cli = parse(&["--json", "status", "somewhere"]);
    assert!(cli.json);
    let cli = parse(&["status", "--json", "somewhere"]);
    assert!(cli.json, "global flags work after the subcommand");
    assert!(!cli.verbose);
    let cli = parse(&["-v", "--json", "init"]);
    assert!(cli.verbose && cli.json);
    let plain = parse(&["status"]);
    assert!(!plain.json && !plain.verbose);
}

#[test]
fn init_defaults_to_cwd_and_takes_a_path() {
    assert_eq!(expect_init(&["init"]), None);
    assert_eq!(
        expect_init(&["init", "/tmp/proj"]),
        Some(PathBuf::from("/tmp/proj"))
    );
}

#[test]
fn add_is_rejected() {
    assert!(
        Cli::try_parse_from(["ferry", "add", "x"]).is_err(),
        "add subcommand should be rejected"
    );
    assert!(
        Cli::try_parse_from(["ferry", "add"]).is_err(),
        "add without path should be rejected"
    );
}

#[test]
fn pair_has_initiate_and_accept_forms() {
    match parse(&["pair"]).command.unwrap() {
        Command::Pair {
            accept,
            dir,
            timeout_secs,
        } => {
            assert_eq!(accept, None);
            assert_eq!(dir, None);
            assert_eq!(timeout_secs, 120);
        }
        other => panic!("{other:?}"),
    }

    match parse(&["pair", "--accept", "/tmp/offer.ferry-pair", "dest"])
        .command
        .unwrap()
    {
        Command::Pair { accept, dir, .. } => {
            assert_eq!(accept, Some(PathBuf::from("/tmp/offer.ferry-pair")));
            assert_eq!(dir, Some(PathBuf::from("dest")));
        }
        other => panic!("{other:?}"),
    }

    match parse(&["pair", "--timeout-secs", "5"]).command.unwrap() {
        Command::Pair { timeout_secs, .. } => assert_eq!(timeout_secs, 5),
        other => panic!("{other:?}"),
    }
}

#[test]
fn share_gating_flag_and_folder() {
    match parse(&["share"]).command.unwrap() {
        Command::Share { folder, i_know, .. } => {
            assert_eq!(folder, None);
            assert!(!i_know);
        }
        other => panic!("{other:?}"),
    }
    match parse(&["share", "--i-know", "sub/dir"]).command.unwrap() {
        Command::Share { folder, i_know, .. } => {
            assert_eq!(folder, Some(PathBuf::from("sub/dir")));
            assert!(i_know);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn conflicts_needs_list_subcommand() {
    assert!(Cli::try_parse_from(["ferry", "conflicts"]).is_err());
    match parse(&["conflicts", "list"]).command.unwrap() {
        Command::Conflicts { action } => {
            let _: ferry_cli::cli::ConflictsAction = action;
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn ignore_pattern_preset_and_list() {
    match parse(&["ignore", "*.log"]).command.unwrap() {
        Command::Ignore { pattern, list, .. } => {
            assert_eq!(pattern.as_deref(), Some("*.log"));
            assert!(!list);
        }
        other => panic!("{other:?}"),
    }
    match parse(&["ignore", "--list"]).command.unwrap() {
        Command::Ignore {
            pattern,
            preset: _,
            list,
            ..
        } => {
            assert_eq!(pattern, None);
            assert!(list);
        }
        other => panic!("{other:?}"),
    }
    match parse(&["ignore", "--preset", "claude"]).command.unwrap() {
        Command::Ignore { preset, .. } => assert_eq!(preset.as_deref(), Some("claude")),
        other => panic!("{other:?}"),
    }
    match parse(&["ignore", "--list", "/tmp/proj"]).command.unwrap() {
        Command::Ignore {
            pattern,
            list,
            folder,
            ..
        } => {
            assert_eq!(pattern.as_deref(), Some("/tmp/proj"));
            assert_eq!(folder, None);
            assert!(list);
        }
        other => panic!("{other:?}"),
    }
    match parse(&["ignore", "--preset", "claude", "/tmp/proj"])
        .command
        .unwrap()
    {
        Command::Ignore {
            preset, pattern, ..
        } => {
            assert_eq!(preset.as_deref(), Some("claude"));
            assert_eq!(pattern.as_deref(), Some("/tmp/proj"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn daemon_folders_listen_peer_transport_interval() {
    match parse(&["daemon"]).command.unwrap() {
        Command::Daemon { folders, .. } => assert!(folders.is_empty()),
        other => panic!("{other:?}"),
    }
    let cli = parse(&[
        "-v",
        "daemon",
        "a",
        "b",
        "--listen",
        "127.0.0.1:44001",
        "--interval-secs",
        "2",
    ]);
    match cli.command.unwrap() {
        Command::Daemon {
            action: _,
            folders,
            listen,
            peer_url,
            transport,
            interval_secs,
        } => {
            assert_eq!(folders.len(), 2);
            assert_eq!(listen.as_deref(), Some("127.0.0.1:44001"));
            assert_eq!(peer_url, None);
            assert_eq!(transport, "tcp");
            assert_eq!(interval_secs, 2);
        }
        other => panic!("{other:?}"),
    }

    let cli = parse(&["daemon", "--peer-url", "127.0.0.1:1"]);
    assert!(
        matches!(cli.command.as_ref().unwrap(), Command::Daemon { peer_url, .. } if peer_url.as_deref() == Some("127.0.0.1:1"))
    );
    let cli = parse(&["daemon", "--peer", "127.0.0.1:1"]);
    assert!(
        matches!(cli.command.as_ref().unwrap(), Command::Daemon { peer_url, .. } if peer_url.as_deref() == Some("127.0.0.1:1"))
    );

    let cli = parse(&["daemon", "--transport", "iroh"]);
    assert!(
        matches!(cli.command.as_ref().unwrap(), Command::Daemon { transport, .. } if transport == "iroh")
    );
}

#[test]
fn sync_flags() {
    let cli = parse(&[
        "sync",
        "myfolder",
        "--peer-url",
        "127.0.0.1:9",
        "--timeout-secs",
        "7",
    ]);
    match cli.command.unwrap() {
        Command::Sync {
            folder,
            peer_url,
            timeout_secs,
            ..
        } => {
            assert_eq!(folder, Some(PathBuf::from("myfolder")));
            assert_eq!(peer_url.as_deref(), Some("127.0.0.1:9"));
            assert_eq!(timeout_secs, 7);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn version_flag_is_wired() {
    let err = Cli::try_parse_from(["ferry", "--version"]).unwrap_err();
    use clap::error::ErrorKind::*;
    assert!(matches!(err.kind(), DisplayVersion), "{err}");
}

#[test]
fn pin_has_four_actions_and_repeatable_paths() {
    match parse(&["pin", "start"]).command.unwrap() {
        Command::Pin {
            action:
                ferry_cli::cli::PinAction::Start {
                    paths,
                    hours,
                    folder: None,
                },
        } => {
            assert!(paths.is_empty(), "no --paths means whole-folder");
            assert_eq!(hours, 8);
        }
        other => panic!("{other:?}"),
    }

    match parse(&["pin", "start", "--hours", "24"]).command.unwrap() {
        Command::Pin {
            action: ferry_cli::cli::PinAction::Start { hours, .. },
        } => assert_eq!(hours, 24),
        other => panic!("{other:?}"),
    }

    match parse(&[
        "pin", "start", "--paths", "src/**", "--paths", "docs/*", "sub",
    ])
    .command
    .unwrap()
    {
        Command::Pin {
            action:
                ferry_cli::cli::PinAction::Start {
                    paths,
                    hours,
                    folder: Some(folder),
                },
        } => {
            assert_eq!(paths, vec!["src/**".to_string(), "docs/*".to_string()]);
            assert_eq!(hours, 8);
            assert_eq!(folder, PathBuf::from("sub"));
        }
        other => panic!("{other:?}"),
    }

    for args in [
        vec!["pin", "stop"],
        vec!["pin", "release"],
        vec!["pin", "status", "elsewhere"],
    ] {
        let cli = parse(&args);
        assert!(
            matches!(cli.command.as_ref().unwrap(), Command::Pin { .. }),
            "{args:?}"
        );
    }

    assert!(Cli::try_parse_from(["ferry", "pin", "rebase"]).is_err());
}

#[test]
fn ui_flags_parse_web_gui_tui() {
    match parse(&["ui"]).command.unwrap() {
        Command::Ui {
            folder,
            gui,
            web,
            tui,
            host,
            port,
            no_open,
            test,
        } => {
            assert_eq!(folder, None);
            assert!(!gui);
            assert!(!web);
            assert!(!tui);
            assert_eq!(host, "127.0.0.1");
            assert_eq!(port, 0);
            assert!(!no_open);
            assert!(!test);
        }
        other => panic!("{other:?}"),
    }

    match parse(&["ui", "--gui", "my_folder"]).command.unwrap() {
        Command::Ui {
            folder,
            gui,
            web,
            tui,
            ..
        } => {
            assert_eq!(folder, Some(PathBuf::from("my_folder")));
            assert!(gui);
            assert!(!web);
            assert!(!tui);
        }
        other => panic!("{other:?}"),
    }

    match parse(&[
        "ui",
        "--web",
        "--host",
        "0.0.0.0",
        "-p",
        "8080",
        "--no-open",
    ])
    .command
    .unwrap()
    {
        Command::Ui {
            gui,
            web,
            tui,
            host,
            port,
            no_open,
            ..
        } => {
            assert!(!gui);
            assert!(web);
            assert!(!tui);
            assert_eq!(host, "0.0.0.0");
            assert_eq!(port, 8080);
            assert!(no_open);
        }
        other => panic!("{other:?}"),
    }

    match parse(&["ui", "--tui"]).command.unwrap() {
        Command::Ui { gui, web, tui, .. } => {
            assert!(!gui);
            assert!(!web);
            assert!(tui);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn help_mentions_the_five_minute_path() {
    let err = Cli::try_parse_from(["ferry", "--help"]).unwrap_err();
    assert!(err.to_string().contains("Five-minute path"));
}
