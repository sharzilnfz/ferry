










































pub mod codec;
pub mod engine;
pub mod error;
pub mod frame;
pub mod secure;
pub mod stream;
pub mod version;

#[cfg(test)]
mod engine_tests;

pub use engine::{run_engine, EngineConfig, FolderState, Granularity, Role, SessionReport};
pub use error::ProtoError;
pub use ferry_crypto::identity::DeviceId;
pub use secure::SecureSession;
pub use stream::{duplex_pair, ByteStream};
pub use version::ProtocolVersion;





pub const WIRE_MAGIC: [u8; 4] = *b"FRW1";
