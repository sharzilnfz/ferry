//! Embedded HTTP dashboard: axum on tokio beside the sync engine.
//!
//! v0 stance (`.scratch/web-dashboard/spec.md`): loopback bind only, no
//! auth, JSON documents shaped exactly per `docs/cli-json.md`, and no GET
//! ever rescans or hashes the tree. `/api/status` reads only the engine's
//! cached folder pointers (short lock inside `EngineHandle`) plus cheap
//! `.ferry/` metadata files. `/api/events` (SSE) is deferred; it answers
//! 501 `not-implemented` and the UI degrades to polling.

mod actions;
pub mod backend;
pub mod error;
pub mod server;
mod status;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ferry_crypto::identity::DeviceIdentity;
use ferry_store::format::hex as hex_str;
use ferry_sync::EngineHandle;

pub use backend::{snapshot_to_status_doc, BoxFuture, DashboardBackend, DirectBackend, IpcBackend};
pub use error::{status_for_code, ApiError, OpError};
pub use server::{
    asset, extract_token, generate_token, is_token_valid, DashboardServer, APP_JS, INDEX_HTML,
    STYLE_CSS,
};

/// Everything a handler needs. Cheap to clone behind the router state Arc.
pub struct UiState {
    handle: EngineHandle,
    store_dir: PathBuf,
    tree_dir: PathBuf,
    folder_id: [u8; 16],
    device_hex: String,
    identity: DeviceIdentity,
}

impl UiState {
    pub fn new(
        handle: EngineHandle,
        store_dir: PathBuf,
        tree_dir: PathBuf,
        folder_id: [u8; 16],
        identity: DeviceIdentity,
    ) -> Self {
        Self {
            handle,
            device_hex: hex_str(identity.public()),
            identity,
            store_dir,
            tree_dir,
            folder_id,
        }
    }

    pub(crate) fn handle(&self) -> &EngineHandle {
        &self.handle
    }

    /// The daemon's folder root IS its `--tree`; `.ferry/` lives under the
    /// `--store` dir (engine layout).
    pub(crate) fn tree_dir(&self) -> &Path {
        &self.tree_dir
    }

    pub(crate) fn state_dir(&self) -> PathBuf {
        self.store_dir.join(".ferry")
    }

    pub(crate) fn folder_id(&self) -> [u8; 16] {
        self.folder_id
    }

    pub(crate) fn device_hex(&self) -> &str {
        &self.device_hex
    }

    pub(crate) fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }
}

/// Bind failure surfaces synchronously so `--ui` typos fail startup loudly;
/// the server itself runs detached and dies with the process.
pub fn spawn(addr: SocketAddr, state: Arc<UiState>) -> Result<(), String> {
    DashboardServer::new(Arc::new(DirectBackend::new(state))).spawn(addr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    #[test]
    fn deferred_and_gate_codes_map_to_their_spec_statuses() {
        assert_eq!(
            status_for_code("warming-up"),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            status_for_code("not-implemented"),
            StatusCode::NOT_IMPLEMENTED
        );
        assert_eq!(status_for_code("pin-active"), StatusCode::CONFLICT);
        assert_eq!(status_for_code("not-found"), StatusCode::NOT_FOUND);
        assert_eq!(status_for_code("bad-request"), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn assets_embed_with_mime_types() {
        let (html, mime) = asset("/index.html").expect("index");
        assert_eq!(mime, "text/html; charset=utf-8");
        assert!(!html.is_empty());
        assert_eq!(asset("/style.css").unwrap().1, "text/css; charset=utf-8");
        assert_eq!(
            asset("/app.js").unwrap().1,
            "text/javascript; charset=utf-8"
        );
        assert!(asset("/missing.png").is_none());
    }
}
