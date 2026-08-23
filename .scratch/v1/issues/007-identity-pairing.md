# T-007: Device identity, pairing, key wrapping

Status: done
Depends on: T-001

Device keypair generation and persistent identity; folder key generation;
pairing flow (short code + QR payload containing public keys, out-of-band
verified); age-style wrapping of folder keys to each device's X25519 public
key; passphrase-wrapped key export for disaster recovery. Per ADR-0002.

Acceptance: two devices pair via exchanged codes; both can unwrap a folder
key; a third device cannot; exported key restores access on a wiped device.

## Comments

**Landed as `crates/ferry-crypto`** (8 commits on `ticket/T-007`, worktree
`.worktrees/T-007`). 54 tests in the crate + 3 acceptance tests over the
public API (`tests/acceptance.rs`); workspace total 127 passing,
clippy `--all-targets` zero warnings, fmt clean.

### Public API surface

- `identity`: `DeviceIdentity` (generate / from_secret_bytes / load-or-create
  at an injectable root, default `~/.ferry/identity/device.key`, file 0600 +
  dir 0700), `device_id()` = raw X25519 public key per the manifest schema.
  Corrupted identity files are loud `IdentityError::Corrupted` — never
  regenerated (silent new keys would silently fork trust). `import_identity`
  restores on a wiped device and refuses to clobber existing identities.
- `folder_key`: `generate_fmk` (32 CSPRNG bytes); normative wrap envelope —
  HKDF-SHA-256(ikm=shared, salt=ephemeral_pub||device_pub,
  info="ferry/v1/keywrap"), ChaCha20-Poly1305, 12 zero-byte nonce,
  aad=folder_id, `wrapped_len == 80` enforced by type. RNG-injectable core
  for pinned vectors.
- `config_head`: `write_config_head` / `parse_config_head` matching
  `docs/store-format.md` byte-for-byte (magic FERRY, kind 0x04, version 1,
  folder_id, reserved zeros, wrapped-key list); rejects wrong kind, nonzero
  reserved, `wrapped_len != 80`.
- `pack_cipher`: `ChaChaCipher` implementing ferry-store's `PackCipher`
  (RFC 8439 ChaCha20-Poly1305). **No ferry-store changes were needed**: its
  `Store::create/open` already accept any `Box<dyn PackCipher>`, so T-008's
  seam swap is literally changing the constructor argument. Interop test
  proves PassthroughCipher vs ChaChaCipher framing lengths are identical for
  identical inputs (whole-pack geometry through `seal_pack_bytes`),
  so the swap is format-neutral.
- `pairing`: versioned LE offer payload (93B: "FRPO", ver 1, folder_id,
  initiator pub, one-time secret, created_sec) == QR content (no second
  framing layer; layout documented in crate docs). Response ("FRPR", 77B)
  carries responder pub + HMAC-SHA256 over transcript
  `"ferry/v1/pairing/confirm" || offer_bytes || responder_pub` keyed by the
  one-time secret. `complete_pairing` verifies then wraps the FMK to BOTH
  device pubs via the envelope.
- Short codes: `XXXX-XXXX-XXXX-XXXX-XXXX` over canonical base32
  alphabet `23456789ABCDEFGHJKLMNPQRSTUVWXYZ` (no 0/O/1/I). Encodes hints
  u16 + BLAKE3(offer)[..8] + CRC-32 truncated to high 16 bits.
- `recovery`: passphrase export ("FRRX", 81→113B envelope: ver || salt16 ||
  nonce12 || AEAD ct of fmk||device_secret), Argon2id KDF.

### Decisions recorded

- **Argon2id params: m=19456 KiB, t=2, p=1, L=32** — OWASP cheat-sheet's
  second recommended config; fixed by v1 rather than stored in the envelope
  (changing them is a version bump). Chosen over the m=46MiB tier to stay
  usable on the ARM boards Ferry targets.
- **Short-code checksum: CRC-32/IEEE truncated to high 16 bits**, not a
  BLAKE3 prefix — the threat is typos not adversaries (authenticity is the
  MAC's job); CRC burst-error detection matches the typo model and any
  independent implementation can reproduce it from the comment. Every
  single-symbol substitution is rejected by test; decoders refuse lookalike
  chars instead of guessing substitutions.
- **One-time secret rides inside the scanned payload** (the physical QR /
  code exchange IS the authorization channel). Pre-completion traffic
  carries no wrapped keys; post-completion wraps authenticate under
  X25519+HKDF against static secrets. Network interception of the offer
  yields nothing decryptable.
- **Ephemeral wrap scalars use StaticSecret** (static_secrets feature)
  rather than EphemeralSecret — same construction, avoids dalek 2.x API
  naming drift between point releases; zeroize-on-drop either way.
- Degenerate (small-order) peer keys rejected via `was_contributory()`
  before any KDF sees a shared secret.

### Verification highlights

- Wrap envelope pinned byte-for-byte AND independently reproduced by a
  pure-Python RFC 5869 + RFC 8439 reference (itself validated against RFC
  8439 §2.8.2) — see `deterministic_rng_pins_full_envelope_bytes`.
- HKDF schedule pinned against Python-computed value over RFC 7748 §6.1 DH
  vector; identity pins §6.1 end-to-end incl. clamping.
- Acceptance scenarios verbatim: two devices pair via exchanged codes and
  both unwrap the same FMK; third device fails MAC verification with a wrong
  secret, cannot open any envelope addressed to confirmed peers, tampered
  envelopes fail authentication; export → wipe → import on fresh dir →
  original FMK restored, wrong passphrase fails cleanly, corrupted backup
  fails loudly.

### License notes (deviation from "all MIT/Apache preferred")

Permissive everywhere, three exceptions to the preference:
`x25519-dalek 2.0.1` + `curve25519-dalek 4.1.3` + `subtle 2.6.1` are
**BSD-3-Clause** (the canonical Rust X25519 stack, ticket-mandated crate);
`blake3 1.8.7` is CC0/Apache-2.0 (already a workspace dep via ferry-store).
Everything else MIT OR Apache-2.0: chacha20poly1305 0.10.1, hkdf 0.12.4,
sha2 0.10.9, hmac 0.12.1, rand 0.8.7, argon2 0.5.3, zeroize 1.9.0,
qrcode 0.14.1, thiserror 2. Shared versions match ferry-store's lockfile.
