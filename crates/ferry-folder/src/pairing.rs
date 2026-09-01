use std::collections::HashMap;
use std::io::{self};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ferry_crypto::base32::ALPHABET;
use ferry_crypto::crc32::crc32;
use ferry_crypto::folder_key::{unwrap_folder_key, wrap_folder_key, Fmk, WRAPPED_LEN};
use ferry_crypto::identity::{DeviceId, DeviceIdentity};
use ferry_crypto::pairing::{
    complete_pairing, open_pair_grant, respond, seal_pair_grant, GrantError, PairingOffer,
    PairingResponse, OFFER_LEN,
};
use rand::Rng;
use serde_json::json;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use crate::error::{CodeInto, FolderError, FolderResult};
use crate::folder::{
    adopt_folder, append_wrap_entry_for, dot_dir, save_settings, unwrap_own_fmk, OpenFolder,
    Settings, SETTINGS_FORMAT_VERSION,
};
use crate::inventory::FolderInventory;

pub const OFFER_SUFFIX: &str = "pair-offer.ferry-pair";
pub const RESPONSE_SUFFIX: &str = "pair-response.ferry-pair";
pub const GRANT_SUFFIX: &str = "pair-grant.ferry-grant";

pub const PAYLOAD_PREFIX: &str = "FERRY1";

#[derive(Clone)]
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
    pub server_stop: Option<Arc<AtomicBool>>,
}

impl std::fmt::Debug for SessionRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionRecord")
            .field("code", &self.code)
            .field("folder_id_hex", &self.folder_id_hex)
            .field("folder_path", &self.folder_path)
            .field("poly", &self.poly)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

pub type SharedRendezvous = Arc<Mutex<HashMap<String, SessionRecord>>>;

#[must_use]
pub fn new_shared_rendezvous() -> SharedRendezvous {
    Arc::new(Mutex::new(HashMap::new()))
}

#[must_use]
pub fn shared_rendezvous() -> SharedRendezvous {
    static STORE: OnceLock<SharedRendezvous> = OnceLock::new();
    STORE.get_or_init(new_shared_rendezvous).clone()
}

fn code_key(code: &str) -> String {
    code.trim()
        .chars()
        .filter(|c| *c != '-' && *c != ' ')
        .collect::<String>()
        .to_ascii_uppercase()
}

fn encode_envelope(code: &str, offer_bytes: &[u8], expires_at: SystemTime) -> String {
    let secs = expires_at
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    format!(
        "{PAYLOAD_PREFIX}:{}:{}:{secs}",
        code_key(code),
        ferry_store::format::hex(offer_bytes)
    )
}

#[derive(Clone, Debug)]
pub struct PayloadEnvelope {
    pub code: String,
    pub offer_bytes: Vec<u8>,
    pub expires_at: SystemTime,
}

#[must_use]
pub fn parse_payload_envelope(text: &str) -> Option<PayloadEnvelope> {
    let rest = text
        .trim()
        .strip_prefix(PAYLOAD_PREFIX)?
        .strip_prefix(':')?;
    let mut parts = rest.split(':');
    let code = code_key(parts.next()?);
    let offer_hex = parts.next()?;
    let secs = parts.next()?.parse::<u64>().ok()?;
    if code.len() != 6
        || !code
            .chars()
            .all(|c| ferry_crypto::base32::ALPHABET.contains(&(c as u8)))
    {
        return None;
    }
    let offer_bytes = ferry_store::format::unhex::<OFFER_LEN>(offer_hex)?.to_vec();
    Some(PayloadEnvelope {
        code,
        offer_bytes,
        expires_at: UNIX_EPOCH + Duration::from_secs(secs),
    })
}

const CODE_EXPIRY: Duration = Duration::from_hours(24);

struct PairingCode {
    code: Zeroizing<String>,
    expires_at: SystemTime,
}

