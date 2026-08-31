










use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;


pub const MAX_FRAME_BYTES: u32 = 512 * 1024 * 1024;


pub type PeerId = [u8; 32];

pub trait Transport: Send + Sync {
    
    fn dial(&self, addr: SocketAddr) -> io::Result<Box<dyn Connection>>;
    
    
    fn listen(&self, addr: SocketAddr) -> io::Result<Box<dyn Listener>>;

    
    fn dial_peer(&self, peer: &PeerId) -> io::Result<Box<dyn Connection>> {
        self.dial(peer_id_to_addr(peer))
    }
}

pub trait Listener: Send + Sync {
    
    fn local_addr(&self) -> io::Result<SocketAddr>;
    
    
    fn accept(&self) -> io::Result<Box<dyn Connection>>;
    
    fn close(&self) -> io::Result<()> {
        Ok(())
    }
}

pub trait Connection: Send {
    
    fn send_frame(&mut self, payload: &[u8]) -> io::Result<()>;
    
    
    fn recv_frame(&mut self) -> io::Result<Vec<u8>>;
}


pub fn addr_to_peer_id(addr: &SocketAddr) -> PeerId {
    let mut out = [0u8; 32];
    match addr {
        SocketAddr::V4(v4) => {
            out[0..4].copy_from_slice(&v4.ip().octets());
            out[4..6].copy_from_slice(&v4.port().to_be_bytes());
        }
        SocketAddr::V6(v6) => {
            out[0..16].copy_from_slice(&v6.ip().octets());
            out[16..18].copy_from_slice(&v6.port().to_be_bytes());
        }
    }
    out
}


pub fn peer_id_to_addr(peer: &PeerId) -> SocketAddr {
    let ip = std::net::Ipv4Addr::new(peer[0], peer[1], peer[2], peer[3]);
    let port = u16::from_be_bytes([peer[4], peer[5]]);
    SocketAddr::V4(std::net::SocketAddrV4::new(ip, port))
}


#[derive(Debug, Default, Clone, Copy)]
pub struct TcpTransport;

impl Transport for TcpTransport {
    fn dial(&self, addr: SocketAddr) -> io::Result<Box<dyn Connection>> {
        let stream = TcpStream::connect(addr)?;
        
        
        
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
        let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(5)));
        Ok(Box::new(TcpConn(stream)))
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
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
        let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(5)));
        Ok(Box::new(TcpConn(stream)))
    }

    fn close(&self) -> io::Result<()> {
        if let Ok(addr) = self.0.local_addr() {
            let _ = TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(50));
        }
        Ok(())
    }
}

impl Listener for Box<dyn Listener> {
    fn local_addr(&self) -> io::Result<SocketAddr> {
        (**self).local_addr()
    }

    fn accept(&self) -> io::Result<Box<dyn Connection>> {
        (**self).accept()
    }

    fn close(&self) -> io::Result<()> {
        (**self).close()
    }
}

impl<T: ?Sized + Listener + Send + Sync> Listener for Arc<T> {
    fn local_addr(&self) -> io::Result<SocketAddr> {
        (**self).local_addr()
    }

    fn accept(&self) -> io::Result<Box<dyn Connection>> {
        (**self).accept()
    }

    fn close(&self) -> io::Result<()> {
        (**self).close()
    }
}

struct TcpConn(TcpStream);



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
            
            assert!(c.recv_frame().is_err());
        });
        let mut cli = TcpTransport.dial(addr).unwrap();
        cli.send_frame(b"first").unwrap();
        cli.send_frame(&[]).unwrap();
        assert_eq!(cli.recv_frame().unwrap(), b"");
        assert_eq!(cli.recv_frame().unwrap(), b"first");
        drop(cli); 
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
        
        let mut raw = std::net::TcpStream::connect(addr).unwrap();
        raw.write_all(&(u32::MAX - 1).to_le_bytes()).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn dial_refuses_unreachable_addresses_promptly() {
        
        let res = TcpTransport.dial("127.0.0.1:1".parse().unwrap());
        match res {
            Err(e) => assert!(matches!(e.kind(), io::ErrorKind::ConnectionRefused)),
            Ok(_) => panic!("dial to a closed port must fail"),
        }
    }

    #[test]
    fn listener_close_unblocks_cleanly() {
        let lst = TcpTransport.listen("127.0.0.1:0".parse().unwrap()).unwrap();
        assert!(lst.close().is_ok());
    }

    #[test]
    fn peer_id_conversion_round_trips() {
        let addr: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let peer = addr_to_peer_id(&addr);
        let back = peer_id_to_addr(&peer);
        assert_eq!(addr, back);
    }
}
