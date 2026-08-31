use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn ferry_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_ferry").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/ferry"),
        PathBuf::from,
    )
}

struct TestDevice {
    home: tempfile::TempDir,
    tree: PathBuf,
}

impl TestDevice {
    fn new(tag: &str) -> Self {
        let home = tempfile::tempdir().expect("home dir");
        let tree =
            std::env::temp_dir().join(format!("ferry-net-pairing-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&tree);
        fs::create_dir_all(&tree).unwrap();
        Self { home, tree }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(ferry_bin());
        c.args(args)
            .env("FERRY_HOME", self.home.path())
            .current_dir(&self.tree)
            .env("RUST_LOG", "");
        c
    }
}

struct ProcSharer(Child);

impl Drop for ProcSharer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn test_cross_process_network_rendezvous_pairing_and_mutual_key_wraps() {
    let dev_a = TestDevice::new("sharer");
    let dev_b = TestDevice::new("joiner");

    // 1. Device A initializes a folder
    let init_status = dev_a
        .command(&["init"])
        .status()
        .expect("ferry init should run");
    assert!(init_status.success(), "ferry init failed");

    // 2. Device A starts sharing in a child process with JSON output
    let mut share_child = dev_a
        .command(&["share", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("ferry share should spawn");

    let stdout = share_child.stdout.take().expect("capture share stdout");
    let reader = BufReader::new(stdout);
    let mut pairing_code = String::new();

    let deadline = Instant::now() + Duration::from_secs(10);
    for line in reader.lines() {
        if Instant::now() > deadline {
            break;
        }
        if let Ok(l) = line {
            if let Ok(doc) = serde_json::from_str::<serde_json::Value>(&l) {
                if let Some(code) = doc["code"].as_str() {
                    pairing_code = code.to_string();
                    break;
                }
            }
            if let Some(pos) = l.find("Share code:") {
                let code = l[pos + 11..].trim();
                let clean: String = code.chars().take_while(|c| c.is_alphanumeric() || *c == '-').collect();
                if clean.len() >= 6 {
                    pairing_code = clean;
                    break;
                }
            }
        }
    }

    assert!(!pairing_code.is_empty(), "Failed to get pairing code from share output");
    let _sharer_guard = ProcSharer(share_child);

    // 3. Device B joins in a separate process using the network rendezvous code
    let join_output = dev_b
        .command(&["join", &pairing_code, dev_b.tree.to_str().unwrap()])
        .output()
        .expect("ferry join should run");

    assert!(
        join_output.status.success(),
        "ferry join failed with: {}",
        String::from_utf8_lossy(&join_output.stderr)
    );

    // 4. Verify folder store was adopted on Device B
    assert!(dev_b.tree.join(".ferry").is_dir());
    assert!(dev_b.tree.join(".ferry/config").is_file());

    // 5. Verify both CONFIG_HEAD files contain mutual wraps
    let id_a = ferry_crypto::identity::load_or_create(&ferry_cli::home::identity_root(dev_a.home.path())).unwrap();
    let id_b = ferry_crypto::identity::load_or_create(&ferry_cli::home::identity_root(dev_b.home.path())).unwrap();

    let cfg_b_bytes = std::fs::read(dev_b.tree.join(".ferry/config")).unwrap();
    let head_b = ferry_crypto::config_head::parse_config_head(&cfg_b_bytes).unwrap();
    assert_eq!(head_b.entries.len(), 2, "Device B CONFIG_HEAD must contain 2 entries");
    let pubs_b: Vec<_> = head_b.entries.iter().map(|e| e.device_pub).collect();
    assert!(pubs_b.contains(id_a.public()), "Device B must contain Device A wrap");
    assert!(pubs_b.contains(id_b.public()), "Device B must contain Device B wrap");

    // Give Device A a moment to finish committing its CONFIG_HEAD update
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut a_has_mutual_wrap = false;
    while Instant::now() < deadline {
        if let Ok(cfg_a_bytes) = std::fs::read(dev_a.tree.join(".ferry/config")) {
            if let Ok(head_a) = ferry_crypto::config_head::parse_config_head(&cfg_a_bytes) {
                let pubs_a: Vec<_> = head_a.entries.iter().map(|e| e.device_pub).collect();
                if pubs_a.contains(id_a.public()) && pubs_a.contains(id_b.public()) {
                    a_has_mutual_wrap = true;
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(a_has_mutual_wrap, "Device A CONFIG_HEAD must contain Device B wrap");

    // 6. Verify subsequent synchronization / folder authorization on both sides
    let open_a = ferry_folder::folder::open_folder(&dev_a.tree, &id_a).expect("Device A can open");
    let open_b = ferry_folder::folder::open_folder(&dev_b.tree, &id_b).expect("Device B can open");
    assert_eq!(open_a.folder_id, open_b.folder_id);

    let fmk_a = ferry_folder::folder::unwrap_own_fmk(&open_a, &id_a).expect("unwrap A FMK");
    let fmk_b = ferry_folder::folder::unwrap_own_fmk(&open_b, &id_b).expect("unwrap B FMK");
    assert_eq!(&fmk_a[..], &fmk_b[..], "FMK must match across paired devices");

    // 7. Verify that trying to join again with the same consumed code is refused
    let dev_c = TestDevice::new("joiner2");
    let join2_output = dev_c
        .command(&["join", &pairing_code, dev_c.tree.to_str().unwrap()])
        .output()
        .expect("ferry join should run");
    assert!(!join2_output.status.success(), "Reusing a consumed code must fail");
}

#[test]
fn test_invalid_or_nonexistent_pairing_code_fails_cleanly() {
    let dev = TestDevice::new("lonely-joiner");
    let join_output = dev
        .command(&["join", "ABCDEF", dev.tree.to_str().unwrap()])
        .output()
        .expect("ferry join should run");
    assert!(!join_output.status.success(), "Nonexistent code must fail");
}
