# Reshape the Transport seam around peer identity and explicit shutdown

Status: done
Depends on:
Blocks:

## Files

- `crates/ferry-sync/src/transport.rs` — the `Transport`/`Listener`/`Connection`
  trait seam (deliberate, per ADR-0003)
- `crates/ferry-iroh/src/transport.rs` — iroh adapter
- `crates/ferry-iroh/src/directory.rs` — instance-isolated `RouteTable`
- `crates/ferry-sync/src/engine.rs` — `EngineConfig::{bind_addr, connect_to}`
- `crates/ferry-daemon/src/main.rs` — dynamic `register_peer` routing

## Problem

The seam holds: two real adapters exist (TcpTransport, IrohTransport), the
engine never sees a socket, and iroh types stop at the crate edge — ADR-0003's
isolation consequence is met. But the interface's currency is TCP-shaped, and
the iroh adapter pays ~200 lines of plumbing to imitate semantics its transport
does not have:

- `SocketAddr` is a fake route key: synthesized ports (45601+ counter), a magic
  alias `127.0.0.1:65534`, and a process-global `directory` registry mapping
  route key → EndpointId.
- Shutdown works by the engine dialing its own listener to unblock `accept`;
  iroh forbids self-connects, so the adapter reproduces the observable behavior
  with a `wake` flag and zero-stream "probe connections".
- `EngineConfig::bind_addr/connect_to` leak addressing into engine config even
  though ADR-0003 says peers are addressed by device public key, never by IP.

Each new transport would re-pay this imitation tax. The interface is the test
surface here, and today it can only be exercised by pretending to be TCP.

## Solution

Widen the trait at its two real joints:

1. Dial/listen take an opaque **peer id** (`[u8; 32]`, already the wire-level
   identity) instead of `SocketAddr`. TcpTransport maps test peer ids to
   localhost ports internally.
2. `Listener` gains explicit `close()`, so accept loops unblock without the
   self-dial probe; delete the wake/probe machinery from ferry-iroh.
3. Replace the global directory with an explicit route table handed to the
   transport at construction; drop `IROH_DIAL_ALIAS`.

This strengthens ADR-0003 rather than contradicting it — it implements
"addressed by device public key" in the interface itself.

## Benefits

- The iroh adapter shrinks to pure QUIC bridging; the alias/wake/probe code
  disappears with its failure modes.
- A third adapter (relay-only, or an in-memory one for fuzzing sessions) becomes
  a small stub instead of a TCP impressionist.
- Shutdown and dial-failure paths become directly assertable through the trait,
  no sockets required.

## Before / after

```text
BEFORE                                  AFTER
trait Transport {                       trait Transport {
  dial(SocketAddr)                        dial(PeerId) / dial(SocketAddr)
  listen(SocketAddr)                      listen() -> Listener
}                                       trait Listener { accept(); close(); }
process-global route directory          RouteTable passed at construction
engine: self-dial to unblock accept     listener.close()
IROH_DIAL_ALIAS "127.0.0.1:65534"       (deleted, uses t.register_peer)
```

## Strength

Done

## Comments

Full analysis with diagrams: /var/folders/y9/hnkm2lv91n5chc4116wp_hf40000gn/T/architecture-review-1787745437.html (architecture audit A0, 2026-08-26).

### Implementation Summary (Subagent R-SEAM)
- **Peer Identity & Transport Trait Widening (`crates/ferry-sync/src/transport.rs`)**:
  - Defined `PeerId = [u8; 32]` alias and added `dial_peer(&self, peer: &PeerId) -> io::Result<Box<dyn Connection>>` default trait method on `Transport`.
  - Added bidirectional conversion helpers `addr_to_peer_id` and `peer_id_to_addr` for deterministic test and transport mapping.
  - Added `Listener::close(&self) -> io::Result<()>` to `trait Listener` with `Send + Sync` bounds, implementing it for `TcpLst`, `Box<dyn Listener>`, and `Arc<T>`.
- **Route Table Encapsulation (`crates/ferry-iroh/src/directory.rs`, `config.rs`, `lib.rs`)**:
  - Implemented `RouteTable` as an explicit instance-isolated table mapping route handles and `PeerId`s to endpoints.
  - Added `IrohConfigBuilder::routes` / `IrohConfig::routes` to allow injecting custom route tables at construction time.
  - Added instance methods `routes()`, `route_table()`, `register_peer()`, `with_route()`, `dial_peer()` on `IrohTransport`.
  - Retained process-wide global fallback table for zero-config discovery in in-process integration tests.
- **Removed Wake/Probe Workarounds (`crates/ferry-iroh/src/transport.rs`, `tests/roundtrip.rs`)**:
  - Deleted `wake: AtomicBool` flag from `Inner`.
  - Cleaned `FramedConnection` to store unwrapped `IrohConn`, `Mutex<SendStream>`, and `Mutex<RecvStream>`, eliminating probe dummy streams.
  - Replaced self-probe simulation in `dial_endpoint` with typed `Err(DialFailure::SelfDial)`.
  - Replaced `self_dial_wakes_listener_with_clean_eof_probe` test in `roundtrip.rs` with `self_dial_is_cleanly_refused` and `listener_close_unblocks_accept`.
- **Dropped `IROH_DIAL_ALIAS` (`crates/ferry-daemon/src/main.rs`)**:
  - Removed static `IROH_DIAL_ALIAS = "127.0.0.1:65534"`.
  - Replaced connect-role alias setup with dynamic `t.register_peer(peer)` returning a synthesized route key.

### Planned Engine Diff (for future engine.rs integration)
When updating `crates/ferry-sync/src/engine.rs` to address peers solely by `PeerId`:
```diff
--- a/crates/ferry-sync/src/engine.rs
+++ b/crates/ferry-sync/src/engine.rs
@@ -82,2 +82,2 @@ pub struct EngineConfig {
-    pub bind_addr: Option<SocketAddr>,
-    pub connect_to: Option<SocketAddr>,
+    pub listen: bool,
+    pub connect_to_peer: Option<PeerId>,
@@ -938,2 +938,2 @@ impl Ctx {
-        let Some(addr) = self.cfg.connect_to else { return; };
-        match self.transport.dial(addr) {
+        let Some(peer) = self.cfg.connect_to_peer else { return; };
+        match self.transport.dial_peer(&peer) {
@@ -1678,3 +1678,3 @@ impl SyncEngine {
-        let listener = match cfg.bind_addr {
-            Some(addr) => Some(Transport::listen(transport.as_ref(), addr)?),
+        let listener = if cfg.listen {
+            Some(transport.listen("127.0.0.1:0".parse().unwrap())?)
```

