

use crate::framing::IpcConnection;


pub type InMemoryStream = tokio::io::DuplexStream;


pub type InMemoryConnection = IpcConnection<InMemoryStream>;


#[must_use]
pub fn create_in_memory_pair() -> (InMemoryConnection, InMemoryConnection) {
    create_in_memory_pair_with_buffer_size(65536)
}


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

    
    pub struct IpcServer {
        listener: UnixListener,
        socket_path: PathBuf,
    }

    impl IpcServer {
        
        
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

        
        pub async fn accept(&self) -> Result<IpcConnection<UnixStream>, IpcError> {
            let (stream, _addr) = self.listener.accept().await?;
            Ok(IpcConnection::new(stream))
        }

        
        #[must_use]
        pub fn socket_path(&self) -> &Path {
            &self.socket_path
        }

        
        pub fn close(&self) {
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }

    impl Drop for IpcServer {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }

    
    pub struct IpcClient;

    impl IpcClient {
        
        pub async fn connect(
            path: impl AsRef<Path>,
        ) -> Result<IpcConnection<UnixStream>, IpcError> {
            let stream = UnixStream::connect(path).await?;
            Ok(IpcConnection::new(stream))
        }
    }
}

#[cfg(windows)]
pub mod windows {
    use std::path::Path;
    use std::sync::Mutex;
    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
    };

    use crate::error::IpcError;
    use crate::framing::IpcConnection;

    
    pub struct IpcServer {
        pipe_name: String,
        server: Mutex<Option<NamedPipeServer>>,
    }

    impl IpcServer {
        
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

        
        pub async fn accept(&self) -> Result<IpcConnection<NamedPipeServer>, IpcError> {
            let current_server = {
                let mut guard = self
                    .server
                    .lock()
                    .map_err(|e| IpcError::Protocol(format!("Lock poisoned: {e}")))?;
                guard.take().ok_or_else(|| {
                    IpcError::Protocol("Server instance uninitialized".to_string())
                })?
            };

            current_server.connect().await?;

            
            let next_server = ServerOptions::new().create(&self.pipe_name)?;
            {
                let mut guard = self
                    .server
                    .lock()
                    .map_err(|e| IpcError::Protocol(format!("Lock poisoned: {e}")))?;
                *guard = Some(next_server);
            }

            Ok(IpcConnection::new(current_server))
        }

        
        #[must_use]
        pub fn pipe_name(&self) -> &str {
            &self.pipe_name
        }

        
        pub fn close(&self) {}
    }

    
    pub struct IpcClient;

    impl IpcClient {
        
        pub async fn connect(
            path: impl AsRef<Path>,
        ) -> Result<IpcConnection<NamedPipeClient>, IpcError> {
            let pipe_name = path.as_ref().to_string_lossy().to_string();
            let mut attempts = 0;
            let client = loop {
                match ClientOptions::new().open(&pipe_name) {
                    Ok(client) => break client,
                    Err(e)
                        if (e.raw_os_error() == Some(231)
                            || e.kind() == std::io::ErrorKind::ResourceBusy)
                            && attempts < 50 =>
                    {
                        attempts += 1;
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                    Err(e) => return Err(IpcError::from(e)),
                }
            };
            Ok(IpcConnection::new(client))
        }
    }
}
