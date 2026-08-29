//! Line-based JSON message framing over async byte streams.

use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf, WriteHalf,
};

use crate::error::IpcError;
use crate::protocol::{ClientCommand, DaemonMessage};

/// Default maximum allowed message length (16 MB) to prevent unbounded memory allocation.
pub const DEFAULT_MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

/// Asynchronous JSON sender half for an IPC connection.
pub struct IpcSender<W> {
    writer: W,
}

impl<W: AsyncWrite + Unpin> IpcSender<W> {
    /// Create a new `IpcSender` wrapping an `AsyncWrite` stream.
    pub fn new(writer: W) -> Self {
        Self { writer }
    }

    /// Serialize and send a typed message followed by a newline delimiter, then flush.
    pub async fn send<M: Serialize>(&mut self, msg: &M) -> Result<(), IpcError> {
        let mut bytes = serde_json::to_vec(msg).map_err(IpcError::Serialization)?;
        bytes.push(b'\n');
        self.writer.write_all(&bytes).await?;
        self.writer.flush().await?;
        Ok(())
    }

    /// Send a server push message.
    pub async fn send_message(&mut self, msg: &DaemonMessage) -> Result<(), IpcError> {
        self.send(msg).await
    }

    /// Send a client command.
    pub async fn send_command(&mut self, cmd: &ClientCommand) -> Result<(), IpcError> {
        self.send(cmd).await
    }

    /// Send raw bytes (appending a newline if not already present) and flush.
    pub async fn send_raw(&mut self, raw: &[u8]) -> Result<(), IpcError> {
        self.writer.write_all(raw).await?;
        if !raw.ends_with(b"\n") {
            self.writer.write_all(b"\n").await?;
        }
        self.writer.flush().await?;
        Ok(())
    }

    /// Flush the underlying writer.
    pub async fn flush(&mut self) -> Result<(), IpcError> {
        self.writer.flush().await?;
        Ok(())
    }

    /// Unwraps and returns the underlying writer.
    pub fn into_inner(self) -> W {
        self.writer
    }
}

/// Asynchronous JSON receiver half for an IPC connection.
pub struct IpcReceiver<R> {
    reader: BufReader<R>,
    max_message_size: usize,
}

impl<R: AsyncRead + Unpin> IpcReceiver<R> {
    /// Create a new `IpcReceiver` wrapping an `AsyncRead` stream with the default max message size.
    pub fn new(reader: R) -> Self {
        Self::with_max_message_size(reader, DEFAULT_MAX_MESSAGE_SIZE)
    }

    /// Create a new `IpcReceiver` with a custom maximum message size limit.
    pub fn with_max_message_size(reader: R, max_message_size: usize) -> Self {
        Self {
            reader: BufReader::new(reader),
            max_message_size,
        }
    }

    /// Read the next raw line (trimmed of trailing newline/whitespace).
    /// Returns `Ok(None)` on clean EOF when no bytes were read.
    pub async fn recv_raw(&mut self) -> Result<Option<String>, IpcError> {
        let mut line = String::new();
        loop {
            line.clear();
            let bytes_read = self.reader.read_line(&mut line).await?;
            if bytes_read == 0 {
                return Ok(None);
            }
            if line.len() > self.max_message_size {
                return Err(IpcError::MessageTooLarge {
                    size: line.len(),
                    max: self.max_message_size,
                });
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                // skip empty lines
                continue;
            }
            return Ok(Some(trimmed.to_string()));
        }
    }

    /// Read and deserialize the next typed message.
    /// Returns `Ok(None)` on clean EOF.
    pub async fn recv<M: DeserializeOwned>(&mut self) -> Result<Option<M>, IpcError> {
        match self.recv_raw().await? {
            Some(raw) => {
                let msg: M = serde_json::from_str(&raw).map_err(IpcError::Deserialization)?;
                Ok(Some(msg))
            }
            None => Ok(None),
        }
    }

    /// Receive the next server push message.
    pub async fn recv_message(&mut self) -> Result<Option<DaemonMessage>, IpcError> {
        self.recv().await
    }

    /// Receive the next client command.
    pub async fn recv_command(&mut self) -> Result<Option<ClientCommand>, IpcError> {
        self.recv().await
    }

    /// Unwraps and returns the underlying reader.
    pub fn into_inner(self) -> R {
        self.reader.into_inner()
    }
}

/// A unified IPC connection combining a reader and a writer over a duplex stream.
pub struct IpcConnection<S> {
    sender: IpcSender<WriteHalf<S>>,
    receiver: IpcReceiver<ReadHalf<S>>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> IpcConnection<S> {
    /// Create a new `IpcConnection` from an underlying duplex stream.
    pub fn new(stream: S) -> Self {
        Self::with_max_message_size(stream, DEFAULT_MAX_MESSAGE_SIZE)
    }

    /// Create a new `IpcConnection` with a custom maximum message size limit.
    pub fn with_max_message_size(stream: S, max_message_size: usize) -> Self {
        let (read_half, write_half) = tokio::io::split(stream);
        Self {
            sender: IpcSender::new(write_half),
            receiver: IpcReceiver::with_max_message_size(read_half, max_message_size),
        }
    }

    /// Serialize and send a typed message.
    pub async fn send<M: Serialize>(&mut self, msg: &M) -> Result<(), IpcError> {
        self.sender.send(msg).await
    }

    /// Send a server push message.
    pub async fn send_message(&mut self, msg: &DaemonMessage) -> Result<(), IpcError> {
        self.sender.send_message(msg).await
    }

    /// Send a client command.
    pub async fn send_command(&mut self, cmd: &ClientCommand) -> Result<(), IpcError> {
        self.sender.send_command(cmd).await
    }

    /// Read and deserialize the next typed message.
    pub async fn recv<M: DeserializeOwned>(&mut self) -> Result<Option<M>, IpcError> {
        self.receiver.recv().await
    }

    /// Receive the next server push message.
    pub async fn recv_message(&mut self) -> Result<Option<DaemonMessage>, IpcError> {
        self.receiver.recv_message().await
    }

    /// Receive the next client command.
    pub async fn recv_command(&mut self) -> Result<Option<ClientCommand>, IpcError> {
        self.receiver.recv_command().await
    }

    /// Read the next raw line string.
    pub async fn recv_raw(&mut self) -> Result<Option<String>, IpcError> {
        self.receiver.recv_raw().await
    }

    /// Send raw bytes followed by newline and flush.
    pub async fn send_raw(&mut self, raw: &[u8]) -> Result<(), IpcError> {
        self.sender.send_raw(raw).await
    }

    /// Flush the writer.
    pub async fn flush(&mut self) -> Result<(), IpcError> {
        self.sender.flush().await
    }

    /// Split this connection into separate sender and receiver halves.
    pub fn split(self) -> (IpcSender<WriteHalf<S>>, IpcReceiver<ReadHalf<S>>) {
        (self.sender, self.receiver)
    }
}
