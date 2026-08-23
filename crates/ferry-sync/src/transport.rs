//! Transport seam (M0): byte-frame pipes between two daemons.
//!
//! The trait boundary is the deliverable; `TcpTransport` is the deliberately
//! ugly throwaway implementation — plain blocking localhost TCP, 4-byte
//! little-endian length-prefixed frames, no encryption, no compression, no
//! resume. T-009 replaces the implementation; the engine never sees sockets.
//!
//! Frame limit guards against a hostile or buggy peer allocating us to
//! death. 512 MiB comfortably exceeds the largest legal pack (16 MiB seal
//! target plus overhead).

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};

/// Hard cap on one frame's payload.
pub const MAX_FRAME_BYTES: u32 = 512 * 1024 * 1024;

pub trait Transport: Send + Sync {
    /// Open an outgoing connection to `addr`.
    fn dial(&self, addr: SocketAddr) -> io::Result<Box<dyn Connection>>;
    /// Bind a listener on `addr` (`:0` picks a free port;
    /// [`Listener::local_addr`] reports the choice).
    fn listen(&self, addr: SocketAddr) -> io::Result<Box<dyn Listener>>;
}

pub trait Listener: Send {
    /// The bound address, after port resolution.
    fn local_addr(&self) -> io::Result<SocketAddr>;
    /// Block until a peer connects. Errors are per-accept; callers keep
    /// accepting until shutdown.
    fn accept(&self) -> io::Result<Box<dyn Connection>>;
}

pub trait Connection: Send {
    /// Send exactly one length-prefixed frame.
    fn send_frame(&mut self, payload: &[u8]) -> io::Result<()>;
    /// Receive exactly one frame payload. `Err(UnexpectedEof)` at a frame
    /// boundary means the peer closed cleanly.
    fn recv_frame(&mut self) -> io::Result<Vec<u8>>;
}

/// The M0 throwaway: std-only TCP with hand-rolled framing.
#[derive(Debug, Default, Clone, Copy)]
pub struct TcpTransport;

impl Transport for TcpTransport {
    fn dial(&self, addr: SocketAddr) -> io::Result<Box<dyn Connection>> {
        Ok(Box::new(TcpConn(TcpStream::connect(addr)?)))
    }

    fn listen(&self, addr: SocketAddr) -> io::Result<Box<dyn Listener>> {
        Ok(Box::new(TcpLst(TcpListener::bind(addr)?)))
    }
}

struct TcpLst(TcpListener);

impl Listener for TcpLst {
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.local_addr()
    }

    fn accept(&self) -> io::Result<Box<dyn Connection>> {
        let (stream, _) = self.0.accept()?;
        Ok(Box::new(TcpConn(stream)))
    }
}

struct TcpConn(TcpStream);

/// Treat a boxed connection like a connection itself, so protocol code can
/// pass `&mut Box<dyn Connection>` where `&mut dyn Connection` is expected.
impl Connection for Box<dyn Connection> {
    fn send_frame(&mut self, payload: &[u8]) -> io::Result<()> {
        (**self).send_frame(payload)
    }

    fn recv_frame(&mut self) -> io::Result<Vec<u8>> {
        (**self).recv_frame()
    }
}

impl Connection for TcpConn {
    fn send_frame(&mut self, payload: &[u8]) -> io::Result<()> {
        let len = u32::try_from(payload.len()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "frame exceeds u32 length prefix",
            )
        })?;
        if len > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "frame too large",
            ));
        }
        self.0.write_all(&len.to_le_bytes())?;
        self.0.write_all(payload)?;
        self.0.flush()
    }

    fn recv_frame(&mut self) -> io::Result<Vec<u8>> {
        let mut len_buf = [0u8; 4];
        match self.0.read_exact(&mut len_buf) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(e),
            Err(e) => return Err(e),
        }
        let len = u32::from_le_bytes(len_buf);
        if len > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "frame too large",
            ));
        }
        let mut payload = vec![0u8; len as usize];
        self.0.read_exact(&mut payload)?;
        Ok(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_including_empty_and_multi() {
        let lst = TcpTransport.listen("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = lst.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut c = lst.accept().unwrap();
            let a = c.recv_frame().unwrap();
            let b = c.recv_frame().unwrap();
            c.send_frame(&b).unwrap();
            c.send_frame(&a).unwrap();
            // Peer closes; next read is a clean EOF error.
            assert!(c.recv_frame().is_err());
        });
        let mut cli = TcpTransport.dial(addr).unwrap();
        cli.send_frame(b"first").unwrap();
        cli.send_frame(&[]).unwrap();
        assert_eq!(cli.recv_frame().unwrap(), b"");
        assert_eq!(cli.recv_frame().unwrap(), b"first");
        drop(cli); // closes the socket
        server.join().unwrap();
    }

    #[test]
    fn oversized_length_prefix_is_rejected_without_allocating() {
        let lst = TcpTransport.listen("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = lst.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut c = lst.accept().unwrap();
            let err = c.recv_frame().unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        });
        // Raw client bypasses the trait to emit a hostile prefix.
        let mut raw = std::net::TcpStream::connect(addr).unwrap();
        raw.write_all(&(u32::MAX - 1).to_le_bytes()).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn dial_refuses_unreachable_addresses_promptly() {
        // Port 1 on loopback is closed by default; connect must error.
        let res = TcpTransport.dial("127.0.0.1:1".parse().unwrap());
        match res {
            Err(e) => assert!(matches!(e.kind(), io::ErrorKind::ConnectionRefused)),
            Ok(_) => panic!("dial to a closed port must fail"),
        }
    }
}
