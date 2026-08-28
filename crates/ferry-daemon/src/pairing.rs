//! Zero-file in-band network pairing over ephemeral rendezvous topics and short codes.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use ferry_crypto::folder_key::{unwrap_folder_key, wrap_folder_key};
use ferry_crypto::identity::DeviceIdentity;
use ferry_crypto::pairing::{
    complete_pairing, derive_pairing_topic, open_pair_grant, respond, seal_pair_grant,
    verify_pairing_code, PairingOffer, PairingResponse, TransportHints,
};
use ferry_folder::folder::{
    open_folder, save_settings, write_default_ignore_if_absent, Settings,
    SETTINGS_FORMAT_VERSION,
};
use ferry_ipc::backend::{OpError, PairResult, PairingSession};
use ferry_store::format::{hex as hex_str, unhex};
use ferry_sync::transport::{Connection, Listener, Transport};
use serde::{Deserialize, Serialize};

/// Grant payload serialized into the sealed grant record during pairing.
#[derive(Serialize, Deserialize, Debug)]
pub struct PairGrantBody {
    pub folder_id: String,
    pub poly: u64,
    pub initiator_pub: String,
    pub wrapped_for_peer: String,
    #[serde(default)]
    pub sync_listen_addr: Option<String>,
}

/// Registry mapping rendezvous topics to active listener addresses for local / in-process / fallback routing.
#[derive(Default)]
struct RendezvousRegistry {
    topics: Mutex<HashMap<[u8; 32], SocketAddr>>,
}

static RENDEZVOUS: OnceLock<RendezvousRegistry> = OnceLock::new();

fn rendezvous_registry() -> &'static RendezvousRegistry {
    RENDEZVOUS.get_or_init(RendezvousRegistry::default)
}

/// Register a local listening address for a rendezvous topic.
pub fn register_rendezvous_topic(topic: [u8; 32], addr: SocketAddr) {
    rendezvous_registry().topics.lock().unwrap().insert(topic, addr);
}

/// Resolve a rendezvous topic into a listening address.
pub fn resolve_rendezvous_topic(topic: &[u8; 32]) -> Option<SocketAddr> {
    rendezvous_registry().topics.lock().unwrap().get(topic).copied()
}

/// Active in-band host pairing session.
pub struct HostPairingSession {
    pub session: PairingSession,
    pub listener_addr: SocketAddr,
    pub topic: [u8; 32],
}

