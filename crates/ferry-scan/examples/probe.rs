use ferry_scan::engine::{ScanEngine, StoreHandle};
use ferry_scan::policy::WatchSignal;
use ferry_store::crypto::PassthroughCipher;
use ferry_store::store::Store;
use notify::Watcher as _;
use rand::SeedableRng;
use std::time::Duration;

fn main() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("probe");
    std::fs::create_dir_all(&root).unwrap();
    let store_root = tmp.path().join("store-root");
    std::fs::create_dir_all(&store_root).unwrap();
    let store = Store::create(&store_root, [1u8; 32], Box::new(PassthroughCipher)).unwrap();
    let handle = StoreHandle {
        store: store.into(),
        poly: ferry_store::chunker::ValidatedPoly::generate(
            &mut rand::rngs::StdRng::seed_from_u64(42),
        ),
        folder_id: [5; 16],
        device_id: [6; 32],
    };
    let engine = ScanEngine::watch(&root, handle).unwrap();
    println!(
        "initial: {:?}",
        engine.current().map(|c| (c.trigger, c.stats.files))
    );

    std::fs::write(root.join("x.txt"), b"hello").unwrap();
    engine.debug_inject_signal(WatchSignal::Overflow {
        reason: "probe".into(),
    });
    std::thread::sleep(Duration::from_secs(2));
    println!(
        "after inject: {:?}",
        engine
            .current()
            .map(|c| (c.trigger, c.stats.files, c.stats.bytes_chunked))
    );

    let (raw_tx, raw_rx) = std::sync::mpsc::channel();
    let mut raw = notify::RecommendedWatcher::new(
        move |res: Result<notify::Event, notify::Error>| {
            let _ = raw_tx.send(res.map(|e| format!("{:?} {:?}", e.kind, e.paths)));
        },
        notify::Config::default(),
    )
    .unwrap();
    raw.watch(&root, notify::RecursiveMode::Recursive).unwrap();

    for i in 0..5 {
        std::fs::write(root.join(format!("e{i}.txt")), format!("data{i}")).unwrap();
    }
    std::thread::sleep(Duration::from_secs(2));
    println!("raw notify events observed: {}", raw_rx.try_iter().count());
    println!(
        "after real events: {:?}",
        engine.current().map(|c| (c.trigger, c.stats.files))
    );
    println!(
        "last_pass: {:?}",
        engine.last_pass().map(|(t, s)| (t, s.files))
    );
}
