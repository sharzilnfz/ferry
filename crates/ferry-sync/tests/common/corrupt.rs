use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const PREAUTH_INBOUND_FRAMES: usize = 2;

pub struct CorruptingTransport {
    inner: Arc<dyn ferry_sync::Transport>,
    armed: AtomicBool,
}

impl CorruptingTransport {
    pub fn new(inner: Arc<dyn ferry_sync::Transport>) -> Arc<Self> {
        Arc::new(CorruptingTransport {
            inner,
            armed: AtomicBool::new(true),
        })
    }

    pub fn fired(&self) -> bool {
        !self.armed.load(Ordering::SeqCst)
    }
}

impl ferry_sync::Transport for CorruptingTransport {
    fn dial(&self, addr: SocketAddr) -> io::Result<Box<dyn ferry_sync::Connection>> {
        Ok(Box::new(CorruptingConn {
            inner: self.inner.dial(addr)?,
            armed: &raw const self.armed as usize,
            seen: 0,
        }))
    }

    fn listen(&self, addr: SocketAddr) -> io::Result<Box<dyn ferry_sync::Listener>> {
        self.inner.listen(addr)
    }
}

struct CorruptingConn {
    inner: Box<dyn ferry_sync::Connection>,

    armed: usize,

    seen: usize,
}

impl CorruptingConn {
    fn flag(&self) -> &AtomicBool {
        unsafe { &*(self.armed as *const AtomicBool) }
    }
}

impl ferry_sync::Connection for CorruptingConn {
    fn send_frame(&mut self, payload: &[u8]) -> io::Result<()> {
        self.inner.send_frame(payload)
    }

    fn recv_frame(&mut self) -> io::Result<Vec<u8>> {
        let frame = self.inner.recv_frame()?;
        self.seen += 1;

        if self.seen > PREAUTH_INBOUND_FRAMES && self.flag().swap(false, Ordering::SeqCst) {
            let mut bad = frame;
            let flip_at = (bad.len() - 1) / 2;
            bad[flip_at] ^= 0x01;
            return Ok(bad);
        }
        Ok(frame)
    }
}
