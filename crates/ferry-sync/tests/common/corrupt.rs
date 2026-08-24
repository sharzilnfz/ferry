//! Test hook: a `Transport` wrapper that flips one byte inside the first
//! post-handshake INBOUND frame it sees, then disarms. Used to prove the
//! receiver verifies transferred bytes and that the engine retries and
//! still converges.
//!
//! T-014 mechanical switch: under protocol v1 the dialer receives exactly
//! two pre-auth frames (HELLO_ACK, AUTH_CONFIRM) before every body region
//! travels SEALED, so "the first ITEM payload" is no longer identifiable
//! from outside the crypto. The equivalent corruption target is the first
//! frame after those two — the peer's first sealed message. Flipping a
//! byte there must fail the AEAD tag check (authentication), never write
//! anything, fail the session, and converge on a later poll.

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Number of pre-auth frames the DIALING side receives before all frames
/// are sealed (HELLO_ACK + AUTH_CONFIRM).
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
            armed: &self.armed as *const AtomicBool as usize,
            seen: 0,
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
    /// Inbound frames seen so far on this connection.
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
        // Corrupt INBOUND post-handshake traffic: with encryption ON the
        // receiver-side AEAD verification is what must catch a flipped
        // byte in transit.
        if self.seen > PREAUTH_INBOUND_FRAMES && self.flag().swap(false, Ordering::SeqCst) {
            let mut bad = frame;
            let flip_at = (bad.len() - 1) / 2; // inside the body region
            bad[flip_at] ^= 0x01;
            return Ok(bad);
        }
        Ok(frame)
    }
}