impl std::fmt::Debug for PairingCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingCode")
            .field("code", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl PairingCode {
    fn generate<R: Rng>(rng: &mut R) -> Self {
        let mut chars = Vec::with_capacity(6);
        for _ in 0..5 {
            let idx = (rng.gen::<u32>() & 31) as usize;
            chars.push(ALPHABET[idx] as char);
        }
        let data_str: String = chars.iter().collect();
        let crc = crc32(data_str.as_bytes());
        let checksum_idx = (crc % 32) as usize;
        chars.push(ALPHABET[checksum_idx] as char);
        let code_string: String = chars.into_iter().collect();
        let expires_at = SystemTime::now() + CODE_EXPIRY;
        PairingCode {
            code: Zeroizing::new(code_string),
            expires_at,
        }
    }

    fn as_str(&self) -> &str {
        self.code.as_str()
    }

    fn expires_at(&self) -> SystemTime {
        self.expires_at
    }

    fn verify(input: &str) -> bool {
        let bytes = code_key(input).into_bytes();
        if bytes.len() != 6 || !bytes.iter().all(|&b| ALPHABET.contains(&b)) {
            return false;
        }
        let expected = ALPHABET[(crc32(&bytes[0..5]) % 32) as usize];
        bool::from(expected.ct_eq(&bytes[5]))
    }
}

fn expired(expires_at: SystemTime, now: SystemTime) -> bool {
    now.duration_since(expires_at).is_ok()
}

pub struct PairingRitual {
    home: PathBuf,
    identity: DeviceIdentity,
    rendezvous: SharedRendezvous,

    folder_overrides: Arc<Mutex<HashMap<String, PathBuf>>>,
}

impl PairingRitual {
    #[must_use]
    pub fn new(home: PathBuf, identity: DeviceIdentity) -> Self {
        Self::with_shared(home, identity, shared_rendezvous())
    }

    #[must_use]
    pub fn with_shared(
        home: PathBuf,
        identity: DeviceIdentity,
        rendezvous: SharedRendezvous,
    ) -> Self {
        Self {
            home,
            identity,
            rendezvous,
            folder_overrides: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn register_folder_path(&self, folder_id_hex: String, path: PathBuf) {
        self.folder_overrides
            .lock()
            .expect("folder overrides")
            .insert(folder_id_hex.to_ascii_lowercase(), path);
    }

    pub fn create_offer(&self, opened: &OpenFolder) -> FolderResult<PendingOffer> {
        let dot = dot_dir(&opened.root);
        std::fs::create_dir_all(&dot).code("io", "check folder permissions")?;
        let fmk = unwrap_own_fmk(opened, &self.identity)?;

        let now = ferry_platform::time::now_unix().0;
        let offer = PairingOffer::create(opened.folder_id, &self.identity, now);
        let offer_bytes = offer.serialize();

        let mut rng = rand::thread_rng();
        let pc = PairingCode::generate(&mut rng);
        let code = pc.as_str().to_string();
        let expires_at = pc.expires_at();

        // Launch network rendezvous server for this pairing code
        let identity_clone = self.identity.clone();
        let record_folder_path = opened.root.clone();
        let record_folder_id = opened.folder_id;
        let record_fmk = fmk;
        let record_poly = opened.poly;
        let record_code = code_key(&code);
        let rendezvous_clone = Arc::clone(&self.rendezvous);
        let offer_bytes_for_server = offer_bytes.clone();

        let server_stop = start_pairing_server(
            code.clone(),
            offer_bytes.clone(),
            expires_at,
            move |resp_bytes| {
                let offer_parsed = PairingOffer::parse(&offer_bytes_for_server).map_err(|e| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
                })?;
                let response = PairingResponse::parse(&resp_bytes).map_err(|e| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
                })?;
                let done = complete_pairing(
                    &offer_parsed,
                    &offer_bytes_for_server,
                    &response,
                    &record_fmk,
                    &identity_clone,
                )
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

                let _ = append_wrap_entry_for(
                    &record_folder_path,
                    record_folder_id,
                    &done.peer_pub,
                    &done.wrapped_for_peer,
                );

                let grant_bytes =
                    seal_grant(&offer_bytes_for_server, &done.wrapped_for_peer, record_poly)
                        .map_err(|e| std::io::Error::other(e.to_string()))?;

                if let Ok(mut map) = rendezvous_clone.lock() {
                    map.remove(&record_code);
                }

                Ok(grant_bytes)
            },
        )
        .ok();

        let record = SessionRecord {
            code: code_key(&code),
            folder_id: opened.folder_id,
            folder_id_hex: ferry_store::format::hex(&opened.folder_id),
            folder_path: opened.root.clone(),
            offer_bytes: offer_bytes.clone(),
            fmk,
            poly: opened.poly,
            expires_at,
            initiator_pub: *self.identity.public(),
            server_stop,
        };
        self.rendezvous
            .lock()
            .expect("rendezvous map")
            .insert(record.code.clone(), record.clone());

        let payload = encode_envelope(&code, &offer_bytes, expires_at);
        Ok(PendingOffer {
            short_code: code,
            payload: payload.into_bytes(),
            payload_path: artifact(&dot, OFFER_SUFFIX),
            expires_at,
        })
    }

    /// Creates an offer given only an opened folder's 32-hex `folder_id`.
    /// Resolves the folder path from overrides or `FolderInventory`.
    pub fn create_offer_for_folder(&self, folder_id_hex: &str) -> FolderResult<PendingOffer> {
        let hex = folder_id_hex.trim().to_ascii_lowercase();
        if ferry_store::format::unhex::<16>(&hex).is_none() {
            return Err(FolderError::new(
                "bad-request",
                format!("invalid folder_id {folder_id_hex}"),
                "folder_id must be 32 hex chars",
            ));
        }
        let folder_path = self.find_folder_path(&hex).ok_or_else(|| {
            FolderError::new(
                "not-found",
                format!("folder_id {folder_id_hex} not found"),
                "register the folder first or check folder_id",
            )
        })?;
        let opened = crate::folder::open_folder(&folder_path, &self.identity)?;
        self.create_offer(&opened)
    }

    /// Single poll step for an active offer; returns `Ok(Some(completed))`
    /// if the other device has answered, or `Ok(None)` if still waiting.
    pub fn poll_offer(&self, opened: &OpenFolder) -> FolderResult<Option<PairingCompleted>> {
        poll_offer_at(
            opened,
            &self.identity,
            &artifact(&dot_dir(&opened.root), OFFER_SUFFIX),
        )
    }

    // ---------------------------------------------------------------------
    // Accepting / joining an offer
    // ---------------------------------------------------------------------

    /// Accepts an offer in any supported shape:
    /// 1. A 6-character short code (`XUM5CA`, `xum-5ca`, etc.)
    /// 2. A sealed payload envelope string (`FERRY1:...`)
    /// 3. A path to a payload file on disk (`.../pair-offer.ferry-pair`)
    ///
    /// If `target` is provided, that directory will be initialized; if `None`,
    /// the current directory is used.
    pub fn accept_offer(
        &self,
        code_or_payload: &str,
        target: Option<&Path>,
    ) -> FolderResult<PendingAcceptance> {
        let input = code_or_payload.trim();
        if let Some(envelope) = parse_payload_envelope(input) {
            return self.accept_envelope(envelope, target);
        }
        if code_key(input).len() == 6 {
            return self.accept_code(input, target);
        }
        let offer_file = PathBuf::from(input);
        let envelope = read_envelope_file(&offer_file)?;
        self.accept_payload(envelope, offer_file, target)
    }

    /// In-band acceptance of a 6-character short code.
    fn accept_code(&self, code: &str, target: Option<&Path>) -> FolderResult<PendingAcceptance> {
        let key = code_key(code);
        if !PairingCode::verify(code) {
            return Err(FolderError::new(
                "pairing-not-found",
                format!("pairing code {key} not found"),
                "check the code and try again",
            ));
        }
        if let Some(record) = self.peek_session(&key) {
            let record = self.consume_expired(&record)?;
            let accepted = self.join_via_session(&record, target)?;
            return Ok(PendingAcceptance {
                expected_short_code: record.code,
                response_path: None,
                target: accepted.folder.clone(),
                grant_path: None,
                offer_bytes: record.offer_bytes,
                identity: self.identity.clone(),
                done: Some(accepted),
            });
        }
        let accepted = self.join_via_network(&key, target)?;
        Ok(PendingAcceptance {
            expected_short_code: key,
            response_path: None,
            target: accepted.folder.clone(),
            grant_path: None,
            offer_bytes: vec![],
            identity: self.identity.clone(),
            done: Some(accepted),
        })
    }

    fn accept_envelope(
        &self,
        envelope: PayloadEnvelope,
        target: Option<&Path>,
    ) -> FolderResult<PendingAcceptance> {
        if expired(envelope.expires_at, SystemTime::now()) {
            return Err(FolderError::new(
                "pairing-expired",
                format!("offer {} expired", envelope.code),
                "ask the sharing device to create a new code",
            ));
        }
        if let Some(record) = self.peek_session(&envelope.code) {
            let record = self.consume_expired(&record)?;
            let accepted = self.join_via_session(&record, target)?;
            return Ok(PendingAcceptance {
                expected_short_code: record.code,
                response_path: None,
                target: accepted.folder.clone(),
                grant_path: None,
                offer_bytes: record.offer_bytes,
                identity: self.identity.clone(),
                done: Some(accepted),
            });
        }
        if let Ok(accepted) = self.join_via_network(&envelope.code, target) {
            return Ok(PendingAcceptance {
                expected_short_code: envelope.code,
                response_path: None,
                target: accepted.folder.clone(),
                grant_path: None,
                offer_bytes: envelope.offer_bytes,
                identity: self.identity.clone(),
                done: Some(accepted),
            });
        }
        Err(FolderError::new(
            "no-answer-channel",
            "this offer has no shared location to answer beside",
            "pass the path to the .ferry-pair payload file instead, or use the 6-character code",
        ))
    }

    fn join_via_network(&self, key: &str, target: Option<&Path>) -> FolderResult<Accepted> {
        let target = target.unwrap_or(Path::new("."));
        if dot_dir(target).is_dir() {
            return Err(FolderError::new(
                "already-initialized",
                format!("{} already contains a .ferry store", target.display()),
                "pick an empty directory",
            ));
        }

        let identity = self.identity.clone();
        let target_buf = target.to_path_buf();

        let accepted = client_discover_and_join(key, Duration::from_secs(3), move |offer_bytes| {
            let offer = PairingOffer::parse(&offer_bytes)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
            let now = ferry_platform::time::now_unix().0;
            let response = respond(&offer, &identity, now);
            let resp_bytes = response.serialize();

            let grant_handler = move |grant_bytes: Vec<u8>| -> std::io::Result<Accepted> {
                let (folder_id, poly, wrapped) =
                    open_grant(&offer_bytes, &grant_bytes).map_err(|e| {
                        std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
                    })?;
                let fmk = *unwrap_folder_key(&wrapped, &folder_id, &identity).map_err(|e| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
                })?;

                let accepted =
                    adopt_and_record(&target_buf, &identity, folder_id, &fmk, poly, &offer_bytes)
                        .map_err(|e| std::io::Error::other(e.to_string()))?;

                Ok(accepted)
            };

            Ok((resp_bytes, grant_handler))
        })
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::TimedOut {
                FolderError::new(
                    "pairing-not-found",
                    format!("pairing code {key} not found"),
                    "check the code and try again",
                )
            } else {
                FolderError::new("pairing-failed", e.to_string(), "retry pairing")
            }
        })?;

        Ok(accepted)
    }

    fn accept_payload(
        &self,
        envelope: PayloadEnvelope,
        offer_file: PathBuf,
        target: Option<&Path>,
    ) -> FolderResult<PendingAcceptance> {
        if expired(envelope.expires_at, SystemTime::now()) {
            return Err(FolderError::new(
                "pairing-expired",
                format!("offer {} expired", envelope.code),
                "ask the sharing device to create a new code",
            ));
        }
        let target = target.unwrap_or(Path::new("."));
        if dot_dir(target).is_dir() {
            return Err(FolderError::new(
                "already-initialized",
                format!("{} already contains a .ferry store", target.display()),
                "cd into the empty directory you want synced, or remove the old store deliberately",
            ));
        }

        let offer = PairingOffer::parse(&envelope.offer_bytes).map_err(bad_offer)?;
        let response = respond(&offer, &self.identity, ferry_platform::time::now_unix().0);
        let response_path = offer_file.with_file_name(RESPONSE_SUFFIX);
        std::fs::write(&response_path, response.serialize()).code(
            "io",
            format!(
                "cannot write {} (offer must live on a writable shared location)",
                response_path.display()
            ),
        )?;

        Ok(PendingAcceptance {
            expected_short_code: envelope.code,
            response_path: Some(response_path),
            target: target.to_path_buf(),
            grant_path: Some(offer_file.with_file_name(GRANT_SUFFIX)),
            offer_bytes: envelope.offer_bytes,
            identity: self.identity.clone(),
            done: None,
        })
    }

    fn peek_session(&self, key: &str) -> Option<SessionRecord> {
        self.rendezvous
            .lock()
            .expect("rendezvous map")
            .get(key)
            .cloned()
    }

    fn consume_expired(&self, record: &SessionRecord) -> FolderResult<SessionRecord> {
        if expired(record.expires_at, SystemTime::now()) {
            if let Some(ref stop) = record.server_stop {
                stop.store(true, Ordering::SeqCst);
            }
            self.rendezvous
                .lock()
                .expect("rendezvous map")
                .remove(&record.code);
            return Err(FolderError::new(
                "pairing-expired",
                format!("pairing code {} expired", record.code),
                "ask the sharing device to create a new code",
            ));
        }
        Ok(record.clone())
    }

    fn join_via_session(
        &self,
        record: &SessionRecord,
        target: Option<&Path>,
    ) -> FolderResult<Accepted> {
        let target = target.unwrap_or(Path::new("."));
        if dot_dir(target).is_dir() {
            return Err(FolderError::new(
                "already-initialized",
                format!("{} already contains a .ferry store", target.display()),
                "pick an empty directory",
            ));
        }

        let offer = PairingOffer::parse(&record.offer_bytes).map_err(bad_offer)?;
        let response = respond(&offer, &self.identity, ferry_platform::time::now_unix().0);
        response.verify(&offer, &record.offer_bytes).map_err(|e| {
            FolderError::new(
                "pairing-failed",
                e.to_string(),
                "the pairing code did not match this offer; start over",
            )
        })?;

        let wrapped_for_peer =
            wrap_folder_key(&record.fmk, &record.folder_id, self.identity.public()).code(
                "crypto",
                "identity keys are local; retry with a fresh identity if this repeats",
            )?;
        let sealed = seal_grant(&record.offer_bytes, &wrapped_for_peer, record.poly)?;
        let (_, poly, wrapped) = open_grant(&record.offer_bytes, &sealed)?;
        let fmk = *unwrap_folder_key(&wrapped, &record.folder_id, &self.identity).map_err(|e| {
            FolderError::new(
                "key-unwrap",
                e.to_string(),
                "the grant did not address this device; re-run the pairing",
            )
        })?;

        let accepted = self.adopt(target, record.folder_id, &fmk, poly)?;

        let wrapped_for_initiator = wrap_folder_key(&fmk, &record.folder_id, &record.initiator_pub)
            .code(
                "crypto",
                "identity keys are local; retry with a fresh identity if this repeats",
            )?;
        append_wrap_entry_for(
            target,
            record.folder_id,
            &record.initiator_pub,
            &wrapped_for_initiator,
        )?;

        let _ = append_wrap_entry_for(
            &record.folder_path,
            record.folder_id,
            self.identity.public(),
            &wrapped_for_peer,
        );

        if let Some(ref stop) = record.server_stop {
            stop.store(true, Ordering::SeqCst);
        }
        self.rendezvous
            .lock()
            .expect("rendezvous map")
            .remove(&record.code);

        Ok(accepted)
    }

    fn adopt(
        &self,
        target: &Path,
        folder_id: [u8; 16],
        fmk: &Fmk,
        poly: u64,
    ) -> FolderResult<Accepted> {
        let store = adopt_folder(target, &self.identity, folder_id, fmk, poly)?;
        store
            .flush()
            .map_err(|e| FolderError::new("store", e.to_string(), "retry"))?;
        store
            .write_index_snapshot()
            .map_err(|e| FolderError::new("store", e.to_string(), "retry"))?;
        save_settings(
            target,
            &Settings {
                format_version: SETTINGS_FORMAT_VERSION,
                folder_id: ferry_store::format::hex(&folder_id),
                honor_gitignore: false,
                presets: Vec::new(),
                overrides: Vec::new(),
            },
        )?;
        Ok(Accepted {
            folder: target.to_path_buf(),
            folder_id,
        })
    }

    fn find_folder_path(&self, folder_id_hex: &str) -> Option<PathBuf> {
        let key = folder_id_hex.to_ascii_lowercase();
        if let Some(p) = self
            .folder_overrides
            .lock()
            .expect("folder overrides")
            .get(&key)
            .cloned()
        {
            return Some(p);
        }
        if let Ok(records) = FolderInventory::new(&self.home).list() {
            for rec in records {
                if rec.folder_id.to_ascii_lowercase() == key {
                    return Some(rec.path);
                }
            }
        }
        None
    }
}

