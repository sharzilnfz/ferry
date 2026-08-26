use ferry_store::agreement::AgreementLedger;
use ferry_store::crypto::PassthroughCipher;
use ferry_store::format::{hex, BlobKind};
use ferry_store::manifest::parse_manifest;
use ferry_store::store::Store;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let store_dir = std::path::Path::new(&args[1]);
    let folder_id = args[2].clone();
    let store = Store::open(store_dir, [0u8; 32], Box::new(PassthroughCipher)).unwrap();
    let records = AgreementLedger::new(store_dir.join(".ferry"))
        .list_folder(&hex_to_16(&folder_id))
        .unwrap();
    println!("records: {}", records.len());
    let root = store
        .index_entries()
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == BlobKind::Manifest)
        .count() as u32;
    let _ = root;
    for (dev, rec) in &records {
        println!(
            "rec dev={} man={} sec={}",
            hex(dev),
            hex(&rec.manifest_id),
            rec.agreed_sec
        );
        match store.get(BlobKind::Manifest, &rec.manifest_id) {
            Ok(bytes) => match parse_manifest(&bytes) {
                Ok(m) => {
                    let cur_root = current_pointer_root(store_dir);
                    println!(
                        "   agreed manifest root={} matches_current={}",
                        hex(&m.root_tree_id),
                        m.root_tree_id == cur_root
                    );
                }
                Err(e) => println!("   PARSE ERR {e}"),
            },
            Err(e) => println!("   GET ERR {e}"),
        }
    }
}

fn current_pointer_root(_store_dir: &std::path::Path) -> [u8; 32] {
    *b"\x58\xd0\x0c\x6f\x63\xec\x5b\xea\x0f\x32\xa8\x9d\xb3\xa3\x1d\xbc\xe4\xb5\x56\xf5\xa5\xc9\x33\x26\xc8\xab\x7d\x03\x8d\x69\x17\x4a"
}

fn hex_to_16(s: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
    }
    out
}
