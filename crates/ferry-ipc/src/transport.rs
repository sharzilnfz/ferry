//! IPC transport helpers: in-memory duplex streams and platform socket / named pipe transports.

use crate::framing::IpcConnection;

/// In-memory duplex byte stream for unit and integration testing.
pub type InMemoryStream = tokio::io::DuplexStream;

/// In-memory IPC connection for testing.
pub type InMemoryConnection = IpcConnection<InMemoryStream>;

/// Create a connected pair of in-memory duplex IPC connections with default buffer (64 KB).
#[must_use]
pub fn create_in_memory_pair() -> (InMemoryConnection, InMemoryConnection) {
    create_in_memory_pair_with_buffer_size(65536)
}

/// Create a connected pair of in-memory duplex IPC connections with a custom buffer size.
#[must_use]
pub fn create_in_memory_pair_with_buffer_size(
    buffer_size: usize,
) -> (InMemoryConnection, InMemoryConnection) {
    let (a, b) = tokio::io::duplex(buffer_size);
    (IpcConnection::new(a), IpcConnection::new(b))
}

#[cfg(unix)]
pub mod unix {
    use std::path::{Path, PathBuf};
    use tokio::net::{UnixListener, UnixStream};

    use crate::error::IpcError;
    use crate::framing::IpcConnection;

    /// Server listening for IPC client connections over a Unix Domain Socket.
    pub struct IpcServer {
        listener: UnixListener,
        socket_path: PathBuf,
    }

    impl IpcServer {
        /// Bind to the specified Unix Domain Socket path.
        /// Automatically creates parent directories and removes stale existing socket files.
        pub fn bind(path: impl AsRef<Path>) -> Result<Self, IpcError> {
            let path = path.as_ref();
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if path.exists() {
                let _ = std::fs::remove_file(path);
            }
            let listener = UnixListener::bind(path)?;
            Ok(Self {
                listener,
                socket_path: path.to_path_buf(),
            })
        }

        /// Accept the next incoming client connection.
        pub async fn accept(&self) -> Result<IpcConnection<UnixStream>, IpcError> {
            let (stream, _addr) = self.listener.accept().await?;
            Ok(IpcConnection::new(stream))
        }

        /// The bound socket path.
        #[must_use]
        pub fn socket_path(&self) -> &Path {
            &self.socket_path
        }

        /// Explicitly clean up the socket file from the filesystem.
        pub fn close(&self) {
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }

    impl Drop for IpcServer {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }

    /// Client for connecting to a Unix Domain Socket IPC server.
    pub struct IpcClient;

    impl IpcClient {
        /// Connect to a Unix Domain Socket at the specified path.
        pub async fn connect(path: impl AsRef<Path>) -> Result<IpcConnection<UnixStream>, IpcError> {
            let stream = UnixStream::connect(path).await?;
            Ok(IpcConnection::new(stream))
        }
    }
}

#[cfg(windows)]
pub mod windows {
    use std::path::Path;
    use std::sync::Mutex;
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions};

    use crate::error::IpcError;
    use crate::framing::IpcConnection;

    /// Server listening for IPC client connections over a Windows Named Pipe.
    pub struct IpcServer {
        pipe_name: String,
        server: Mutex<Option<NamedPipeServer>>,
    }

    impl IpcServer {
        /// Bind to the specified Windows Named Pipe.
        pub fn bind(path: impl AsRef<Path>) -> Result<Self, IpcError> {
            let pipe_name = path.as_ref().to_string_lossy().to_string();
            let server = ServerOptions::new()
                .first_pipe_instance(true)
                .create(&pipe_name)?;
            Ok(Self {
                pipe_name,
                server: Mutex::new(Some(server)),
            })
        }

        /// Accept the next incoming client connection.
        pub async fn accept(&self) -> Result<IpcConnection<NamedPipeServer>, IpcError> {
            let current_server = {
                let mut guard = self.server.lock().map_err(|e| {
                    IpcError::Protocol(format!("Lock poisoned: {e}"))
                })?;
                guard.take().ok_or_else(|| {
                    IpcError::Protocol("Server instance uninitialized".to_string())
                })?
            };

            current_server.connect().await?;

            // Prepare next pipe instance for subsequent connections
            let next_server = ServerOptions::new().create(&self.pipe_name)?;
            {
                let mut guard = self.server.lock().map_err(|e| {
                    IpcError::Protocol(format!("Lock poisoned: {e}"))
                })?;
                *guard = Some(next_server);
            }

            Ok(IpcConnection::new(current_server))
        }

        /// The pipe name.
        #[must_use]
        pub fn pipe_name(&self) -> &str {
            &self.pipe_name
        }

        /// Explicit close method (no-op on Windows).
        pub fn close(&self) {}
    }

    /// Client for connecting to a Windows Named Pipe IPC server.
    pub struct IpcClient;

    impl IpcClient {
        /// Connect to a Windows Named Pipe at the specified path.
        pub async fn connect(path: impl AsRef<Path>) -> Result<IpcConnection<NamedPipeClient>, IpcError> {
            let pipe_name = path.as_ref().to_string_lossy().to_string();
            let client = ClientOptions::new().open(&pipe_name)?;
            Ok(IpcConnection::new(client))
        }
    }
}
