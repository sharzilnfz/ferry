//! Shared harness for ferry-sync integration tests: two real engines over
//! loopback TCP in temp dirs, plus the corrupting-transport test hook and
//! tree-comparison helpers.

// Each test binary compiles this module separately; not every binary uses
// every helper.
#![allow(dead_code)]

pub mod corrupt;

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use ferry_store::chunker::generate_polynomial;
use ferry_sync::engine::device_identity_for_tag;
use ferry_sync::format::hex;
use ferry_sync::{EngineConfig, EngineHandle, SyncEngine};

/// Store for a test engine, opened through ferry-folder — the one module
/// that owns key unwrap and cipher choice. No test here names a cipher.
pub fn test_store(cfg: &EngineConfig) -> Arc<ferry_store::store::Store> {
    ferry_folder::open_or_create_test_store(&cfg.store_dir, &device_identity_for_tag(&cfg.tag))
        .expect("test folder store")
}

/// Build (but do not start) an engine over a ferry-folder-opened store.
pub fn engine(cfg: EngineConfig, transport: Arc<dyn ferry_sync::Transport>) -> SyncEngine {
    let store = test_store(&cfg);
    SyncEngine::with_store(cfg, transport, store).expect("engine init")
}

#[allow(unused_imports)]
pub use corrupt::CorruptingTransport;

/// N from the ticket: convergence budget. Default 30s, configurable.
pub fn timeout_from_env() -> Duration {
    let secs = std::env::var("FERRY_SYNC_TEST_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(30);
    Duration::from_secs(secs)
}

/// Transport selection for the WHOLE suite (T-009 seam payoff):
///
/// - unset / `tcp` → the M0 throwaway loopback TCP transport (default).
/// - `iroh` → both engines ride `ferry_iroh::IrohTransport` end to end.
///
/// Run the parity proof with:
///
/// ```text
/// FERRY_SYNC_E2E_TRANSPORT=iroh cargo test -p ferry-sync
/// ```
///
/// The scenarios, assertions, and engine code are byte-identical in both
/// modes; only this constructor differs. That is the seam doing its job.
pub fn default_transport() -> Arc<dyn ferry_sync::Transport> {
    match std::env::var("FERRY_SYNC_E2E_TRANSPORT").as_deref() {
        Ok("iroh") => {
            // Distinct fixed seeds per call would collide across fixture
            // nodes; derive per-instance randomness instead. Two calls =
            // two endpoints = two public keys, exactly like two machines.
            use rand::RngCore;
            let mut seed_a = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut seed_a);
            let t = ferry_iroh::IrohTransport::new(
                ferry_iroh::IrohConfig::builder().secret(seed_a).build(),
            )
            .expect("iroh transport builds");
            Arc::new(t)
        }
        _ => Arc::new(ferry_sync::TcpTransport),
    }
}

pub struct EngineFixture {
    pub _dir: tempfile::TempDir,
    listen_addr: std::net::SocketAddr,
    pub poly: u64,
    name: String,
    pub a: EngineHandle,
    pub b: EngineHandle,
}

/// Config hook passed to [`EngineFixture::start_with_cfg_a`].
type CfgHook = Box<dyn FnOnce(&mut EngineConfig)>;

impl EngineFixture {
    /// Node A listens; node B connects. Both poll fast for snappy tests.
    pub fn start(name: &str, seed: u64) -> Self {
        Self::start_inner(name, seed, None, None)
    }

    /// Like [`start`], but node B dials through the given transport (test
    /// hook point).
    pub fn start_with_transport_b(
        name: &str,
        seed: u64,
        transport_b: Arc<dyn ferry_sync::Transport>,
    ) -> Self {
        Self::start_inner(name, seed, Some(transport_b), None)
    }

    /// Like [`start`], but node A's config passes through `hook_cfg_a`
    /// before the engine builds (e.g. to set `pin_state_dir`, T-06).
    pub fn start_with_cfg_a(
        name: &str,
        seed: u64,
        hook_cfg_a: impl FnOnce(&mut EngineConfig) + 'static,
    ) -> Self {
        Self::start_inner(name, seed, None, Some(Box::new(hook_cfg_a)))
    }

