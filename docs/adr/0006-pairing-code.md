# ADR-0006: Short pairing code format — 6-char base32 with checksum

Status: accepted (2026-08-28)

## Context

Seamless onboarding needs a short rendezvous code typed by humans. Two options were considered:

- 6-word BIP39-like (11 bits per word, 2048-word list, 66 bits total)
- 6-char base32 with checksum (5 bits per char, 30 bits total)

Both satisfy the product requirement of under-ten-second entry. Wave 1 tickets run in parallel and must avoid shared wordlist vendoring.

## Decision

Choose 6-char base32 with checksum.

- Alphabet is the crate's canonical base32 (`23456789ABCDEFGHJKLMNPQRSTUVWXYZ`, no `0/O/1/I`).
- Generation: 5 random base32 symbols (25 bits entropy via `rand 0.8`) plus 1 checksum symbol derived as `CRC32(first-5-bytes) % 32` mapped through the same alphabet. Total 6 chars, 30 bits on wire.
- Verification: recompute checksum from first 5 chars, constant-time compare (`subtle 2.6.1 ct_eq`) against the 6th, then constant-time equality of the full 6-char string against stored code. Any single-symbol substitution fails either the checksum or the final equality. Lookalikes `0/1/I/O` are rejected on input, not guessed.
- Entropy: 25 bits data entropy gives about 33M possibilities. For 1000 generated codes the birthday collision expectation is near 0.015, so the >900 unique test passes reliably while keeping the code short. The underlying RNG is 128-bit (`OsRng`/`StdRng` backed) and the truncation is intentional for human length; brute-force is throttled by the 24h expiry and rendezvous rate limits in Wave 3, not by code length alone.
- Expiry: `PairingCode` stores `expires_at: SystemTime` set to `now + 24h` at generation. `is_expired(now)` takes the caller's `SystemTime` and compares with `duration_since`; no `SystemTime::now()` inside the check. This keeps tests deterministic and avoids hidden clock dependency.
- Zeroize: the code string is held in `zeroize::Zeroizing<String>` and `Debug` is redacted.

Rejected 6-word wordlist: more user-friendly for dictation but requires vendoring a 2048-entry static list, larger UI footprint, and word-boundary ambiguity (hyphens vs spaces). The deferred wordlist can be added as an alternative encoding without breaking the current 6-char verifier: both would share the same `PairingCode` newtype behind a feature flag.

## Amendment (2026-08-29): mechanics moved behind the pairing ritual

The deep-architecture consolidation moved the code mechanics out of the public `ferry-crypto::pairing_code` module into the unified `PairingRitual` (`ferry-folder::pairing`, private to the ritual). Every guarantee above is unchanged — same format, same CRC32 checksum, same constant-time verification (`subtle::ct_eq`), same 24h expiry, same zeroization — but the ritual is now the only way to mint or answer a code: there is no parallel public code workflow callers could use instead of `create_offer` / `accept_offer`. On the accept path, alphabet + checksum verification runs before the rendezvous lookup; final equality is enforced by the lookup's exact match. ferry-crypto keeps the raw primitives (base32 alphabet, CRC-32) only.

## Amendment (2026-09-01): Network Rendezvous Discovery & Mutual CONFIG_HEAD Commit

The pairing workflow integrates with transport-level network rendezvous (UDP broadcast/multicast and P2P rendezvous topics derived from the 6-character code). Upon completion of the key exchange over the network, both the sharer and joiner atomically wrap the Folder Master Key for each other's device public key and commit the updated allow-list into `CONFIG_HEAD`, allowing subsequent background sync sessions to authorize without manual intervention.

## Consequences

- No wordlist asset, no extra dependency.
- Codes are 6 uppercase chars, trivial to type on mobile and desktop.
- Single-character typo detection via 5-bit CRC-derived checksum; full equality still enforces a constant-time compare so timing does not leak prefix match length.
- Future work can add a 6-word alias as an alternative `Display` while reusing the same expiry and zeroize guarantees.

## Verification

- `cargo test -p ferry-folder` includes `pairing_code_tests` (ritual-level ports of the old `pairing_code_tests` vectors: format + checksum recompute over 1000 codes, checksum-flip refusal, case/hyphen tolerance, 24h TTL, constant-time/zeroize source scan) and `ritual.rs` for round-trip, wrong-code, expiry-consumption, and one-time use.
- `cargo clippy --workspace --all-targets` clean.
- Grep for `==` inside `verify` returns zero; `ct_eq` is present.
