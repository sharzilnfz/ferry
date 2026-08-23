# T-008: Encrypted exchange protocol

Status: ready-for-agent
Depends on: T-002, T-003, T-007

Wire protocol over any byte stream: hello/authenticate (device keys), offer
manifests, request missing blobs by hash, stream encrypted chunks, verify
every block hash after decryption before it touches disk. Protocol messages
documented as an extension of `docs/store-format.md`. Version-negotiated from
day one.

Acceptance: T-006's skeleton runs over this protocol with encryption on;
corrupted chunks in transit are detected and rejected, never written.
