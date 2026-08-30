//! T-004 acceptance gate (SPEC M1 scan-throughput risk):
//!
//! ```text
//! cargo run --release -p ferry-scan --bin bench-gate
//! ```
//!
//! Generates a deterministic 100k-file / ~500 MiB fixture, then measures:
//!
//! 1. INITIAL full scan — gate < 60 s
//! 2. INCREMENTAL rescan after exactly 100 changed files — gate < 2 s
//!
//! Methodology notes (repeated in the output):
//! - Fixture content is deterministic pseudo-random (splitmix64 per file),
//!   so runs are comparable and no external corpus is needed.
//! - The store lives INSIDE the fixture root at `.ferry` (realistic layout);
//!   its structural exclusion from scans is exercised by the benchmark
//!   itself.
//! - Gate timing for the incremental rescan measures the SCAN PASS ONLY
//!   (`scan_once` after events have settled), not notification latency or
//!   the debounce window. Both are engine-quality concerns measured
//!   elsewhere; the SPEC risk is scan throughput.
//! - The store uses the production ChaCha20-Poly1305 cipher, so the gates
//!   measure the real end-to-end scan cost (encryption adds a near-constant
//!   per-byte multiplier on top of chunking).
//! - Machine info is printed for the record; gates are absolute per ticket.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ferry_crypto::pack_cipher::ChaChaCipher;
use ferry_scan::config::ScanConfig;
use ferry_scan::engine::{ScanEngine, StoreHandle};
use ferry_store::diff::diff_manifests;
use ferry_store::store::Store;

const DIRS: usize = 100;
const FILES_PER_DIR: usize = 1_000;
const TARGET_TOTAL_BYTES: u64 = 500 * 1024 * 1024;
const CHANGED_COUNT: usize = 100;
const INIT_GATE: f64 = 60.0;
const INCR_GATE: f64 = 2.0;

/// splitmix64 — tiny deterministic PRNG for fixture bytes and sizes.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn fill_block(buf: &mut [u8], seed: &mut u64) {
    for chunk in buf.chunks_mut(8) {
        let v = splitmix64(seed).to_le_bytes();
        chunk.copy_from_slice(&v[..chunk.len()]);
    }
}

fn machine_info() -> String {
    #[cfg(target_os = "macos")]
    {
        let cpu = std::process::Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .ok()
            .map_or_else(
                || "unknown".into(),
                |o| String::from_utf8_lossy(&o.stdout).trim().to_string(),
            );
        let ram = std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .parse::<u64>()
                    .ok()
            })
            .map_or_else(
                || "unknown".into(),
                |b| format!("{:.0} GiB", b as f64 / 1024.0 / 1024.0 / 1024.0),
            );
        let cores = std::process::Command::new("sysctl")
            .args(["-n", "hw.ncpu"])
            .output()
            .ok()
            .map_or_else(
                || "?".into(),
                |o| String::from_utf8_lossy(&o.stdout).trim().to_string(),
            );
        let ver = std::process::Command::new("sw_vers")
            .args(["-productVersion"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        format!("macOS {ver}; {cpu}; {cores} cores; {ram} RAM")
    }
    #[cfg(not(target_os = "macos"))]
    {
        let os = std::env::consts::OS;
        format!("{os} (fill in CPU/RAM from /proc on Linux hosts)")
    }
}

