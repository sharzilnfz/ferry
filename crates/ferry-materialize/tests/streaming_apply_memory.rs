use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};

use ferry_materialize::Applier;
use ferry_store::chunker::{is_irreducible, Chunker};
use ferry_store::crypto::PassthroughCipher;
use ferry_store::manifest::{file_entry, serialize_tree_node, TreeNode};
use ferry_store::store::Store;
use ferry_store::BlobKind;

struct BigBlockAlloc;

const BIG_BLOCK_MIN: usize = 4 * 1024 * 1024;
static BIG_LIVE: AtomicUsize = AtomicUsize::new(0);
static BIG_PEAK: AtomicUsize = AtomicUsize::new(0);

fn big_track_add(size: usize) {
    let live = BIG_LIVE.fetch_add(size, Ordering::SeqCst) + size;
    BIG_PEAK.fetch_max(live, Ordering::SeqCst);
}

fn big_track_sub(size: usize) {
    BIG_LIVE.fetch_sub(size, Ordering::SeqCst);
}

unsafe impl std::alloc::GlobalAlloc for BigBlockAlloc {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        let ptr = unsafe { std::alloc::System.alloc(layout) };
        if !ptr.is_null() && layout.size() >= BIG_BLOCK_MIN {
            big_track_add(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        if layout.size() >= BIG_BLOCK_MIN {
            big_track_sub(layout.size());
        }

        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        if layout.size() >= BIG_BLOCK_MIN {
            big_track_sub(layout.size());
        }

        let p = unsafe { std::alloc::System.realloc(ptr, layout, new_size) };
        if !p.is_null() && new_size >= BIG_BLOCK_MIN {
            big_track_add(new_size);
        }
        p
    }
}

#[global_allocator]
static GLOBAL_ALLOC: BigBlockAlloc = BigBlockAlloc;

fn xorshift_fill(buf: &mut [u8], state: &mut u64) {
    for b in buf.iter_mut() {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *b = (*state >> 24) as u8;
    }
}

fn fmk() -> [u8; 32] {
    core::array::from_fn(|i| (i * 7 + 3) as u8)
}

fn first_valid_poly() -> u64 {
    let mut p = (1u64 << 53) | 1;
    while !is_irreducible(p) {
        p += 2;
    }
    p
}

#[test]
fn apply_of_32mib_file_keeps_peak_allocation_bounded() {
    const BLOCK: usize = 1024 * 1024;
    const TOTAL: usize = 33 * 1024 * 1024 + 65_536;
    const MT_A: (i64, u32) = (1_700_000_000, 111);

    let dir = tempfile::tempdir().unwrap();
    let store_root = dir.path().join("store-root");
    std::fs::create_dir_all(&store_root).unwrap();
    let store = Store::create(&store_root, fmk(), Box::new(PassthroughCipher)).unwrap();
    let target = dir.path().join("target");
    std::fs::create_dir_all(&target).unwrap();

    let poly = first_valid_poly();

    let mut chunker = Chunker::new(poly).unwrap();
    let mut block = vec![0u8; BLOCK];
    let mut cur: Vec<u8> = Vec::with_capacity(BLOCK * 2);
    let mut chunks: Vec<(_, u64)> = Vec::new();
    let mut state = 0x0123_4567_89AB_CDEFu64;
    let mut remaining = TOTAL;
    while remaining > 0 {
        let n = BLOCK.min(remaining);
        xorshift_fill(&mut block[..n], &mut state);
        let mut eaten = 0usize;
        for len in chunker.feed(&block[..n]) {
            let fresh = len - cur.len();
            cur.extend_from_slice(&block[eaten..eaten + fresh]);
            eaten += fresh;
            chunks.push((store.put_data(&cur).unwrap(), cur.len() as u64));
            cur.clear();
        }
        cur.extend_from_slice(&block[eaten..n]);
        remaining -= n;
    }
    let tail = chunker.finish();
    assert_eq!(tail, cur.len());
    if tail > 0 {
        chunks.push((store.put_data(&cur).unwrap(), tail as u64));
    }
    assert_eq!(chunks.iter().map(|c| c.1).sum::<u64>(), TOTAL as u64);

    store.flush().unwrap();
    let root_tree_id = store
        .put_meta(
            BlobKind::TreeNode,
            &serialize_tree_node(&TreeNode {
                entries: vec![file_entry("big.bin", false, MT_A.0, MT_A.1, chunks)],
            }),
        )
        .unwrap();
    store.flush().unwrap();

    BIG_PEAK.store(0, Ordering::SeqCst);
    Applier::new(&store, &target)
        .apply_tree(&root_tree_id)
        .unwrap();
    let peak = BIG_PEAK.load(Ordering::SeqCst);

    const LIMIT: usize = 30 * 1024 * 1024;
    println!(
        "T-09 memory gate: file {TOTAL} bytes, peak big-block allocation during apply {peak} \
         bytes (limit {LIMIT})"
    );
    assert!(
        peak <= LIMIT,
        "peak big-block allocation during apply was {peak} bytes \
         (limit {LIMIT}, file {TOTAL}); this smells like whole-file buffering"
    );

    let mut written = std::fs::File::open(target.join("big.bin")).unwrap();
    let mut expect = vec![0u8; BLOCK];
    let mut got = vec![0u8; BLOCK];
    let mut state = 0x0123_4567_89AB_CDEFu64;
    let mut left = TOTAL;
    while left > 0 {
        let n = BLOCK.min(left);
        xorshift_fill(&mut expect[..n], &mut state);
        written.read_exact(&mut got[..n]).unwrap();
        assert_eq!(
            &got[..n],
            &expect[..n],
            "content diverged at {}",
            TOTAL - left
        );
        left -= n;
    }
    assert_eq!(
        written.read(&mut got).unwrap(),
        0,
        "file must end exactly at TOTAL"
    );
    let md = std::fs::symlink_metadata(target.join("big.bin")).unwrap();
    assert_eq!(md.len() as usize, TOTAL);
}
