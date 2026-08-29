# 03: Direct PinGate Materialization in ferry-materialize

**What to build:**
Integrate session pin filtering directly into `ferry_materialize::Applier` via a `PinGate` policy, executing atomic withheld-entry logging to `HeldLedger` at the moment of materialization. This closes the fetch-to-apply TOCTOU race condition and allows retiring and deleting `crates/ferry-sync/src/applier.rs` (-284 LOC).

**Blocked by:** 02: Deep PinManager State & Ledger Engine

**Status:** done

- [x] Add `PinGate` trait and configuration to `ferry_materialize::Applier` (e.g. `Applier::with_pin_gate(...)`).
- [x] During `Applier::apply_session_change_set`, evaluate path matches against the active pin gate before disk mutation and append withheld entries to `HeldLedger`.
- [x] Update `ferry-sync/src/exchange.rs` and `ferry-sync/src/engine.rs` to call `ferry_materialize::Applier` directly.
- [x] Delete `crates/ferry-sync/src/applier.rs`.
- [x] Verify ADR-0004 compliance: concurrent peer edits inside active pin scopes are withheld and ledgered without tree mutation.
- [x] All sync and materialization tests pass (`cargo test -p ferry-sync -p ferry-materialize`).
