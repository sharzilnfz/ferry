# 04: Zero-File In-Band Network Pairing via Short Codes

**What to build:** An in-band device pairing flow using 6-word or 6-character ephemeral pairing codes. When a folder is shared, Ferry generates a pairing code and advertises on local mDNS / Iroh relays. The recipient enters the code, and both machines automatically negotiate the cryptographic envelope across a direct QUIC stream without creating or moving payload files.

**Blocked by:** 03: Centralized Multi-Folder Device Daemon & Registry

**Status:** ready-for-agent

- [ ] Short pairing code generation and derivation implemented in `ferry-crypto`.
- [ ] Ephemeral discovery topic advertisement via mDNS on LAN and Iroh relay.
- [ ] In-band cryptographic handshake (Offer -> Response -> Grant) executes across an established QUIC connection.
- [ ] `UiBackend` exposes `create_pairing_session()` and `join_pairing_session()`.
- [ ] End-to-end tests verify zero-file pairing between two local instances.
