//! Manual probe for debugging the worker pipeline (not a test).
//! Run: cargo run -p ferry-scan --example probe

use std::time::Duration;
use ferry_scan::engine::{ScanEngine, StoreHandle};
use ferry_scan::policy::WatchSignal;
use ferry_store::crypto::PassthroughCipher;
use ferry_store::store::Store;
use rand::SeedableRng;

fn main() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("probe");
    std::fs::create_dir_all(&root).unwrap();
    let store_root = tmp.path().join("store-root");
    std::fs::create_dir_all(&store_root).unwrap();
    let store = Store::create(&store_root, [1u8; 32], Box::new(PassthroughCipher)).unwrap();
    let handle = StoreHandle {
        store: store.into(),
        poly: ferry_store::chunker::generate_polynomial(&mut rand::rngs::StdRng::seed_from_u64(42)),
        folder_id: [5; 16],
        device_id: [6; 32],
    };
    let engine = ScanEngine::watch(&root, handle).unwrap();
    println!("initial: {:?}", engine.current().map(|c| (c.trigger, c.stats.files)));

    std::fs::write(root.join("x.txt"), b"hello").unwrap();
    engine.debug_inject_signal(WatchSignal::Overflow { reason: "probe".into() });
    std::thread::sleep(Duration::from_secs(2));
    println!(
        "after inject: {:?}",
        engine.current().map(|c| (c.trigger, c.stats.files, c.stats.bytes_chunked))
    );

    // Real-event path: write files, wait past quiet window, check worker.
    for i in 0..5 {
        std::fs::write(root.join(format!("e{i}.txt")), format!("data{i}")).unwrap();
    }
    std::thread::sleep(Duration::from_secs(2));
    println!(
        "after real events: {:?}",
        engine.current().map(|c| (c.trigger, c.stats.files))
    );
}