pub struct PendingOffer {
    pub short_code: String,

    pub payload: Vec<u8>,

    pub payload_path: PathBuf,

    pub expires_at: SystemTime,
}

impl PendingOffer {
    #[must_use]
    pub fn qr_payload(&self) -> String {
        String::from_utf8_lossy(&self.payload).into_owned()
    }

    pub fn write_payload(&self) -> FolderResult<()> {
        std::fs::write(&self.payload_path, &self.payload).code(
            "io",
            format!("cannot write {}", self.payload_path.display()),
        )
    }

    pub fn complete(
        self,
        opened: &OpenFolder,
        identity: &DeviceIdentity,
        timeout_secs: u64,
    ) -> FolderResult<PairingCompleted> {
        std::fs::write(&self.payload_path, &self.payload).code(
            "io",
            format!("cannot write {}", self.payload_path.display()),
        )?;

        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        while Instant::now() < deadline {
            if let Some(done) = poll_offer_at(opened, identity, &self.payload_path)? {
                return Ok(done);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let response_path = self.payload_path.with_file_name(RESPONSE_SUFFIX);
        Err(FolderError::new(
            "pair-timeout",
            format!(
                "no response appeared within {timeout_secs}s at {}",
                response_path.display()
            ),
            "run the accepting step against this offer file on the other device, then retry",
        ))
    }
}

pub struct PairingCompleted {
    pub peer_device_id: DeviceId,
    pub folder_id: [u8; 16],

    pub short_code: String,
    pub offer_path: PathBuf,

    pub grant_path: PathBuf,
}

pub struct PendingAcceptance {
    pub expected_short_code: String,

    pub response_path: Option<PathBuf>,
    target: PathBuf,
    grant_path: Option<PathBuf>,
    offer_bytes: Vec<u8>,
    identity: DeviceIdentity,
    done: Option<Accepted>,
}

impl PendingAcceptance {
    pub fn complete(self, timeout_secs: u64) -> FolderResult<Accepted> {
        if let Some(done) = self.done {
            return Ok(done);
        }
        let grant_path = self.grant_path.clone().ok_or_else(|| {
            FolderError::new(
                "bad-offer",
                "internal inconsistency: no grant location for a pending acceptance",
                "retry the pairing",
            )
        })?;
        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        let grant_bytes = loop {
            match std::fs::read(&grant_path) {
                Ok(bytes) => break bytes,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(_) => {
                    return Err(FolderError::new(
                        "pair-timeout",
                        format!(
                            "no grant appeared within {timeout_secs}s at {}",
                            grant_path.display()
                        ),
                        "make sure the other device completed its initiating step and copied pair-grant.ferry-grant beside the offer file",
                    ));
                }
            }
        };

        let (folder_id, poly, wrapped_for_peer) = open_grant(&self.offer_bytes, &grant_bytes)?;
        let fmk =
            *unwrap_folder_key(&wrapped_for_peer, &folder_id, &self.identity).map_err(|e| {
                FolderError::new(
                    "key-unwrap",
                    e.to_string(),
                    "the grant did not address this device; re-run the pairing",
                )
            })?;

        let accepted = adopt_and_record(
            &self.target,
            &self.identity,
            folder_id,
            &fmk,
            poly,
            &self.offer_bytes,
        )?;
        Ok(accepted)
    }
}

fn adopt_and_record(
    target: &Path,
    identity: &DeviceIdentity,
    folder_id: [u8; 16],
    fmk: &Fmk,
    poly: u64,
    offer_bytes: &[u8],
) -> FolderResult<Accepted> {
    let store = adopt_folder(target, identity, folder_id, fmk, poly)?;
    store
        .flush()
        .map_err(|e| FolderError::new("store", e.to_string(), "retry"))?;
    store
        .write_index_snapshot()
        .map_err(|e| FolderError::new("store", e.to_string(), "retry"))?;
    save_settings(
        target,
        &Settings {
            format_version: SETTINGS_FORMAT_VERSION,
            folder_id: ferry_store::format::hex(&folder_id),
            honor_gitignore: false,
            presets: Vec::new(),
            overrides: Vec::new(),
        },
    )?;

    let initiator = PairingOffer::parse(offer_bytes)
        .map_err(bad_offer)?
        .initiator_pub;
    let wrapped_for_initiator = wrap_folder_key(fmk, &folder_id, &initiator).code(
        "crypto",
        "identity keys are local; retry with a fresh identity if this repeats",
    )?;
    append_wrap_entry_for(target, folder_id, &initiator, &wrapped_for_initiator)?;

    Ok(Accepted {
        folder: target.to_path_buf(),
        folder_id,
    })
}

pub struct Accepted {
    pub folder: PathBuf,
    pub folder_id: [u8; 16],
}

fn artifact(folder_dot: &Path, suffix: &str) -> PathBuf {
    folder_dot.join(suffix)
}

fn read_envelope_file(path: &Path) -> FolderResult<PayloadEnvelope> {
    let bytes = std::fs::read(path).code(
        "not-found",
        "check the path to the offer file printed by the sharing device",
    )?;
    let text = String::from_utf8_lossy(&bytes);
    parse_payload_envelope(&text).ok_or_else(|| {
        FolderError::new(
            "bad-offer",
            "the payload file is not a FERRY1 pairing envelope",
            "get a fresh offer file from the sharing device",
        )
    })
}

fn bad_offer(e: ferry_crypto::pairing::PairingError) -> FolderError {
    FolderError::new(
        "bad-offer",
        e.to_string(),
        "get a fresh offer file from the sharing device",
    )
}

fn seal_grant(
    offer_bytes: &[u8],
    wrapped_for_peer: &[u8; WRAPPED_LEN],
    poly: u64,
) -> FolderResult<Vec<u8>> {
    let body = json!({
        "wrapped_for_peer": hex_of(wrapped_for_peer),
        "poly": poly,
    })
    .to_string()
    .into_bytes();
    seal_pair_grant(offer_bytes, &body).map_err(grant_error)
}

fn open_grant(offer_bytes: &[u8], raw: &[u8]) -> FolderResult<([u8; 16], u64, [u8; WRAPPED_LEN])> {
    let body = open_pair_grant(offer_bytes, raw).map_err(grant_error)?;
    let doc: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|_| FolderError::new("bad-grant", "grant body unreadable", "redo the pairing"))?;
    let wrapped_hex = doc["wrapped_for_peer"].as_str().ok_or_else(|| {
        FolderError::new("bad-grant", "grant body incomplete", "redo the pairing")
    })?;
    let poly = doc["poly"].as_u64().ok_or_else(|| {
        FolderError::new("bad-grant", "grant body incomplete", "redo the pairing")
    })?;
    let wrapped = unhex_80(wrapped_hex)?;

    let folder_id: [u8; 16] = offer_bytes
        .get(5..21)
        .and_then(|s| <&[u8] as TryInto<[u8; 16]>>::try_into(s).ok())
        .ok_or_else(|| {
            FolderError::new(
                "bad-grant",
                "the offer file is truncated",
                "get a fresh offer file from the sharing device",
            )
        })?;
    Ok((folder_id, poly, wrapped))
}

fn grant_error(e: GrantError) -> FolderError {
    match e {
        GrantError::Malformed { .. } => FolderError::new(
            "bad-grant",
            "the grant file is malformed",
            "have the other device re-run its initiating step",
        ),
        GrantError::Auth => FolderError::new(
            "bad-grant",
            "the grant file failed authentication",
            "it must travel together with THIS exact offer file; redo the pairing",
        ),
        GrantError::OfferTruncated { .. } => FolderError::new(
            "bad-grant",
            "the offer file is truncated",
            "get a fresh offer file from the sharing device",
        ),
        GrantError::Internal => FolderError::new("crypto", "grant seal failed", "retry"),
    }
}

fn unhex_80(s: &str) -> FolderResult<[u8; WRAPPED_LEN]> {
    ferry_store::format::unhex::<WRAPPED_LEN>(s).ok_or_else(|| {
        FolderError::new(
            "bad-grant",
            "grant key envelope is not 160 hex chars",
            "redo the pairing",
        )
    })
}

fn hex_of(b: &[u8]) -> String {
    ferry_store::format::hex(b)
}

fn poll_offer_at(
    opened: &OpenFolder,
    identity: &DeviceIdentity,
    offer_path: &Path,
) -> FolderResult<Option<PairingCompleted>> {
    if !offer_path.exists() {
        return Ok(None);
    }
    let envelope = read_envelope_file(offer_path)?;

    let dot = dot_dir(&opened.root);
    let response_path = artifact(&dot, RESPONSE_SUFFIX);
    if !response_path.exists() {
        return Ok(None);
    }
    let Ok(response_bytes) = std::fs::read(&response_path) else {
        return Ok(None);
    };
    if response_bytes.is_empty() {
        return Ok(None);
    }

    let response = match PairingResponse::parse(&response_bytes) {
        Ok(r) => r,
        Err(ferry_crypto::pairing::PairingError::Truncated { .. }) => return Ok(None),
        Err(_) => {
            return Err(FolderError::new(
                "pair-bad-response",
                "the response file is damaged; have the other device re-run accept",
                "have the other device re-run accept",
            ));
        }
    };

    let fmk = unwrap_own_fmk(opened, identity)?;
    let done = complete_pairing(
        &PairingOffer::parse(&envelope.offer_bytes).map_err(bad_offer)?,
        &envelope.offer_bytes,
        &response,
        &fmk,
        identity,
    )
    .map_err(|e| {
        FolderError::new(
            "pair-verify",
            e.to_string(),
            "the response did not match this offer; start over with a fresh offer",
        )
    })?;

    append_wrap_entry_for(
        &opened.root,
        opened.folder_id,
        &done.peer_pub,
        &done.wrapped_for_peer,
    )?;

    let grant = seal_grant(&envelope.offer_bytes, &done.wrapped_for_peer, opened.poly)?;
    let grant_path = artifact(&dot, GRANT_SUFFIX);
    std::fs::write(&grant_path, &grant)
        .code("io", format!("cannot write {}", grant_path.display()))?;

    Ok(Some(PairingCompleted {
        peer_device_id: done.peer_pub,
        folder_id: opened.folder_id,
        short_code: envelope.code,
        offer_path: offer_path.to_path_buf(),
        grant_path,
    }))
}

pub use ferry_rendezvous::{
    bind_discovery_socket, recv_frame, send_frame, service_name_for_code, topic_for_code,
    DISCOVERY_PORT, MAX_PAIRING_FRAME_LEN, MULTICAST_ADDR, PAIRING_TOPIC_KEY,
};

pub fn start_pairing_server<F>(
    code: String,
    offer_bytes: Vec<u8>,
    expires_at: SystemTime,
    on_response: F,
) -> io::Result<Arc<AtomicBool>>
where
    F: FnOnce(Vec<u8>) -> io::Result<Vec<u8>> + Send + 'static,
{
    let handle =
        ferry_rendezvous::start_pairing_server(code, offer_bytes, expires_at, on_response)?;
    Ok(handle.stopped)
}

pub fn client_discover_and_join<H, G, R>(
    code: &str,
    timeout: Duration,
    perform_handshake: H,
) -> io::Result<R>
where
    H: FnOnce(Vec<u8>) -> io::Result<(Vec<u8>, G)>,
    G: FnOnce(Vec<u8>) -> io::Result<R>,
{
    ferry_rendezvous::client_discover_and_join(code, timeout, perform_handshake)
}
