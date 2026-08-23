//! M0 walking skeleton, THE acceptance (T-006), verbatim from the ticket:
//!
//! "script starts both daemons, touches 50 random files including an
//! append-heavy log file, asserts convergence within N seconds, tears down."
//!
//! This is the programmatic half: the same scenario run in-process over a
//! real `TcpTransport` on loopback — two full engine instances, two separate
//! stores and trees, nothing shared but TCP. `scripts/skeleton-e2e.sh` is
//! the process-level twin of this file.
//!
//! N defaults to 30 seconds; override with FERRY_SYNC_TEST_TIMEOUT_SECS.

mod common;

use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::time::Duration;

use common::{EngineFixture, TreeBuilder};
use ferry_sync::format::hex;

/// Deterministic per-test seed so failures reproduce.
const SEED: u64 = 20260824;

#[test]
fn fifty_random_files_plus_append_heavy_log_converge_within_n_seconds() {
    let timeout = common::timeout_from_env();
    let fx = EngineFixture::start("conv", SEED);

    // --- mutate: 50 random files across nested dirs + one append-heavy log.
    let mut builder = TreeBuilder::new(fx.tree_a(), SEED);
    builder.create_random_files(50);
    let exec_file = "scripts/run.sh";
    builder.write_exec(exec_file, b"#!/bin/sh\necho skeleton\n");

    let log_rel = "logs/app.log";
    let total_lines = 250usize;
    let writer = spawn_log_writer(fx.tree_a().join(log_rel), total_lines);

    // Writer appends for ~1-2s while both engines poll and sync. The
    // converged copy must contain ALL lines once the writer is done.
    writer.join().expect("log writer thread");

    // --- assert convergence within N seconds of quiescence.
    let deadline = std::time::Instant::now() + timeout;
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "no convergence within {:?}; state A={:?} B={:?}",
            timeout,
            fx.a.stats(),
            fx.b.stats()
        );
        if fx.converged() && trees_byte_equal(&fx) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // Manifest ids agree on both sides and are recorded against each peer.
    let agreed = fx.a.agreed_id().expect("A agreed");
    assert_eq!(agreed, fx.b.agreed_id().expect("B agreed"));
    assert_ne!(agreed, [0u8; 32]);

    // The complete log tail: every appended line, no tearing.
    let got_lines = count_lines(&fx.tree_b().join(log_rel));
    assert_eq!(got_lines, total_lines, "log tail torn on receiving side");
    let last = last_line(&fx.tree_b().join(log_rel));
    let want_payload = format!("{:x}", SEED ^ total_lines as u64);
    assert_eq!(
        last,
        format!("log line {total_lines} payload-{want_payload}")
    );

    // Exec bit survived the trip (SPEC permission subset).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(fx.tree_b().join(exec_file))
            .unwrap()
            .permissions()
            .mode();
        assert_ne!(mode & 0o111, 0, "exec bit lost in transfer");
    }

    // Both engines saw at least one successful session.
    assert!(fx.a.stats().sessions_ok >= 1);
    assert!(fx.b.stats().sessions_ok >= 1);

    println!(
        "convergence ok: agreed={} stats A={:?} B={:?}",
        hex(&agreed),
        fx.a.stats(),
        fx.b.stats()
    );
}

/// Bidirectional: edits on either side converge. Two sequential phases on
/// one long-lived pair; simultaneous same-tick edits are explicitly OUT of
/// scope for M0 (T-010 owns conflict handling).
#[test]
fn edits_on_either_side_converge_both_directions() {
    let fx = EngineFixture::start("bidi", SEED + 1);

    // Phase 1: edit on A only.
    let mut b1 = TreeBuilder::new(fx.tree_a(), SEED + 2);
    b1.write("from-a.txt", b"written on node A");
    b1.write("nested/deep/from-a.bin", &vec![7u8; 4096]);
    wait_converged(&fx, common::timeout_from_env());
    assert!(trees_byte_equal(&fx), "phase 1 (A->B)");

    // Phase 2: edit on B only.
    let mut b2 = TreeBuilder::new(fx.tree_b(), SEED + 3);
    b2.write("from-b.txt", b"written on node B");
    b2.write("from-a.txt", b"edited on node B"); // overwrite of A's file
    b2.remove("nested/deep/from-a.bin");
    wait_converged(&fx, common::timeout_from_env());
    assert!(trees_byte_equal(&fx), "phase 2 (B->A)");
    assert_eq!(
        fs::read(fx.tree_a().join("from-a.txt")).unwrap(),
        b"edited on node B",
        "B's overwrite must win on A"
    );
    assert!(
        !fx.tree_a().join("nested/deep/from-a.bin").exists(),
        "deletion must propagate"
    );
}

fn wait_converged(fx: &EngineFixture, timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    while !(fx.converged() && trees_byte_equal(fx)) {
        assert!(
            std::time::Instant::now() < deadline,
            "no convergence within {timeout:?}: A={:?} B={:?}",
            fx.a.stats(),
            fx.b.stats()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn trees_byte_equal(fx: &EngineFixture) -> bool {
    common::trees_identical(&fx.tree_a(), &fx.tree_b())
}

fn count_lines(p: &Path) -> usize {
    fs::read_to_string(p).unwrap().lines().count()
}

fn last_line(p: &Path) -> String {
    fs::read_to_string(p)
        .unwrap()
        .lines()
        .last()
        .unwrap_or_default()
        .to_string()
}

/// Appends numbered lines with small sleeps, mimicking an agent or server
/// writing a log continuously while sync runs in the background.
fn spawn_log_writer(path: std::path::PathBuf, lines: usize) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        for i in 1..=lines {
            writeln!(f, "log line {i} payload-{:x}", SEED ^ i as u64).unwrap();
            f.flush().unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
    })
}
