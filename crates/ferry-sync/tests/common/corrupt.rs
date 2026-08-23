//! Test hook: a `Transport` wrapper that flips one byte inside the first
//! ITEM frame it sees, then disarms. Used to prove the receiver verifies
//! transferred bytes and that the engine retries and still converges.

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ferry_sync::proto;

/// Wraps any transport. Corruption happens exactly once, on the first frame
/// whose message tag is ITEM, flipping a byte deep in the payload so both
/// length framing and headers stay intact — only content breaks.
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
            armed: &self.armed as *const AtomicBool as usize,
        }))
    }

    fn listen(&self, addr: SocketAddr) -> io::Result<Box<dyn ferry_sync::Listener>> {
        self.inner.listen(addr)
    }
}

struct CorruptingConn {
    inner: Box<dyn ferry_sync::Connection>,
    // Raw pointer back to the transport's armed flag. The fixture outlives
    // every connection in these tests; documented test-only aliasing.
    armed: usize,
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
        // Corrupt INBOUND item payloads: the receiver-side verification is
        // what must catch a flipped byte in transit.
        if !frame.is_empty()
            && frame[0] == proto::tag::ITEM
            && self.flag().swap(false, Ordering::SeqCst)
        {
            let mut bad = frame;
            let flip_at = 1 + (bad.len() - 1) / 2; // inside the payload body
            bad[flip_at] ^= 0x01;
            return Ok(bad);
        }
        Ok(frame)
    }
}
