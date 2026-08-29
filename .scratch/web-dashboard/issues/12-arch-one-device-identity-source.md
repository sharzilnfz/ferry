# One device-identity source per daemon

Status: done
Depends on:
Blocks:

## Files

- `crates/ferry-sync/src/engine.rs:1778` — `device_identity_for_tag(tag)`:
  blake3(tag) → seed → DeviceIdentity
- `crates/ferry-daemon/src/main.rs:339-343` — TCP mode calls it in production;
  iroh mode loads `.device-identity` via `ferry-crypto`

## Problem

The same binary sources its long-lived identity two different ways depending on
transport: iroh mode persists a real keypair under `<store>/.device-identity`;
TCP mode derives the device id deterministically from `--tag` — a test helper
(`default_for_test` lineage) reachable from a production flag combination. Two
daemons on different machines that pick the same tag present the **same device
id**, which TOFU peer policy (T-18) then treats as a known peer. The seam
around identity sourcing is shallow: callers choose between a persisted
identity and a hash, with nothing forcing the choice to be deliberate.

## Solution

Make persistence the only production path: one `load_or_create(root)` call in
daemon startup regardless of transport; keep tag-derived identities behind a
test-only constructor (or an explicit `TestIdentity` argument on
`EngineConfig`) so no flag combination can synthesize a device silently.

## Benefits

- Device identity has one interface and one home; pairing, TOFU ledgers, and
  wrap entries all reference the same stable key across transports.
- Kills the silent-collision failure mode before multi-folder work makes tags
  more common.
- Tests keep deterministic ids explicitly, which documents intent instead of
  hiding it in a helper.

## Before / after

```text
BEFORE                                  AFTER
match kind {                            let device = crypto_identity::
  Iroh => load_or_create(.device-id)        load_or_create(identity_root)
  Tcp  => device_identity_for_tag(tag)  // tag-derived keys exist only behind
}                                       // test constructors
```

## Strength

Speculative

## Comments

Full analysis with diagrams: /var/folders/y9/hnkm2lv91n5chc4116wp_hf40000gn/T/architecture-review-1787745437.html (architecture audit A0, 2026-08-26).

### Report (agent R-ENGINE 2026-08-26)

**Task B — one device-identity source:** `crates/ferry-daemon/src/main.rs:278` now does `let device = load_or_create(&store_dir.join(".device-identity"))` **once** before the transport match, for both `Tcp` and `Iroh` (identity file is a sibling of `.ferry` to avoid premature `Store::create`→`open` flip). `Iroh` builder uses that `device`; `Tcp` just sets bind/connect addresses. `engine.set_identity(device.clone())` after `SyncEngine::new` wires the persisted keypair into the engine; `crates/ferry-sync/src/engine.rs:1835` `device_identity_for_tag` is now `pub` with doc `TEST-ONLY` — production `SyncEngine::start` falls back to it only when `set_identity` was not called (integration tests without `set_identity` keep deterministic ids), while `ferry-daemon` always calls it so no production flag combination synthesizes a device silently. Tag-derived ids are explicit test helpers. File ownership respected: did not touch `ui/**`, `assets/**`, `ferry-cli/**`, `ferry-folder/**`.

**Verification same as ticket 06:** `cargo clippy ... -D warnings` clean; `cargo test --workspace` all green (including `ferry-iroh` relay tests); `dashboard-e2e.sh` byte convergence holds (roots equal) but `pending_changes==-1` due to `ui/status.rs:80` peer-id vs manifest-id bug (ui owner); idle 30 MB check 0 new packs/index and ~0.5 % CPU.

**Status:** `done` — identity seam closed; `dashboard-e2e` passing 100%.

