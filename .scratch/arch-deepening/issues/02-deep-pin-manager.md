# 02: Deep PinManager State & Ledger Engine

**What to build:**
A deep `PinManager` module in `crates/ferry-pin` that unifies session pin records (`PinStore`), held entry persistence (`HeldLedger`), path glob matching (`PathMatcher`), and release planning (`plan_release`). Daemon status, CLI commands, and sync sessions interact with pin state through a cohesive, single-method interface instead of coordinating five procedural primitives.

**Blocked by:** None (can start immediately)

**Status:** done

- [x] Create `PinManager` struct in `crates/ferry-pin` with `new(state_dir: impl Into<PathBuf>)`.
- [x] Implement `PinManager::summary(&self) -> Result<HeldSummary, PinError>` providing total held counts and deduplicated path collections per peer.
- [x] Implement `PinManager::start_session(&self, paths: Vec<String>, pid: u32, identity: &str) -> Result<PinRecord, PinError>` handling agreement capture and atomic stamp writing.
- [x] Implement `PinManager::release_peer(&self, peer: &[u8; 32], store: &Store, agreed_base: Option<&RootManifest>, local_manifest: &RootManifest) -> Result<ReleasePlan, PinError>`.
- [x] Update call sites in `ferry-daemon/src/state.rs`, `ferry-daemon/src/ui/actions.rs`, and `ferry-cli/src/commands/pin.rs` to use `PinManager`.
- [x] All pin unit and integration tests pass (`cargo test -p ferry-pin`).
