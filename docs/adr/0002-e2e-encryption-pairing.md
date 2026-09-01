# ADR-0002: End-to-end encryption by default, per-folder keys, explicit pairing

Status: accepted (2026-08-23)

## Context

The tool will carry `.env` files, credentials, agent state, and proprietary
code. Research shows ~65% of leaked secrets live in env files and attackers
mass-scan for them. "Trust the transport" is not enough; any relay or future
hosted component must be unable to read user data. Competing tools either skip
E2E (Unison, Mutagen, rsync) or are closed about their crypto (Resilio,
Bowline).

## Decision

- Every blob and manifest is encrypted client-side before it ever touches the
  network or another disk. AEAD with ChaCha20-Poly1305 (or AES-256-GCM) in an
  age-STREAM-like chunked construction.
- Keys are per-folder symmetric keys (Folder Master Key / FMK), generated
  locally, wrapped to each paired device's public key (X25519, age-style
  envelopes).
- Devices pair out-of-band: QR code or short code exchange, no account, no
  password reset story that could become a recovery backdoor. Losing all
  devices loses the data; document this loudly.
- Relays see ciphertext and metadata only.

## Amendment (2026-08-30): CONFIG_HEAD Commit & Noise Secure Sessions

Folder keys and authorized device memberships are cryptographically committed
into `CONFIG_HEAD` (`.ferry/config`), forming an immutable signed lineage.
Transport sessions perform mutual cryptographic authentication using Noise
handshakes (Noise_XX / IK patterns) over QUIC streams, guaranteeing that
unauthorized peers cannot initiate sync or observe plaintext manifests even on
open local networks.

## Consequences

- No server-side search, web preview, or sharing-with-a-link features. Accepted.
- Key backup becomes a UX problem (passphrase-wrapped key export, Tether-style).
- Chunk-size leakage is a known side channel; handled separately in ADR-0005.
