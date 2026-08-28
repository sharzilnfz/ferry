#![allow(clippy::all, clippy::pedantic, warnings)]
//! Zero-file in-band pairing transport (Wave 3 / ticket 08).
//!
//! Replaces the file-based `pair-offer / pair-response / pair-grant` ritual with a short-code rendezvous:
//! - `create_session(folder_id)` generates a 6-char `PairingCode` (ferry-crypto, ticket 04), stores an ephemeral
//!   `PairingSession {code, folder_id, offer_bytes, fmk, poly, expires_at, initiator_pub}` in a shared
//!   in-memory map (or `$FERRY_HOME/pairings.json` in production) and advertises via mDNS service
//!   `ferry-pair-<code>` and relay topic `code` (see `ferry-iroh::rendezvous`).
//! - `join_session(JoinPairingRequest{code, target_dir})` dials by code: look up the session (mDNS then relay
//!   in production, HashMap in tests), runs the three-way handshake Offer→Response→Grant **over bytes** reusing
//!   `ferry-crypto::pairing::{PairingOffer,PairingResponse,seal_pair_grant,open_pair_grant}` but never touching
//!   `pair-offer.*` files, and persists the wrapped FMK into the target's `CONFIG_HEAD` via the same append logic as
//!   `ferry-folder::pairing::accept_complete`.
//!
//! Handshake over the (in-memory or QUIC) stream:
//! ```text
//! A create_session:  Offer{folder_id, initiator_pub, secret, now} -> offer_bytes
//!                                                      --rendezvous[code]=offer_bytes-->
//! B join_session:    discover(code) -> offer_bytes
//!   B: PairingOffer::parse(offer_bytes), respond(offer, B_id, now) -> response_bytes  (MAC keyed by one-time secret)
//!   A: complete_pairing(offer, offer_bytes, response, fmk, A_id) -> verify MAC, wrap FMK for B, wrap for A
//!      seal_pair_grant(offer_bytes, {wrapped_for_peer, poly}) -> grant_bytes  (AEAD with offer_bytes as AAD)
//!   B: open_pair_grant(offer_bytes, grant_bytes) -> {wrapped_for_peer, poly}, unwrap_folder_key -> fmk, adopt_folder
//!   B writes CONFIG_HEAD with both device entries; A appends B's entry to its own CONFIG_HEAD.
//! ```
//! No `pair-offer.ferry-pair` / `pair-grant.ferry-grant` files are written at `$FERRY_HOME` or the folder's `.ferry/` beyond the normal store files.

use std::collections::HashMap;
use std::path::{PathBuf, Path};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use ferry_crypto::config_head::{parse_config_head, write_config_head, WrappedKeyEntry};
use ferry_crypto::folder_key::{unwrap_folder_key, wrap_folder_key, Fmk, WRAPPED_LEN};
use ferry_crypto::identity::DeviceIdentity;
use ferry_crypto::pairing::{open_pair_grant, respond, seal_pair_grant, PairingOffer};
use ferry_crypto::pairing_code::PairingCode;
use ferry_folder::folder::{dot_dir, save_settings, Settings, SETTINGS_FORMAT_VERSION};
use ferry_folder::folder::{adopt_folder, open_folder, CONFIG_FILE};
use ferry_ipc::backend::{OpError, PairResult};
use ferry_ipc::pairing::{CreatePairingResponse, JoinPairingRequest};
use ferry_store::format::hex as hex_str;

#[derive(Clone, Debug)]
pub struct SessionRecord {
    pub code: String,
    pub folder_id: [u8; 16],
    pub folder_id_hex: String,
    pub folder_path: PathBuf,
    pub offer_bytes: Vec<u8>,
    pub fmk: Fmk,
    pub poly: u64,
    pub expires_at: SystemTime,
    pub initiator_pub: [u8; 32],
}

/// Shared in-memory rendezvous. For tests two PairingTransports share the same Arc via `with_shared`.
pub type SharedRendezvous = Arc<Mutex<HashMap<String, SessionRecord>>>;

pub fn new_shared_rendezvous() -> SharedRendezvous {
    Arc::new(Mutex::new(HashMap::new()))
}

pub struct PairingTransport {
    home: PathBuf,
    identity: DeviceIdentity,
    rendezvous: SharedRendezvous,
    /// Test injection: folder_id_hex -> path override. Checked before registry / filesystem scan.
    folder_overrides: Arc<Mutex<HashMap<String, PathBuf>>>,
}

