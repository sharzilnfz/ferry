mod common;

use std::time::Duration;

use common::{timeout_from_env, EngineFixture, TreeBuilder};

#[test]
fn empty_peer_hydrates_whole_tree_from_scratch() {
    let fx = EngineFixture::start("boot", 77);

    let mut tb = TreeBuilder::new(fx.tree_a(), 1234);
    for i in 0..25 {
        let rel = format!("pkg{}/mod{}/asset-{i:02}.bin", i % 5, i % 3);
        tb.write_random(&rel, 4096);
    }
    tb.write_exec("tools/go.sh", b"#!/bin/sh\nexit 0\n");
    tb.write("empty.marker", b"");
    let mut nested = TreeBuilder::new(fx.tree_a(), 99);
    let _ = &mut nested;

    let deadline = std::time::Instant::now() + timeout_from_env();
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "bootstrap did not finish in time: A={:?} B={:?}",
            fx.a.stats(),
            fx.b.stats()
        );
        if fx.converged()
            && count_files(&fx.tree_b()) == 27
            && common::trees_identical(&fx.tree_a(), &fx.tree_b())
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let files = count_files(&fx.tree_b());
    assert_eq!(files, 27, "expected all 27 fixtures hydrated, saw {files}");
    assert!(fx.tree_b().join("empty.marker").is_file());

    assert_eq!(fx.a.agreed_id(), fx.b.agreed_id());
}

fn count_files(root: &std::path::Path) -> usize {
    fn walk(dir: &std::path::Path) -> usize {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| {
                let p = e.path();
                if p.is_dir() {
                    walk(&p)
                } else {
                    usize::from(p.is_file())
                }
            })
            .sum()
    }
    walk(root)
}
