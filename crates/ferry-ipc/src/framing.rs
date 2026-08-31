

use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf, WriteHalf,
};

use crate::error::IpcError;
use crate::protocol::{ClientCommand, DaemonMessage};


pub const DEFAULT_MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;


pub struct IpcSender<W> {
    writer: W,
}

impl<W: AsyncWrite + Unpin> IpcSender<W> {
    
    pub fn new(writer: W) -> Self {
        Self { writer }
    }

    
    pub async fn send<M: Serialize>(&mut self, msg: &M) -> Result<(), IpcError> {
        let mut bytes = serde_json::to_vec(msg).map_err(IpcError::Serialization)?;
        bytes.push(b'\n');
        self.writer.write_all(&bytes).await?;
        self.writer.flush().await?;
        Ok(())
    }

    
    pub async fn send_message(&mut self, msg: &DaemonMessage) -> Result<(), IpcError> {
        self.send(msg).await
    }

    
    pub async fn send_command(&mut self, cmd: &ClientCommand) -> Result<(), IpcError> {
        self.send(cmd).await
    }

    
    pub async fn send_raw(&mut self, raw: &[u8]) -> Result<(), IpcError> {
        self.writer.write_all(raw).await?;
        if !raw.ends_with(b"\n") {
            self.writer.write_all(b"\n").await?;
        }
        self.writer.flush().await?;
        Ok(())
    }

    
    pub async fn flush(&mut self) -> Result<(), IpcError> {
        self.writer.flush().await?;
        Ok(())
    }

    
    pub fn into_inner(self) -> W {
        self.writer
    }
}


pub struct IpcReceiver<R> {
    reader: BufReader<R>,
    max_message_size: usize,
}

impl<R: AsyncRead + Unpin> IpcReceiver<R> {
    
    pub fn new(reader: R) -> Self {
        Self::with_max_message_size(reader, DEFAULT_MAX_MESSAGE_SIZE)
    }

    
    pub fn with_max_message_size(reader: R, max_message_size: usize) -> Self {
        Self {
            reader: BufReader::new(reader),
            max_message_size,
        }
    }

    
    
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
                
                continue;
            }
            return Ok(Some(trimmed.to_string()));
        }
    }

    
    
    pub async fn recv<M: DeserializeOwned>(&mut self) -> Result<Option<M>, IpcError> {
        match self.recv_raw().await? {
            Some(raw) => {
                let msg: M = serde_json::from_str(&raw).map_err(IpcError::Deserialization)?;
                Ok(Some(msg))
            }
            None => Ok(None),
        }
    }

    
    pub async fn recv_message(&mut self) -> Result<Option<DaemonMessage>, IpcError> {
        self.recv().await
    }

    
    pub async fn recv_command(&mut self) -> Result<Option<ClientCommand>, IpcError> {
        self.recv().await
    }

    
    pub fn into_inner(self) -> R {
        self.reader.into_inner()
    }
}


pub struct IpcConnection<S> {
    sender: IpcSender<WriteHalf<S>>,
    receiver: IpcReceiver<ReadHalf<S>>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> IpcConnection<S> {
    
    pub fn new(stream: S) -> Self {
        Self::with_max_message_size(stream, DEFAULT_MAX_MESSAGE_SIZE)
    }

    
    pub fn with_max_message_size(stream: S, max_message_size: usize) -> Self {
        let (read_half, write_half) = tokio::io::split(stream);
        Self {
            sender: IpcSender::new(write_half),
            receiver: IpcReceiver::with_max_message_size(read_half, max_message_size),
        }
    }

    
    pub async fn send<M: Serialize>(&mut self, msg: &M) -> Result<(), IpcError> {
        self.sender.send(msg).await
    }

    
    pub async fn send_message(&mut self, msg: &DaemonMessage) -> Result<(), IpcError> {
        self.sender.send_message(msg).await
    }

    
    pub async fn send_command(&mut self, cmd: &ClientCommand) -> Result<(), IpcError> {
        self.sender.send_command(cmd).await
    }

    
    pub async fn recv<M: DeserializeOwned>(&mut self) -> Result<Option<M>, IpcError> {
        self.receiver.recv().await
    }

    
    pub async fn recv_message(&mut self) -> Result<Option<DaemonMessage>, IpcError> {
        self.receiver.recv_message().await
    }

    
    pub async fn recv_command(&mut self) -> Result<Option<ClientCommand>, IpcError> {
        self.receiver.recv_command().await
    }

    
    pub async fn recv_raw(&mut self) -> Result<Option<String>, IpcError> {
        self.receiver.recv_raw().await
    }

    
    pub async fn send_raw(&mut self, raw: &[u8]) -> Result<(), IpcError> {
        self.sender.send_raw(raw).await
    }

    
    pub async fn flush(&mut self) -> Result<(), IpcError> {
        self.sender.flush().await
    }

    
    pub fn split(self) -> (IpcSender<WriteHalf<S>>, IpcReceiver<ReadHalf<S>>) {
        (self.sender, self.receiver)
    }
}