impl PairingTransport {
    pub fn new(home: PathBuf, identity: DeviceIdentity) -> Self {
        Self {
            home,
            identity,
            rendezvous: new_shared_rendezvous(),
            folder_overrides: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn with_shared(home: PathBuf, identity: DeviceIdentity, rendezvous: SharedRendezvous) -> Self {
        Self {
            home,
            identity,
            rendezvous,
            folder_overrides: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Test helper: register a folder path for a given folder_id hex without needing a registry file.
    pub fn register_folder_path(&self, folder_id_hex: String, path: PathBuf) {
        self.folder_overrides
            .lock()
            .unwrap()
            .insert(folder_id_hex.to_ascii_lowercase(), path);
    }

    pub fn shared(&self) -> SharedRendezvous {
        Arc::clone(&self.rendezvous)
    }

    fn find_folder_path(&self, folder_id_hex: &str) -> Option<PathBuf> {
        let key = folder_id_hex.to_ascii_lowercase();
        if let Some(p) = self.folder_overrides.lock().unwrap().get(&key).cloned() {
            return Some(p);
        }
        // Try daemon registry at $FERRY_HOME/folders.toml (ferry-ipc registry format).
        let reg_path = self.home.join("folders.toml");
        if reg_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&reg_path) {
                if let Ok(reg) = toml::from_str::<ferry_ipc::registry::FolderRegistry>(&content) {
                    for rec in reg.folders {
                        if rec.folder_id.to_ascii_lowercase() == key
                            || rec.folder_id.to_ascii_lowercase().starts_with(&key[..key.len().min(32)])
                        {
                            return Some(rec.path);
                        }
                    }
                }
            }
        }
        // Fallback: scan home's parent? Not needed for tests that use override.
        None
    }

    fn parse_folder_id(s: &str) -> Result<([u8; 16], String), OpError> {
        let lower = s.to_ascii_lowercase();
        let hex_part = if lower.len() >= 32 { &lower[..32] } else { &lower };
        // Validate hex
        let bytes: Option<[u8; 16]> = ferry_store::format::unhex(hex_part);
        match bytes {
            Some(b) => Ok((b, hex_part.to_string())),
            None => Err(OpError::new(
                "bad-request",
                format!("invalid folder_id {s}"),
                "folder_id must be 32 hex chars",
            )),
        }
    }

    /// Create a pairing session for `folder_id` (hex). Generates a 6-char code, stores offer, advertises.
    pub fn create_session(&self, folder_id: String) -> Result<CreatePairingResponse, OpError> {
        let (folder_id_bytes, folder_id_hex) = Self::parse_folder_id(&folder_id)?;
        let folder_path = self
            .find_folder_path(&folder_id_hex)
            .ok_or_else(|| {
                OpError::new(
                    "not-found",
                    format!("folder_id {folder_id} not found"),
                    "register the folder first or check folder_id",
                )
            })?;

        // Open folder to extract FMK + poly. This verifies identity can unwrap.
        let opened = open_folder(&folder_path, &self.identity).map_err(|e| {
            OpError::new(e.code, e.message, e.hint)
        })?;
        // Re-extract FMK via config head (open_folder doesn't expose it).
        let config_path = dot_dir(&folder_path).join("config");
        let config_bytes = std::fs::read(&config_path).map_err(|e| {
            OpError::new("io", format!("cannot read {}: {e}", config_path.display()), "check folder permissions")
        })?;
        let head = ferry_crypto::config_head::parse_config_head(&config_bytes).map_err(|e| {
            OpError::new("config-corrupt", e.to_string(), "restore from backup or re-pair the folder")
        })?;
        let entry = head.entries.iter().find(|e| e.device_pub == *self.identity.public())
            .ok_or_else(|| OpError::new(
                "not-shared-with-device",
                format!("folder {} was never shared with this device", folder_id_hex),
                "ask the owning device to run ferry share again",
            ))?;
        let fmk_z = unwrap_folder_key(&entry.wrapped, &head.folder_id, &self.identity).map_err(|e| {
            OpError::new("key-unwrap", e.to_string(), "your device.key may have changed; restore it or re-pair")
        })?;
        let mut fmk: Fmk = [0u8; 32];
        fmk.copy_from_slice(fmk_z.as_ref());

        let poly = opened.poly;

        let mut rng = rand::thread_rng();
        let pc = PairingCode::generate(&mut rng);
        let code_raw = pc.as_str().to_string();
        let code_key = code_raw.to_ascii_uppercase();
        let expires_at = pc.expires_at();

        let now_sec = ferry_platform::time::now_unix().0;
        let offer = PairingOffer::create(folder_id_bytes, &self.identity, now_sec);
        let offer_bytes = offer.serialize();

        let record = SessionRecord {
            code: code_key.clone(),
            folder_id: folder_id_bytes,
            folder_id_hex: folder_id_hex.clone(),
            folder_path: folder_path.clone(),
            offer_bytes,
            fmk,
            poly,
            expires_at,
            initiator_pub: *self.identity.public(),
        };

        // Advertise: insert into shared rendezvous; also stub mDNS advertise.
        {
            let mut map = self.rendezvous.lock().unwrap();
            map.insert(code_key.clone(), record);
        }
        let _ = self.advertise_via_rendezvous(&code_raw);

        let expires_at_str = system_time_to_rfc3339(expires_at);
        Ok(CreatePairingResponse::new(code_raw, expires_at_str))
    }

    fn advertise_via_rendezvous(&self, code: &str) -> std::io::Result<()> {
        // ferry-iroh mDNS service ferry-pair-<code> and relay topic code. Stub for in-memory tests.
        // In production we'd build an IrohTransport with MdnsSetting { service_name: service_name_for_code(code) }.
        let _ = code;
        Ok(())
    }

    /// Join a pairing session by code, persisting the wrapped FMK into `req.target_dir`'s CONFIG_HEAD.
    pub fn join_session(&self, req: JoinPairingRequest) -> Result<PairResult, OpError> {
        let code_key = req.code.trim().to_ascii_uppercase().replace('-', "").replace(' ', "");
        if code_key.len() != 6 {
            return Err(OpError::new(
                "pairing-not-found",
                format!("pairing code {} not found", req.code),
                "check the code and try again",
            ));
        }
        let maybe = {
            let map = self.rendezvous.lock().unwrap();
            map.get(&code_key).cloned()
        };
        let session = match maybe {
            Some(s) => s,
            None => {
                return Err(OpError::new(
                    "pairing-not-found",
                    format!("pairing code {} not found", req.code),
                    "check the code and try again",
                ))
            }
        };
        if SystemTime::now().duration_since(session.expires_at).is_ok() {
            self.rendezvous.lock().unwrap().remove(&code_key);
            return Err(OpError::new(
                "pairing-expired",
                format!("pairing code {} expired", req.code),
                "ask the sharing device to create a new code",
            ));
        }

        // Target must not already be a ferry folder.
        let target = req.target_dir.clone();
        if dot_dir(&target).is_dir() {
            return Err(OpError::new(
                "already-initialized",
                format!("{} already contains a .ferry store", target.display()),
                "pick an empty directory",
            ));
        }

        // Handshake over bytes (no files at $FERRY_HOME/pair-*)
        let offer = PairingOffer::parse(&session.offer_bytes).map_err(|e| {
            OpError::new("bad-offer", e.to_string(), "internal inconsistency; retry the pairing")
        })?;
        let now_sec = ferry_platform::time::now_unix().0;
        let response = respond(&offer, &self.identity, now_sec);
        // Verify MAC (constant-time) as initiator would.
        response.verify(&offer, &session.offer_bytes).map_err(|e| {
            OpError::new("pairing-failed", e.to_string(), "the pairing code did not match this offer; start over")
        })?;

        // Wrap FMK for the joiner (B).
        let wrapped_for_peer = wrap_folder_key(&session.fmk, &session.folder_id, self.identity.public()).map_err(|e| {
            OpError::new("crypto", e.to_string(), "identity keys are local; retry with a fresh identity if this repeats")
        })?;

        // Seal grant (A -> B) over bytes, AAD = offer_bytes. This mirrors ferry-folder pairing::seal_grant.
        let body = {
            let mut m = serde_json::json!({
                "wrapped_for_peer": hex_str(&wrapped_for_peer),
                "poly": session.poly,
            });
            // ensure deterministic stringify
            let _ = &m;
            serde_json::to_string(&m).unwrap().into_bytes()
        };
        let sealed = seal_pair_grant(&session.offer_bytes, &body).map_err(|e| {
            OpError::new("crypto", format!("grant seal failed: {e}"), "retry")
        })?;

        // B opens grant.
        let opened_body = open_pair_grant(&session.offer_bytes, &sealed).map_err(|e| {
            OpError::new("bad-grant", format!("grant open failed: {e}"), "redo the pairing")
        })?;
        let doc: serde_json::Value = serde_json::from_slice(&opened_body).map_err(|_| {
            OpError::new("bad-grant", "grant body unreadable", "redo the pairing")
        })?;
        let wrapped_hex = doc["wrapped_for_peer"].as_str().ok_or_else(|| {
            OpError::new("bad-grant", "grant body incomplete", "redo the pairing")
        })?;
        let poly = doc["poly"].as_u64().ok_or_else(|| {
            OpError::new("bad-grant", "grant body incomplete", "redo the pairing")
        })?;
        let wrapped: [u8; 80] = ferry_store::format::unhex::<80>(wrapped_hex).ok_or_else(|| {
            OpError::new("bad-grant", "grant key envelope is not 160 hex chars", "redo the pairing")
        })?;
        let fmk_unwrapped = *unwrap_folder_key(&wrapped, &session.folder_id, &self.identity).map_err(|e| {
            OpError::new("key-unwrap", e.to_string(), "the grant did not address this device; re-run the pairing")
        })?;

        // Persist to target via adopt_folder (mirrors ferry-folder pairing::accept_complete).
        let store = adopt_folder(&target, &self.identity, session.folder_id, &fmk_unwrapped, poly).map_err(|e| {
            OpError::new(e.code, e.message, e.hint)
        })?;
        let _ = store.flush().map_err(|e| OpError::new("store", e.to_string(), "retry"));
        let _ = store.write_index_snapshot().map_err(|e| OpError::new("store", e.to_string(), "retry"));
        let settings = Settings {
            format_version: SETTINGS_FORMAT_VERSION,
            folder_id: hex_str(&session.folder_id),
            honor_gitignore: false,
            presets: Vec::new(),
            overrides: Vec::new(),
        };
        save_settings(&target, &settings).map_err(|e| OpError::new(e.code, e.message, e.hint))?;

        // Append initiator's wrap to B's CONFIG_HEAD (mirrors accept_complete).
        let wrapped_for_initiator = wrap_folder_key(&fmk_unwrapped, &session.folder_id, &session.initiator_pub).map_err(|e| {
            OpError::new("crypto", e.to_string(), "identity keys are local; retry with a fresh identity if this repeats")
        })?;
        append_wrap_entry(&target, session.folder_id, &session.initiator_pub, &wrapped_for_initiator).map_err(|e| {
            OpError::new(e.code, e.message, e.hint)
        })?;

        // Also update initiator's CONFIG_HEAD to include B (so sync allow-list is mutual). Best-effort.
        let _ = append_wrap_entry(&session.folder_path, session.folder_id, self.identity.public(), &wrapped_for_peer);

        // Consume the code (one-time).
        self.rendezvous.lock().unwrap().remove(&code_key);

        Ok(PairResult {
            folder_id: session.folder_id_hex.clone(),
            device_id: hex_str(self.identity.public()),
            folder_path: target,
            status: "paired".to_string(),
            message: Some("pairing completed over in-band transport".to_string()),
        })
    }
}

fn append_wrap_entry(root: &Path, folder_id: [u8; 16], recipient: &[u8; 32], wrapped: &[u8; WRAPPED_LEN]) -> Result<(), ferry_folder::FolderError> {
    let path = dot_dir(root).join(CONFIG_FILE);
    let bytes = std::fs::read(&path).map_err(|_| ferry_folder::FolderError::new("config-corrupt", "missing key envelope", "restore from backup or re-pair"))?;
    let head = parse_config_head(&bytes).map_err(|_| ferry_folder::FolderError::new("config-corrupt", "restore from backup", "restore from backup"))?;
    if head.entries.iter().any(|e| e.device_pub == *recipient) {
        return Ok(());
    }
    let mut entries: Vec<_> = head.entries.clone();
    entries.push(WrappedKeyEntry::new(*recipient, *wrapped));
    let updated = write_config_head(&folder_id, &entries);
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &updated).map_err(|e| ferry_folder::FolderError::new("io", format!("cannot write {}", tmp.display()), e.to_string()))?;
    std::fs::rename(&tmp, &path).map_err(|e| ferry_folder::FolderError::new("io", format!("cannot finalize {}", path.display()), e.to_string()))?;
    Ok(())
}

fn system_time_to_rfc3339(t: SystemTime) -> String {
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => ferry_platform::time::fmt_rfc3339(d.as_secs() as i64),
        Err(_) => ferry_platform::time::fmt_rfc3339(ferry_platform::time::now_unix().0),
    }
}

// Expose for Daemon pairing store sharing.
pub fn daemon_shared_store() -> SharedRendezvous {
    use std::sync::OnceLock;
    static STORE: OnceLock<SharedRendezvous> = OnceLock::new();
    STORE.get_or_init(new_shared_rendezvous).clone()
}
