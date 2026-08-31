use std::path::PathBuf;

use ferry_materialize::Applier;
use ferry_store::crypto::PassthroughCipher;
use ferry_store::format::{unhex, BlobId};
use ferry_store::store::Store;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut store_dir: Option<PathBuf> = None;
    let mut target_dir: Option<PathBuf> = None;
    let mut tree_hex: Option<String> = None;
    let mut fmk_hex: Option<String> = None;
    let mut delay_ms: u64 = 0;

    while let Some(arg) = args.next() {
        let mut val = || args.next().expect("missing value after {arg}");
        match arg.as_str() {
            "--store" => store_dir = Some(PathBuf::from(val())),
            "--target" => target_dir = Some(PathBuf::from(val())),
            "--tree" => tree_hex = Some(val()),
            "--fmk-hex" => fmk_hex = Some(val()),
            "--delay-ms" => delay_ms = val().parse().expect("delay-ms must be a number"),
            other => panic!("unknown arg {other}"),
        }
    }

    let store_dir = store_dir.expect("--store required");
    let target = target_dir.expect("--target required");
    let tree_hex = tree_hex.expect("--tree required");
    let fmk_hex = fmk_hex.expect("--fmk-hex required");

    let tree: BlobId = unhex(&tree_hex).expect("tree id must be 64 hex chars");
    let fmk: [u8; 32] = unhex(&fmk_hex).expect("fmk must be 64 hex chars");

    let store =
        Store::open(&store_dir, fmk, Box::new(PassthroughCipher)).expect("store open failed");

    println!("applying tree {} with pace {delay_ms}ms", hex_of(&tree));
    std::io::Write::flush(&mut std::io::stdout()).unwrap();

    let stats = Applier::new(&store, &target)
        .pace_ms(delay_ms)
        .apply_tree(&tree)
        .expect("apply failed");
    println!(
        "done: {} mutations ({files} files, {dirs} dirs, {sym} symlinks, {unl} unlinked)",
        stats.mutations(),
        files = stats.files_written,
        dirs = stats.dirs_created,
        sym = stats.symlinks_written,
        unl = stats.unlinked,
    );
}

fn hex_of(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}