fn main() {
    println!("=== ferry-scan bench-gate (T-004) ===");
    println!("machine: {}", machine_info());
    println!(
        "fixture : {DIRS} dirs x {FILES_PER_DIR} files = {} files, target ~{} MiB",
        DIRS * FILES_PER_DIR,
        TARGET_TOTAL_BYTES / 1024 / 1024
    );

    // Workspace root = crate dir ../../.. ; fixtures under target/ so they
    // never pollute the repo.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let bench_root = manifest_dir
        .join("../../target/bench-fixture")
        .canonicalize()
        .unwrap_or_else(|_| manifest_dir.join("../../target"));
    let fixture = bench_root.join("scan-fixture-100k");
    let _ = std::fs::remove_dir_all(&fixture);
    std::fs::create_dir_all(&fixture).unwrap();
    let fixture = fixture.canonicalize().unwrap();

    // ---- 1. generate fixture ------------------------------------------------
    let t_gen = Instant::now();
    let total_bytes = generate_fixture(&fixture);
    let gen_secs = t_gen.elapsed().as_secs_f64();
    println!(
        "generated {} files, {} MiB in {:.1}s -> {}",
        DIRS * FILES_PER_DIR,
        total_bytes / 1024 / 1024,
        gen_secs,
        fixture.display()
    );

    // ---- 2. store inside the watched root ----------------------------------
    let store = Arc::new(Store::create(&fixture, [1u8; 32], Box::new(ChaChaCipher)).unwrap());
    let handle = StoreHandle {
        store: store.clone(),
        poly: ferry_store::chunker::ValidatedPoly::new(0x0025_b468_838d_cb75 | (1 << 53))
            .expect("bench polynomial constant is monic irreducible of degree 53"),
        folder_id: [7; 16],
        device_id: [8; 32],
    };

    // ---- 3. GATE: initial full scan ----------------------------------------
    let cfg = ScanConfig {
        quiet_window: Duration::from_millis(150),
        audit_interval: Duration::from_hours(1),
        poll_interval: Duration::from_secs(10),
    };
    let t0 = Instant::now();
    let engine = ScanEngine::watch_with(
        fixture.clone(),
        handle,
        cfg,
        Arc::new(ferry_scan::NoIgnores),
    )
    .expect("initial scan");
    let init_secs = t0.elapsed().as_secs_f64();
    let baseline = engine.current().expect("baseline published");
    println!(
        "INITIAL full scan: {:>7.2} s ({} files, {} MiB hashed) [gate <{INIT_GATE}s] {}",
        init_secs,
        baseline.stats.files,
        baseline.stats.bytes_chunked / 1024 / 1024,
        verdict(init_secs < INIT_GATE),
    );
    assert_eq!(baseline.stats.files, DIRS * FILES_PER_DIR);

    // ---- 4. change exactly 100 DISTINCT files ------------------------------
    let mut rng_state = 0xC10C_u64;
    let n = (DIRS * FILES_PER_DIR) as u64;
    // Deterministic shuffle => 100 distinct targets, spread across the tree.
    let mut idx: Vec<u64> = (0..n).collect();
    for i in (1..n).rev() {
        let j = splitmix64(&mut rng_state) % (i + 1);
        idx.swap(i as usize, j as usize);
    }
    let mut changed: Vec<PathBuf> = Vec::with_capacity(CHANGED_COUNT);
    for &file_idx in &idx[..CHANGED_COUNT] {
        let d = (file_idx / FILES_PER_DIR as u64) as usize;
        let f = (file_idx % FILES_PER_DIR as u64) as usize;
        let p = file_path(&fixture, d, f);
        let mut seed = 0xF00D_0000 + file_idx;
        let len = p.metadata().unwrap().len() as usize;
        let mut buf = vec![0u8; len];
        fill_block(&mut buf, &mut seed);
        std::fs::write(&p, &buf).unwrap();
        changed.push(p);
    }
    assert_eq!(changed.len(), CHANGED_COUNT);

    // Let the watcher queue the burst (FSEvents coalesces within ms).
    std::thread::sleep(Duration::from_millis(700));

    // ---- 5. GATE: incremental rescan of those 100 --------------------------
    // The worker may win the race (it debounces 150 ms then scans), or this
    // scan_once may drain the queue first; either way the measured quantity
    // is the completed PASS duration recorded in stats (walk+hash+store IO).
    let _ = engine.scan_once();
    let deadline = Instant::now() + Duration::from_secs(10);
    let updated = wait_for_new_current(&engine, &baseline.manifest_id, deadline);
    let incr_secs = updated.stats.duration.as_secs_f64();
    println!(
        "INCREMENTAL after {CHANGED_COUNT} changes: {incr_secs:.3} s \
         ({} dirs rebuilt, {} files rehashed, {} KiB hashed) [gate <{INCR_GATE}s] {}",
        updated.stats.dirty_dirs,
        updated.stats.files_rehashed,
        updated.stats.bytes_chunked / 1024,
        verdict(incr_secs < INCR_GATE),
    );

    // Correctness: EXACTLY the 100 touched files moved.
    let cs = diff_manifests(&store, &baseline.manifest, &updated.manifest).unwrap();
    let modified = cs.content_modified.len() + cs.metadata_modified.len();
    assert_eq!(cs.added.len(), 0, "{cs:?}");
    assert_eq!(cs.removed.len(), 0, "{cs:?}");
    assert_eq!(modified, CHANGED_COUNT, "mutation set must be exact");

    // Short-circuit proof at scale: zero-change pass hashes nothing.
    let idle = engine.scan_once().unwrap();
    assert_eq!(idle.stats.bytes_chunked, 0);

    println!("correctness: mutation set == exactly {CHANGED_COUNT} content_modified ✓");
    println!("short-circuit: zero-change pass hashed 0 bytes ✓");

    let pass = init_secs < INIT_GATE && incr_secs < INCR_GATE;
    println!(
        "\n{{\"initial_secs\": {init_secs:.2}, \"incremental_secs\": {incr_secs:.3}, \"files\": {}, \"total_bytes\": {total_bytes}, \"gate_initial\": {INIT_GATE}, \"gate_incremental\": {INCR_GATE}, \"pass\": {pass}}}",
        DIRS * FILES_PER_DIR
    );
    if !pass {
        std::process::exit(1);
    }
}

fn verdict(ok: bool) -> &'static str {
    if ok {
        "PASS"
    } else {
        "FAIL"
    }
}

fn wait_for_new_current(
    engine: &ScanEngine,
    prev: &ferry_store::BlobId,
    deadline: Instant,
) -> Arc<ferry_scan::CurrentScan> {
    loop {
        assert!(Instant::now() < deadline, "no new scan published in time");
        if let Some(c) = engine.current() {
            if &c.manifest_id != prev {
                return c;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn file_path(root: &Path, d: usize, f: usize) -> PathBuf {
    root.join(format!("cluster_{d:03}"))
        .join(format!("file_{f:05}.bin"))
}

fn generate_fixture(root: &Path) -> u64 {
    let mut total = 0u64;
    for d in 0..DIRS {
        let dir = root.join(format!("cluster_{d:03}"));
        std::fs::create_dir(&dir).unwrap();
        for f in 0..FILES_PER_DIR {
            let mut seed = (d as u64) << 32 | f as u64;
            // Mean ~5 KiB with deterministic variance; lands near 500 MiB.
            let size = 2048 + (splitmix64(&mut seed) % 6144) as usize;
            let mut buf = vec![0u8; size];
            fill_block(&mut buf, &mut seed);
            let path = dir.join(format!("file_{f:05}.bin"));
            let mut file = std::fs::File::create(path).unwrap();
            file.write_all(&buf).unwrap();
            total += size as u64;
        }
        if d % 10 == 9 {
            eprintln!("  generated {}/{} dirs...", d + 1, DIRS);
        }
    }
    total
}