/// Start an in-band pairing session on the host side.
///
/// Binds an ephemeral listener, advertises the rendezvous topic, computes the 6-word mnemonic,
/// and handles incoming in-band handshake in a background task without writing any pair files.
pub fn start_host_pairing(
    folder_root: &Path,
    identity: &DeviceIdentity,
    folder_id_override: Option<String>,
    sync_listen_addr_override: Option<SocketAddr>,
) -> Result<HostPairingSession, OpError> {
    let opened = open_folder(folder_root, identity).map_err(|e| OpError::new(e.code, e.message, e.hint))?;
    let folder_id = if let Some(fid_str) = folder_id_override {
        unhex::<16>(&fid_str).ok_or_else(|| OpError::bad_request("invalid folder_id hex", "check folder id"))?
    } else {
        opened.folder_id
    };

    let fmk = ferry_folder::folder::unwrap_own_fmk(&opened, identity)
        .map_err(|e| OpError::new(e.code, e.message, e.hint))?;
    let poly = opened.poly;

    let now_sec = ferry_platform::time::now_unix().0;
    let offer = PairingOffer::create(folder_id, identity, now_sec);
    let offer_bytes = offer.serialize();

    let hints = TransportHints(TransportHints::DIRECT_LAN | TransportHints::RELAY_OFFERED);
    let code = offer.mnemonic(hints);
    let topic = derive_pairing_topic(&code);

    let transport = ferry_sync::transport::TcpTransport;
    let listener = transport
        .listen("127.0.0.1:0".parse().unwrap())
        .map_err(|e| OpError::new("network-error", e.to_string(), "cannot bind listener"))?;
    let local_addr = listener
        .local_addr()
        .map_err(|e| OpError::new("network-error", e.to_string(), "cannot get local addr"))?;

    register_rendezvous_topic(topic, local_addr);
    ferry_iroh::publish_topic(
        topic,
        ferry_iroh::Route {
            endpoint_id: *identity.public(),
            ip_hints: vec![local_addr],
        },
    );

    let session_id = format!("sess-{}", hex_str(&topic[..8]));
    let session = PairingSession {
        session_id: session_id.clone(),
        code: code.clone(),
        folder_id: hex_str(&folder_id),
        role: "host".to_string(),
        status: "advertising".to_string(),
        message: Some("Pairing session active. Share the 6-word code with your peer.".to_string()),
    };

    let host_identity = identity.clone();
    let folder_root_buf = folder_root.to_path_buf();

    // Read sync listen addr
    let sync_listen_addr = sync_listen_addr_override
        .map(|a| a.to_string())
        .or_else(|| {
            std::fs::read_to_string(folder_root_buf.join(".ferry/listen.addr"))
                .ok()
                .map(|s| s.trim().to_string())
        });

    let sync_addr_for_thread = sync_listen_addr.clone();

    // Spawn background worker to accept incoming connection and perform zero-file handshake
    std::thread::spawn(move || {
        let Ok(mut conn) = listener.accept() else { return };

        // 1. Host sends offer bytes
        if conn.send_frame(&offer_bytes).is_err() {
            return;
        }

        // 2. Host receives pairing response
        let Ok(resp_bytes) = conn.recv_frame() else { return };
        let Ok(response) = PairingResponse::parse(&resp_bytes) else { return };

        // 3. Host completes pairing and derives wrapped keys
        let Ok(completed) = complete_pairing(&offer, &offer_bytes, &response, &fmk, &host_identity) else { return };

        // 4. Host builds and seals pair grant record
        let grant_body = PairGrantBody {
            folder_id: hex_str(&folder_id),
            poly,
            initiator_pub: hex_str(host_identity.public()),
            wrapped_for_peer: hex_str(&completed.wrapped_for_peer),
            sync_listen_addr: sync_addr_for_thread,
        };
        let Ok(body_json) = serde_json::to_vec(&grant_body) else { return };
        let Ok(sealed_grant) = seal_pair_grant(&offer_bytes, &body_json) else { return };

        // 5. Host sends sealed grant
        if conn.send_frame(&sealed_grant).is_err() {
            return;
        }

        // 6. Host updates local folder store CONFIG_HEAD to trust peer device
        let _ = ferry_folder::folder::append_wrap_entry_for(
            &folder_root_buf,
            folder_id,
            &response.responder_pub,
            &completed.wrapped_for_peer,
        );
        if let Ok(opened) = open_folder(&folder_root_buf, &host_identity) {
            let _ = opened.store.flush();
        }
    });

    let record = ferry_ipc::backend::PairingSessionRecord {
        session_id: session_id.clone(),
        code: code.clone(),
        folder_id: hex_str(&folder_id),
        device_id: hex_str(identity.public()),
        listen_addr: local_addr.to_string(),
        poly,
        fmk_hex: hex_str(fmk.as_ref()),
        created_sec: now_sec,
        sync_listen_addr,
    };
    let _ = ferry_ipc::backend::save_pairing_record(&record);

    Ok(HostPairingSession {
        session,
        listener_addr: local_addr,
        topic,
    })
}

