# T-18: Peer authorization policy — TOFU result persisted and enforced

Status: ready-for-agent
Depends on: T-07 (folder-pointer/engine state settled)

Audit finding (High): the shipped daemon constructs
`EngineConfig { expected_peer_id: None, ... }`
(crates/ferry-daemon/src/main.rs ~229-231), which maps to
`ExpectPeer::TrustOnFirstUse`; `check_identity`
(crates/ferry-sync/src/session.rs ~309-333) accepts whichever DeviceId proves
possession of its own key. The doc promises "reports it so the caller can pin
it for next time," but no caller does: `est.peer` feeds status lines and
agreement records only. There is no authorization gate between handshake and
serve/pull — anyone who learns the endpoint id can pull decrypted chunks and
push adoptable manifests.

Fix (stay std-threaded, no new deps):
1. Persist first-seen peer identity per folder (e.g. `.ferry/peers/<hex>` or
   beside the agreement ledger — follow existing storage conventions) on TOFU
   accept, and refuse mismatches loudly on later sessions.
2. Allow-list mode: EngineConfig gains an explicit peer policy seeded from the
   folder's CONFIG_HEAD wrap entries where available; deny-unknown by default
   once seeded.
3. New-device events surface visibly (status/report path), never silently
   sync.
4. Deterministic tests: first connect pins identity; second connect with a
   DIFFERENT keypair is refused with a typed error; same keypair proceeds;
   allow-list pre-seed skips TOFU.

Acceptance: tests above green through the ENGINE public API (not CLI loops);
pairing/e2e suites still green; behavior documented in the engine config docs
where ExpectPeer is described.
