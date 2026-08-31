pub mod engine;
pub mod exchange;
pub mod session;
pub mod transport;

pub use engine::{
    pick_donor, select_donor, EngineConfig, EngineHandle, EngineStats, IngestError,
    PeerExpectation, PeerPolicy, PeerState, SessionError, SyncEngine,
};
pub use exchange::{ingest_pack_verified, run_v1_session, CurrentState, ExchangeHost};
pub use ferry_crypto::identity::DeviceIdentity;
pub use ferry_materialize::{ApplyOutcome, MaterializeError as ApplyError};
pub use ferry_store::format;
pub use ferry_store::{BlobId, BlobKind};
pub use session::{Established, ExpectPeer};
pub use transport::{Connection, Listener, TcpTransport, Transport};

pub const DEFAULT_FOLDER_ID: [u8; 16] = [
    0x6d, 0x30, 0x2d, 0x73, 0x6b, 0x65, 0x6c, 0x3a, 0x66, 0x6f, 0x6c, 0x64, 0x65, 0x72, 0x21, 0x21,
];

pub fn empty_tree_id() -> BlobId {
    *blake3::hash(&ferry_store::manifest::serialize_tree_node(
        &ferry_store::manifest::TreeNode {
            entries: Vec::new(),
        },
    ))
    .as_bytes()
}