    fn start_inner(
        name: &str,
        seed: u64,
        transport_b: Option<Arc<dyn ferry_sync::Transport>>,
        hook_cfg_a: Option<CfgHook>,
    ) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let poly = generate_polynomial(&mut StdRng::seed_from_u64(seed));

        fs::create_dir_all(dir.path().join("a/tree")).unwrap();
        fs::create_dir_all(dir.path().join("b/tree")).unwrap();

        let mut cfg_a = self_cfg(dir.path(), "a", format!("{name}-a"), poly);
        if let Some(hook) = hook_cfg_a {
            hook(&mut cfg_a);
        }
        cfg_a.bind_addr = Some("127.0.0.1:0".parse().unwrap());
        let mut cfg_b = self_cfg(dir.path(), "b", format!("{name}-b"), poly);

        let id_a = device_identity_for_tag(&cfg_a.tag);
        let id_b = device_identity_for_tag(&cfg_b.tag);

        let (store_a, fmk) =
            ferry_folder::folder::create_folder(&cfg_a.store_dir, &id_a, cfg_a.folder_id, poly)
                .expect("create folder a");
        ferry_folder::folder::save_settings(
            &cfg_a.store_dir,
            &ferry_folder::folder::Settings {
                format_version: ferry_folder::folder::SETTINGS_FORMAT_VERSION,
                folder_id: hex(&cfg_a.folder_id),
                honor_gitignore: false,
                presets: Vec::new(),
                overrides: Vec::new(),
            },
        )
        .unwrap();
        store_a.flush().unwrap();
        store_a.write_index_snapshot().unwrap();

        let store_b = ferry_folder::folder::adopt_folder(
            &cfg_b.store_dir,
            &id_b,
            cfg_b.folder_id,
            &fmk,
            poly,
        )
        .expect("adopt folder b");
        ferry_folder::folder::save_settings(
            &cfg_b.store_dir,
            &ferry_folder::folder::Settings {
                format_version: ferry_folder::folder::SETTINGS_FORMAT_VERSION,
                folder_id: hex(&cfg_b.folder_id),
                honor_gitignore: false,
                presets: Vec::new(),
                overrides: Vec::new(),
            },
        )
        .unwrap();
        store_b.flush().unwrap();
        store_b.write_index_snapshot().unwrap();

        let mut engine_a = SyncEngine::with_store(cfg_a, default_transport(), Arc::new(store_a))
            .expect("engine A");
        let addr = engine_a
            .listen_addr()
            .expect("A must report its bound port");
        // Trust policy is refuse-by-default; these fixtures test convergence,
        // not pairing, so both nodes opt into TOFU explicitly (ADR-0007).
        engine_a.set_peer_policy(ferry_sync::PeerPolicy::TrustOnFirstUse);
        let a = engine_a.start();

        cfg_b.connect_to = Some(addr);
        let tb = transport_b.unwrap_or_else(default_transport);
        let mut engine_b = SyncEngine::with_store(cfg_b, tb, Arc::new(store_b)).expect("engine B");
        engine_b.set_peer_policy(ferry_sync::PeerPolicy::TrustOnFirstUse);
        let b = engine_b.start();

        EngineFixture {
            _dir: dir,
            listen_addr: addr,
            poly,
            name: name.to_string(),
            a,
            b,
        }
    }

    fn b_config(&self) -> EngineConfig {
        let mut cfg = self_cfg(self._dir.path(), "b", format!("{}-b", self.name), self.poly);
        cfg.connect_to = Some(self.listen_addr);
        cfg
    }

    /// Stop the default B and start a fresh one through `transport`.
    pub fn replace_b(&mut self, transport: Arc<dyn ferry_sync::Transport>) -> EngineHandle {
        self.b.shutdown();
        let cfg = self.b_config();
        let store_b = test_store(&cfg);
        let mut engine_b = SyncEngine::with_store(cfg, transport, store_b).expect("engine B");
        engine_b.set_peer_policy(ferry_sync::PeerPolicy::TrustOnFirstUse);
        self.b = engine_b.start();
        self.b.clone()
    }

    pub fn tree_a(&self) -> PathBuf {
        self._dir.path().join("a/tree")
    }

    pub fn tree_b(&self) -> PathBuf {
        self._dir.path().join("b/tree")
    }

    /// Converged = same non-zero agreed manifest id on both sides AND equal
    /// current root trees matching the agreed manifest.
    pub fn converged(&self) -> bool {
        match (
            self.a.agreed_id(),
            self.b.agreed_id(),
            self.a.root_id(),
            self.b.root_id(),
            self.a.current_manifest_id(),
            self.b.current_manifest_id(),
        ) {
            (Some(x), Some(y), Some(rx), Some(ry), Some(mx), Some(my)) => {
                x == y && x != [0u8; 32] && rx == ry && x == mx && y == my
            }
            _ => false,
        }
    }
}

