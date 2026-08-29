# 04: Noise Handshake Encapsulation into SecureSession

**What to build:**
A deep `SecureSession` state machine in `crates/ferry-proto/src/secure.rs` that encapsulates Diffie-Hellman exchanges, transcript hashing, auth proof sealing, and AEAD frame encryption/decryption behind a single `SecureSession::establish(stream, role, identity, expected_peer)` interface. This strips ~200 lines of procedural crypto-plumbing out of the protocol conversation engine.

**Blocked by:** None (can start immediately)

**Status:** done

- [x] Create `SecureSession<S: ByteStream>` in `crates/ferry-proto/src/secure.rs` wrapping I/O stream, negotiated version, peer identity, and directional ciphers.
- [x] Implement `SecureSession::establish` executing the 7-step Noise handshake and returning the authenticated session.
- [x] Implement `send_frame` and `recv_frame` methods directly on `SecureSession` managing frame counters and AEAD encryption.
- [x] Refactor `crates/ferry-proto/src/engine.rs` to initialize `SecureSession::establish` and operate entirely over authenticated frames.
- [x] Unit-test `SecureSession` handshake failures (identity mismatch, corrupt auth tag, bad version) independently.
- [x] Protocol test suite passes (`cargo test -p ferry-proto`).