/// Execute joiner-side in-band pairing using a 6-word code.
///
/// Dials the host using the rendezvous topic derived from the code, runs the handshake,
/// unwraps the FMK, and initializes the folder store locally with zero temporary pair files.
pub async fn execute_joiner_pairing(
    code: &str,
    target_dir: &Path,
    identity: &DeviceIdentity,
) -> Result<PairResult, OpError> {
    let clean_code = code.trim();
    let topic = derive_pairing_topic(clean_code);

    // Resolve rendezvous destination
    let mut resolved_addr = resolve_rendezvous_topic(&topic);
    if resolved_addr.is_none() {
        if let Some(route) = ferry_iroh::resolve_topic(&topic) {
            if let Some(first) = route.ip_hints.first() {
                resolved_addr = Some(*first);
            }
        }
    }
    if resolved_addr.is_none() {
        if let Some(rec) = ferry_ipc::backend::load_pairing_record(clean_code) {
            if let Ok(addr) = rec.listen_addr.parse() {
                resolved_addr = Some(addr);
            }
        }
    }

    let dest_addr = resolved_addr.ok_or_else(|| {
        OpError::new(
            "not-found",
            format!("no host discovered for pairing code '{clean_code}'"),
            "ensure the host has an active pairing session",
        )
    })?;

    let identity_clone = identity.clone();
    let target_path_buf = target_dir.to_path_buf();
    let code_str = clean_code.to_string();

    tokio::task::spawn_blocking(move || {
        let transport = ferry_sync::transport::TcpTransport;
        let mut conn = transport
            .dial(dest_addr)
            .map_err(|e| OpError::new("connect-error", e.to_string(), "failed to connect to host"))?;

        // 1. Joiner receives offer bytes
        let offer_bytes = conn
            .recv_frame()
            .map_err(|e| OpError::new("network-error", e.to_string(), "failed to receive offer"))?;

        // 2. Joiner verifies code against offer bytes
        let _verified = verify_pairing_code(&code_str, &offer_bytes)
            .map_err(|e| OpError::new("pairing-verify", e.to_string(), "pairing code rejected"))?;

        let offer = PairingOffer::parse(&offer_bytes)
            .map_err(|e| OpError::new("pairing-offer", e.to_string(), "malformed offer from host"))?;

        // 3. Joiner constructs and sends response
        let now_sec = ferry_platform::time::now_unix().0;
        let response = respond(&offer, &identity_clone, now_sec);
        let resp_bytes = response.serialize();
        conn.send_frame(&resp_bytes)
            .map_err(|e| OpError::new("network-error", e.to_string(), "failed to send response"))?;

        // 4. Joiner receives and opens sealed grant
        let grant_bytes = conn
            .recv_frame()
            .map_err(|e| OpError::new("network-error", e.to_string(), "failed to receive grant"))?;

        let grant_body_bytes = open_pair_grant(&offer_bytes, &grant_bytes)
            .map_err(|e| OpError::new("grant-auth", e.to_string(), "grant authentication failed"))?;

        let grant_body: PairGrantBody = serde_json::from_slice(&grant_body_bytes)
            .map_err(|e| OpError::new("grant-format", e.to_string(), "malformed grant body"))?;

        let wrapped_for_peer = unhex::<80>(&grant_body.wrapped_for_peer)
            .ok_or_else(|| OpError::new("crypto", "invalid wrapped key hex", "grant corrupted"))?;

        let initiator_pub = unhex::<32>(&grant_body.initiator_pub)
            .ok_or_else(|| OpError::new("crypto", "invalid initiator pub hex", "grant corrupted"))?;

        // 5. Joiner unwraps the FMK
        let fmk = unwrap_folder_key(&wrapped_for_peer, &offer.folder_id, &identity_clone)
            .map_err(|e| OpError::new("crypto", e.to_string(), "failed to unwrap FMK"))?;

        // 6. Initialize local folder store
        std::fs::create_dir_all(&target_path_buf)
            .map_err(|e| OpError::new("io", e.to_string(), "cannot create target directory"))?;

        let store = ferry_folder::folder::adopt_folder(
            &target_path_buf,
            &identity_clone,
            offer.folder_id,
            &fmk,
            grant_body.poly,
        )
        .map_err(|e| OpError::new(e.code, e.message, e.hint))?;

        // Wrap FMK for host as peer in local CONFIG_HEAD
        if let Ok(wrapped_for_initiator) =
            wrap_folder_key(&fmk, &offer.folder_id, &initiator_pub)
        {
            let _ = ferry_folder::folder::append_wrap_entry_for(
                &target_path_buf,
                offer.folder_id,
                &initiator_pub,
                &wrapped_for_initiator,
            );
        }

        store
            .flush()
            .map_err(|e| OpError::new("store", e.to_string(), "store flush failed"))?;
        store
            .write_index_snapshot()
            .map_err(|e| OpError::new("store", e.to_string(), "index snapshot failed"))?;

        let settings = Settings {
            format_version: SETTINGS_FORMAT_VERSION,
            folder_id: hex_str(&offer.folder_id),
            honor_gitignore: true,
            presets: Vec::new(),
            overrides: Vec::new(),
        };
        save_settings(&target_path_buf, &settings)
            .map_err(|e| OpError::new(e.code, e.message, e.hint))?;
        let _ = write_default_ignore_if_absent(&target_path_buf);

        let sync_addr = grant_body
            .sync_listen_addr
            .or_else(|| {
                ferry_ipc::backend::load_pairing_record(&code_str)
                    .and_then(|r| r.sync_listen_addr)
            })
            .unwrap_or_else(|| dest_addr.to_string());
        let peers_dir = target_path_buf.join(".ferry").join("peers");
        let _ = std::fs::create_dir_all(&peers_dir);
        let _ = std::fs::write(
            peers_dir.join(format!("{}.addr", hex_str(&initiator_pub))),
            sync_addr,
        );

        Ok(PairResult {
            folder_id: hex_str(&offer.folder_id),
            device_id: hex_str(identity_clone.public()),
            folder_path: target_path_buf,
            status: "completed".to_string(),
            message: Some(format!("Successfully paired via code: {code_str}")),
        })
    })
    .await
    .map_err(|e| OpError::new("internal", e.to_string(), "worker error"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferry_folder::folder::create_folder;

    #[tokio::test]
    async fn zero_file_in_band_pairing_between_two_instances() {
        let dir_alice = tempfile::tempdir().unwrap();
        let dir_bob = tempfile::tempdir().unwrap();

        let alice_identity = DeviceIdentity::generate();
        let bob_identity = DeviceIdentity::generate();

        // 1. Alice creates a local folder
        let folder_id = rand::random::<[u8; 16]>();
        let mut rng = rand::thread_rng();
        let poly = ferry_store::chunker::generate_polynomial(&mut rng);
        let (store_a, _) = create_folder(dir_alice.path(), &alice_identity, folder_id, poly).unwrap();
        store_a.flush().unwrap();
        store_a.write_index_snapshot().unwrap();

        let settings = Settings {
            format_version: SETTINGS_FORMAT_VERSION,
            folder_id: hex_str(&folder_id),
            honor_gitignore: true,
            presets: Vec::new(),
            overrides: Vec::new(),
        };
        save_settings(dir_alice.path(), &settings).unwrap();

        // 2. Alice starts in-band pairing session
        let session = start_host_pairing(dir_alice.path(), &alice_identity, None, None).unwrap();
        assert_eq!(session.session.role, "host");
        assert_eq!(session.session.status, "advertising");
        assert_eq!(session.session.code.split('-').count(), 6);

        // 3. Bob joins using the 6-word code
        let join_result = execute_joiner_pairing(
            &session.session.code,
            dir_bob.path(),
            &bob_identity,
        )
        .await
        .unwrap();

        assert_eq!(join_result.status, "completed");
        assert_eq!(join_result.folder_id, hex_str(&folder_id));

        // 4. Verify Bob opened the folder and both unwrapped the same FMK
        let opened_alice = open_folder(dir_alice.path(), &alice_identity).unwrap();
        let opened_bob = open_folder(dir_bob.path(), &bob_identity).unwrap();
        assert_eq!(opened_bob.folder_id, folder_id);
        assert_eq!(opened_bob.poly, poly);

        let fmk_alice = ferry_folder::folder::unwrap_own_fmk(&opened_alice, &alice_identity).unwrap();
        let fmk_bob = ferry_folder::folder::unwrap_own_fmk(&opened_bob, &bob_identity).unwrap();
        assert_eq!(fmk_alice, fmk_bob);

        // 5. Verify no .ferry-pair files were created
        assert!(!dir_alice.path().join(".ferry").join("offer.ferry-pair").exists());
        assert!(!dir_alice.path().join(".ferry").join("response.ferry-pair").exists());
        assert!(!dir_alice.path().join(".ferry").join("grant.ferry-pair").exists());
        assert!(!dir_bob.path().join(".ferry").join("offer.ferry-pair").exists());
        assert!(!dir_bob.path().join(".ferry").join("response.ferry-pair").exists());
        assert!(!dir_bob.path().join(".ferry").join("grant.ferry-pair").exists());
    }
}
