



mod common;

use std::time::Duration;

use common::{timeout_from_env, EngineFixture, TreeBuilder};

#[test]
fn steady_state_sync_never_triggers_full_index_rebuild() {
    assert_eq!(
        ferry_store::store::rebuild_index_calls(),
        0,
        "another test or startup path already rebuilt; this assertion only \
         holds while rebuild stays cold-start-only"
    );

    let fx = EngineFixture::start("incr", 777);

    let mut tb = TreeBuilder::new(fx.tree_a(), 4242);
    
    let paths = tb.create_random_files(10);

    let deadline = std::time::Instant::now() + timeout_from_env();
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "no convergence: A={:?} B={:?}",
            fx.a.stats(),
            fx.b.stats()
        );
        if fx.converged() && common::trees_identical(&fx.tree_a(), &fx.tree_b()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    
    
    let b_root = fx.tree_b();
    for rel in &paths {
        assert!(b_root.join(rel).is_file(), "{rel} missing on B");
    }

    
    for rel in &paths[0..3] {
        tb.write_random(rel, 4096);
    }
    let deadline = std::time::Instant::now() + timeout_from_env();
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "no reconvergence after edits: A={:?} B={:?}",
            fx.a.stats(),
            fx.b.stats()
        );
        if fx.converged() && common::trees_identical(&fx.tree_a(), &fx.tree_b()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    assert_eq!(
        ferry_store::store::rebuild_index_calls(),
        0,
        "steady-state ingest ran a full index rebuild (T-15 violation)"
    );
}
