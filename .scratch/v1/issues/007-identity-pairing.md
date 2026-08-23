# T-007: Device identity, pairing, key wrapping

Status: ready-for-agent
Depends on: T-001

Device keypair generation and persistent identity; folder key generation;
pairing flow (short code + QR payload containing public keys, out-of-band
verified); age-style wrapping of folder keys to each device's X25519 public
key; passphrase-wrapped key export for disaster recovery. Per ADR-0002.

Acceptance: two devices pair via exchanged codes; both can unwrap a folder
key; a third device cannot; exported key restores access on a wiped device.
