# ADR-0007: Refuse-by-default peer trust; trust-on-first-use becomes opt-in

Status: accepted (2026-08-30)

## Context

Ferry v0's default peer policy was trust-on-first-use (TOFU): a folder with no
`CONFIG_HEAD` accepted whichever device first proved key possession and pinned
it. That default contradicts ADR-0002's explicit-pairing decision — devices
pair out-of-band by scanning a QR or typing a short code — and the glossary's
definition of pairing as a deliberate ritual. A snapshot-restored or
man-in-the-middle first connection could become the pinned peer before any
human paired anything. The TOFU resolution also lived mid-session in the sync
engine, duplicated across three self-filter walks (two in the engine's session
path, one in the daemon supervisor), where no reviewer could audit it as one
policy.

## Decision

Peer policy defaults to an empty allow-list, which refuses every remote peer.
A folder syncs only with devices whose identities are explicitly paired into
its `CONFIG_HEAD`. The remote-peer derivation (configured device set minus
self) and the expected-peer resolution live once, on `PeerPolicy`, beside its
`CONFIG_HEAD` parser; the three duplicated walks are deleted.

TOFU survives only as an explicit opt-in, in either of two forms:

- `EngineConfig::allow_trust_on_first_use = true`, for folders with no
  `CONFIG_HEAD` (host applications choose this; the shipped CLI and daemon do
  not).
- `SyncEngine::set_peer_policy(PeerPolicy::TrustOnFirstUse)`, the programmatic
  form used by tests.

Opt-in TOFU keeps its original guarantees: the first authenticated peer is
pinned per folder under `.ferry/peers/` and every later session strictly
enforces that pin.

## Consequences

- Two unpaired devices never sync, by default. The pairing ritual (ADR-0006)
  is the only path to a working folder; the empty-allow-list error must point
  at `ferry pair`.
- A restored-from-snapshot folder with no `CONFIG_HEAD` no longer trusts the
  first comer; it refuses instead.
- This strengthens, and does not reopen, ADR-0002: pairing remains out-of-band
  and explicit; only the fallback when pairing data is absent changes.
- Test harnesses that exercise convergence, not trust, set the TOFU opt-in
  explicitly so the refuse default stays observable in the policy tests.

## Verification

- `crates/ferry-sync/tests/peer_policy.rs` asserts the refuse default through
  the engine (unpaired connector fails, no data lands) and the TOFU pin/refuse
  cycle behind the explicit opt-in.
- `cargo test -p ferry-sync -p ferry-daemon` green; clippy clean.