fn self_cfg(base: &Path, slot: &str, tag: String, poly: u64) -> EngineConfig {
    let mut cfg = EngineConfig::default_for_test(poly);
    cfg.tag = tag;
    cfg.store_dir = base.join(slot).join("store");
    cfg.tree_dir = base.join(slot).join("tree");
    cfg
}

/// Deterministic fixture tree writer used by several tests.
pub struct TreeBuilder {
    root: PathBuf,
    rng: StdRng,
}

impl TreeBuilder {
    pub fn new(root: impl Into<PathBuf>, seed: u64) -> Self {
        TreeBuilder {
            root: root.into(),
            rng: StdRng::seed_from_u64(seed),
        }
    }

    pub fn write(&mut self, rel: &str, bytes: &[u8]) {
        let p = self.root.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, bytes).unwrap();
    }

    pub fn write_exec(&mut self, rel: &str, bytes: &[u8]) {
        self.write(rel, bytes);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = self.root.join(rel);
            let mut perm = fs::metadata(&p).unwrap().permissions();
            perm.set_mode(0o755);
            fs::set_permissions(p, perm).unwrap();
        }
    }

    pub fn write_random(&mut self, rel: &str, max_len: usize) -> Vec<u8> {
        let len = self.rng.gen_range(0..=max_len);
        let bytes: Vec<u8> = (0..len).map(|_| self.rng.gen()).collect();
        self.write(rel, &bytes);
        bytes
    }

    pub fn remove(&self, rel: &str) {
        fs::remove_file(self.root.join(rel)).unwrap();
    }

    /// `count` files spread across nested dirs with random content.
    /// Returns rel paths written.
    pub fn create_random_files(&mut self, count: usize) -> Vec<String> {
        let mut out = Vec::new();
        for i in 0..count {
            let dir_depth = self.rng.gen_range(0..=2);
            let mut rel = String::new();
            for _ in 0..dir_depth {
                let _ = write!(rel, "d{}/", self.rng.gen_range(1..=3));
            }
            let _ = write!(rel, "file-{i:03}.bin");
            self.write_random(&rel, 8192);
            out.push(rel);
        }
        out
    }
}

/// Byte-for-byte equality of every file under both trees, relative paths
/// matched exactly. Extra files on either side are inequality.
pub fn trees_identical(a: &Path, b: &Path) -> bool {
    let ra = listing(a);
    let rb = listing(b);
    if ra.len() != rb.len() {
        eprintln!("file count differs: {} vs {}", ra.len(), rb.len());
        return false;
    }
    for (rel, ha) in &ra {
        match rb.get(rel) {
            Some(hb) if hb == ha => {}
            _ => {
                eprintln!("mismatch at {rel}: {ha} vs {:?}", rb.get(rel));
                return false;
            }
        }
    }
    true
}

fn listing(root: &Path) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn walk(base: &Path, dir: &Path, out: &mut std::collections::BTreeMap<String, String>) {
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk(base, &p, out);
        } else if p.is_file() {
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
            let digest = blake3_hash_file(&p);
            out.insert(rel, hex(digest.as_bytes()));
        }
    }
}

fn blake3_hash_file(p: &Path) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();
    let mut f = fs::File::open(p).unwrap();
    std::io::copy(&mut f, &mut hasher).unwrap();
    hasher.finalize()
}
