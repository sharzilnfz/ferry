use ferry_store::crypto::PassthroughCipher;
use ferry_store::format::{hex, BlobKind};
use ferry_store::manifest::parse_manifest;
use ferry_store::store::Store;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let store_dir = &args[1];
    let want = args.get(2);
    let store =
        Store::open(std::path::Path::new(store_dir), [0u8; 32], Box::new(PassthroughCipher)).unwrap();
    for e in store.index_entries().unwrap() {
        if e.kind != BlobKind::Manifest {
            continue;
        }
        if let Some(w) = want {
            if hex(&e.id) != *w {
                continue;
            }
        }
        let shown = hex(&e.id);
        match store.get(BlobKind::Manifest, &e.id) {
            Ok(bytes) => match parse_manifest(&bytes) {
                Ok(m) => println!(
                    "{shown} root={} dev={} sec={} parent={}",
                    hex(&m.root_tree_id),
                    hex(&m.device_id),
                    m.created_sec,
                    hex(&m.parent_manifest_id),
                ),
                Err(err) => println!("{shown} PARSE ERR {err}"),
            },
            Err(err) => println!("{shown} GET ERR {err}"),
        }
    }
}
