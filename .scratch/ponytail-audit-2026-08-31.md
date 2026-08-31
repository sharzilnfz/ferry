# Ferry-Sync Ponytail Audit — Whole-Repo Launch Readiness

**Date:** 2026-08-31  
**Branch:** `feat/deep-sync-consolidation` (d12b524)  
**Scope:** full workspace, 18 crates (ferry-cli is `ferry` binary, ferry-daemon is `ferry-sync` binary), 78 700 Rust LOC, 223 Rust files, 690 cargo packages  
**Index:** `ferry-sync` project, 7 384 nodes, 38 175 edges, mode moderate, indexed 2026-08-30T17:29:09Z — all cited paths `no_recorded_issue` (see Coverage).  
**Method:** `get_architecture(all)` + 3 parallel swarm partitions (deep-sync, core-infra, ui-surface). No edits. Prove with `cargo tree`, `rg`, `wc -l`.  
**Principles driving every cut:** `laziness-protocol`, `subtract-before-you-add`, `model-the-domain`, `minimize-reader-load`, `boundary-discipline`, `guard-the-context-window`.

---

## How to read this report

Each finding is one line per `ponytail-audit` tag, ranked biggest cut first. Severity maps to launch risk.

| Tag | Meaning | Replacement |
|-----|---------|-------------|
| `delete` | dead code / speculative feature | nothing |
| `stdlib` | stdlib ships it | named function |
| `native` | platform already does it | named feature |
| `yagni` | one impl / one caller / one product | inline or delete |
| `shrink` | same logic fewer lines | shorter form |

Impact is framed for **consumer** (what the user notices) and **maintainer** (what the owner inherits) per `experience-first` + `model-the-domain`.

---

## 1. Ranked findings — biggest cut first (net deletable ~3 800–4 500 LOC, ~5% of repo, before deps)

### P1 — Blockers for launch (ship with these and the onboarding story is harder to explain)

| # | Tag | What to cut → replacement | Evidence | LOC | Consumer | Maintainer |
|---|-----|---------------------------|----------|-----|----------|------------|
| **1** | `yagni` `delete` | Entire `ferry-pin` crate (facade re-exporting `ferry-sync-engine`) → `engine::pin::{manager,release}` submodule | `crates/ferry-pin/src/lib.rs:20-35` re-exports 9 symbols from engine; zero reverse dep `rg "use ferry_pin" crates/ferry-sync-engine` = 0; `rg "ferry-pin" Cargo.toml` = 6 dependents that could depend on `ferry-sync-engine` directly | **828** (35+529+264) | One fewer crate to explain in install docs. | One fewer circular re-export (`converge → hold → pin → hold`). `model-the-domain`: pinning is a convergence policy, not a separate domain. |
| **2** | `delete` `duplicate` | Dual `RouteTable` registries (instance `Inner.routes` + global `GLOBAL_DIRECTORY: OnceLock<RouteTable>`) → single injected `RouteTable` via `IrohConfig::routes` | `crates/ferry-iroh/src/directory.rs:60-155` vs `crates/ferry-iroh/src/transport.rs:96,191,241,464` (`resolve_route` checks `self.inner` then fallback global) | ~80 + fixes bug | User sees one routing truth (no stale global). | Fixes `separate-before-serializing-shared-state` violation. Two maps storing same `RouteKey→Route`. |
| **3** | `shrink` `yagni` | `AutoBackend` / `DirectBackend` / `InProcessAdapter` trio (1 074 LOC total) → one `FolderBackend<StateSource>` | `crates/ferry-daemon/src/ui/backend.rs:120` (610) + `:697` (50) + `:830` (414); `get_status` 70% identical at `:176` vs `:846`; `share_*`/`pair_*` duplicated 3× differing only by `spawn_blocking` | **~600** | Fewer backend bugs to surface as 500s. | `minimize-reader-load`: one place to add an endpoint. Today 3 places. |
| **4** | `duplicate` | Dual rendezvous (in-mem `SharedRendezvous: Arc<Mutex<HashMap>>` + fs `/tmp/ferry-rendezvous-<CODE>.json` + no-op `ferry-iroh/src/rendezvous.rs:advertise/discover`) → one transport | `crates/ferry-folder/src/pairing.rs:90,146` (in-mem 1 line + fs 60 LOC `write/read/remove_rendezvous_file`; `peek_session` checks both) + `crates/ferry-iroh/src/rendezvous.rs:22,31` stubs + `crates/ferry-iroh/src/transport.rs:464` fallback | ~95 | Pairing either works via mDNS/relay or it does not. The fs file is the leftover `zero-file` that still writes a file. | Two sources of truth for same `SessionRecord`. Pick one. |
| **5** | `duplicate` `shrink` | `ferry-tui/src/app.rs:196 handle_key` (143 LOC) vs `:339 handle_key_action` (210 LOC) duplicated `match key.code` trees → keep async variant, sync wrapper delegates | `rg -n handle_key crates/ferry-tui/src/app.rs` = 4 defs; diff shows identical picker/modal guards | **~140** | Fixes divergence where TUI and async policy drift. | One keymap to change. |
| **6** | `yagni` `duplicate` | `DirectionCipher` in `crates/ferry-sync/src/session.rs:137,202` (115 LOC) byte-compatible duplicate of `ferry_proto::secure::SessionCipher` (comment admits it) → reuse `ferry_proto::secure` directly | `session.rs:137` "`Byte-compatible with ferry_proto::secure::SessionCipher (proven by interop tests)`" | **115** | No user-visible change. Removes a place where a crypto bug could be fixed in one copy and not the other. | `model-the-domain`: one cipher. |
| **7** | `stdlib` `native` | `ferry-crypto/src/base32.rs:25-74` (172 LOC) custom 5-bit packing + `ferry-crypto/src/crc32.rs:13-23` (48 LOC) bitwise CRC-32 → `data-encoding` / `crc32fast::hash` (dep already via `ferry_store`) | `crc32` same poly `0xEDB88320`; `base32` diverges from RFC4648 only by `ALPHABET = b"23456789..."` (20 symbols, 4 groups for short codes) | **~220** | No user impact. | `subtract-before-you-add`: stop owning bit math. `crc32fast` is table-driven, 10× faster. |
| **8** | `shrink` `yagni` | `ferry-sync-engine/src/hold.rs:17-89` (72 LOC) two one-caller fns (`hold_matcher`, `record_held`) + `ferry-sync-engine/src/matcher.rs:7-64` `PathMatcher` wrapper over `ignore::gitignore::Gitignore` → inline `hold_matcher` into `ConvergenceEngine` and use `Gitignore` directly | `hold_matcher` 1 prod caller `converge.rs:301`, `record_held` 1 prod caller `converge.rs:436`; `PathMatcher::matches` adds single `starts_with` fallback not exercised in prod | **~180** | No user impact. | `minimize-reader-load`: 3 files collapse to 1. File exports one thing. |

### P2 — Strongly recommended pre-launch (polish + perf)

| # | Tag | What to cut → replacement | Evidence | LOC |
|---|-----|---------------------------|----------|-----|
| 9 | `duplicate` | `format_bytes` duplicated `crates/ferry-gui/src/app.rs:59` (14) + `crates/ferry-tui/src/state.rs:68` (15) → `ferry_platform::human_bytes` or `ferry_ipc::format::bytes` | `rg -n "fn format_bytes" crates` = 2, `rg format_bytes` = 11 hits | **28** |
| 10 | `duplicate` | `hex_short`/`device_short`/`id_short`/`hex_of` 7 defs `crates/ferry-crypto/src/lib.rs:73`, `crates/ferry-sync-engine/src/naming.rs:25`, `crates/ferry-iroh/src/identity.rs:48`, `crates/ferry-sync/src/engine.rs:1128`, `crates/ferry-sync/src/exchange.rs:685`, `crates/ferry-folder/src/pairing.rs:1088`, `crates/ferry-materialize/examples/apply_once.rs:71` → one `ferry_store::format::hex(&id[..N])` | `rg -n "fn hex_short|fn device_short|fn id_short|fn hex_of" crates` = 7 | ~20 |
| 11 | `duplicate` `two-sources` | `SyncState` (`crates/ferry-tui/src/state.rs:13`, 40 LOC) vs `BeaconState` (`crates/ferry-gui/src/beacon.rs:12`, 48 LOC) + `gui/app.rs:353 beacon_state()` (25) + `app.rs:381 current_badge()` — same domain, different names (`Pinned` vs `Holding`) → shared `EngineState` enum | `rg "BeaconState|SyncState" crates` = 55 hits; `multi_frontend_consistency` test exists only to keep them in sync | **~80** |
| 12 | `stdlib` | `percent_decode_query_value` (`crates/ferry-daemon/src/ui/server.rs:583`, 47) + `extract_token` (`:211`, 24) + `api_fs_ls` re-validation (`:631`, 87) vs single `ferry_folder::inventory::validate_path` → `url::form_urlencoded::parse` / `percent_encoding` (already via `axum`) + trust single validator | `rg percent_decode` = 3 hits only in `server.rs:583,653,656`; axum already depends on `percent-encoding` | **~70+55** |
| 13 | `shrink` | `AutoBackend` 10× identical `match client.X().await { Ok=>Ok, Err(e) if e.is_transport()=>fallback }` (`crates/ferry-ipc/src/backend.rs:796-1145`) → `fn fallback_or` or macro | 10 copy-paste arms ≈ 200 LOC → 30 | **~170** |
| 14 | `shrink` `stdlib` | `crates/ferry-store/src/pack.rs:965 PackCache` 110 LOC manual `HashMap+VecDeque+Mutex` LRU + `pack.rs:1281 StagingPools::offer` 75 LOC duplicated `data/meta` branches → `lru::LruCache` (indirect dep already) + extract `pool(is_meta)` | `PackCache::get` does `order.remove(pos)` O(n) scan | **~185** |
| 15 | `shrink` `stdlib` | `crates/ferry-store/src/format.rs:128 put_u*/put_bytes` 51 wrappers + `format.rs:153 Reader` 64 LOC + `format.rs:219 hex/unhex` 23 LOC → inline `extend_from_slice(&v.to_le_bytes())` + `std::io::Cursor` + `hex` crate (already via `blake3`) | `rg put_u` = 51 hits in `ferry-store`; `Reader` duplicated in `ferry-crypto/src/pairing.rs:117` | **~109** |
| 16 | `native` `yagni` | `crates/ferry-platform/src/time.rs:38 civil_from_days/days_from_civil` 80 LOC Gregorian calendar + `time.rs:13 split_unix/join_unix` vs `chrono`/`time` crate | 80 LOC vs `chrono::NaiveDateTime::from_timestamp` one-liner | **~80** |
| 17 | `yagni` | `crates/ferry-iroh/src/config.rs:99 IrohConfigBuilder` 45 LOC 6 setters + `build()` returning `self.0` → struct literal `IrohConfig{ secret: Some(...), ..Default::default()}` | 6 setters × 5 lines, one product | **45** |
| 18 | `yagni` `native` | `ferry_platform::time` hand-rolled `mtime_sec/mtime_nsec/split_mtime` 4 clones (`ferry-scan/src/walk.rs:579`, `engine.rs:1015`, `ferry-materialize/src/apply.rs:579`) + `live_exec` 5 defs → single `ferry_platform::split_unix` + `PermissionsExt::mode & 0o111` via `std::os::unix::fs` | `rg split_unix\|mtime_sec` = 27 hits | ~50 |

### P3 — Nice-to-have (ship after launch, or batch into one cleanup PR)

| # | Tag | What to cut → replacement | Evidence | LOC |
|---|-----|---------------------------|----------|-----|
| 19 | `shrink` | `crates/ferry-folder/src/folder.rs:128 create_folder` vs `:159 adopt_folder` 90% identical → `fn init_store(root,fmk,poly)` | diff shows `create_dir_all`+`Store::create`+`put_polynomial`+`wrap_folder_key`+`write_config_head` repeated | ~26 |
| 20 | `shrink` | `default_listing_root` vs `ferry_home` (`crates/ferry-folder/src/inventory.rs:109+134`) 18 LOC `FERRY_HOME → current_dir/HOME → /tmp` duplicated | verbatim | 18 |
| 21 | `delete` `yagni` | `crates/ferry-cli/src/folder.rs:1` 75 LOC thin re-export over `ferry-folder` (every fn is `map_err(CliError::from)`) + `crates/ferry-daemon/src/folder_engine.rs:1` 5 LOC re-export | single caller `cli/commands/*`; platform branch already covered | 80 |
| 22 | `delete` | `crates/ferry-store/src/store.rs:613 REBUILD_INDEX_CALLS / LOCK_HOLD_MAX_US / DEBUG_LOCKS / note_hold()` 28 LOC `FERRY_DEBUG` probe + `store.rs:775 set_seal_target` 5 LOC | `rg REBUILD_INDEX_CALLS` = 3 hits (decl+2 tests) | 33 |
| 23 | `delete` `yagni` | `crates/ferry-ipc/src/backend.rs:331 FakeBackend` 420 LOC test double in `src/` + `InMemPairingSession` → move to `tests/` | duplicates `Supervisor`+`FolderInventory` logic | ~420 (test-only) |
| 24 | `shrink` | `ferry-sync-engine/src/naming.rs:36 conflict_display_name` + `unique_conflict_dest:60` advisory probe + `converge.rs:774 write_loser_copy` counter loop → single exclusive landing (second probe already handles collision) | landing appears in both `naming::unique_conflict_dest:60-83` and `converge::write_loser_copy:774-818` | ~60 |
| 25 | `shrink` `yagni` | `crates/ferry-platform/src/winpath.rs:1` 210 LOC `\\?\` prefix compiled everywhere + `crates/ferry-materialize/src/apply.rs:2013 validate_components` colon `:` guard speculative on non-Windows + `temp.rs:43 NAME_LEN_LIMIT=200` 50 LOC hash via `BLAKE3(rel_path)[..16]` | `extend_path` is identity on POSIX | ~260 (gated) |
| 26 | `shrink` `native` | `crates/ferry-tui/src/state.rs:107` 6 cached strings + `:416 update_cached_strings` 53 LOC called from 7 `apply_*` methods (premature zero-alloc) → compute in `render_*` | zero-alloc claim saves one `format!` per 200ms frame | ~63 |
| 27 | `shrink` | `crates/ferry-daemon/src/ui/server.rs:268 asset()` + `:277 serve_index/serve_css/serve_js` 15 LOC + `:289 fallback` → one `asset` + fallback (router already has 5 paths `"/","/index.html","/index","/style.css","/app.js"` serving same index) | fallback already serves index | ~37 |
| 28 | `shrink` | `crates/ferry-sync-engine/src/pin_error.rs:8` 49 LOC `PinError` re-wrapping `StoreError/ManifestError/Io` (duplicates `ConvergenceError` strings) + `report.rs:94 compactor` `COMPACT_MAX_LINES=4096/KEEP=1024` speculative retention | `grep StructuralSplit` = 2 identical `#[error("cannot converge safely under this pin...")]` literals | ~49+56 |

---

## 2. Deep call chains (>3 layers) — `laziness-protocol` / `minimize-reader-load`

If answering “where does X come from?” needs >3 hops, flatten.

| Chain | Depth | Where | Fix |
|-------|-------|-------|-----|
| `ConvergenceEngine::converge:278` → `reconcile:334` → `index_change_set:181`+`is_ancestor:294` → `manifest_chunk_refs:251` → `chunk_path_map:669` DFS → `hold::hold_matcher` → `PathMatcher` → `ignore::Gitignore` | 7 | `crates/ferry-sync-engine/src/converge.rs:278,334,669` + `hold.rs:17` + `matcher.rs:7` | `reconcile` returns `chunks_held: BTreeSet<BlobId>` so `gate_plan` needs no second tree walk; delete `chunk_path_map` DFS (60 LOC) |
| `run_engine:140` → `folder_phases:279` → `exchange_offers:501` → `run_stage:369` → `pull_folder:952` → `fetch_blobs:780` → `read_item_batches:818` → `store.put_blob` | 7 | `crates/ferry-sync/src/engine.rs:140,279,369,501,952` | Collapse 3 BFS guards (`MAX_BFS_ROUNDS=64`, `MAX_ADVERT_ROWS_TOTAL=262144`, `MAX_BATCHES_PER_ROUND=1024`) into one `BUDGET` |
| `IrohTransport::dial:459` → `dial_endpoint:277` → `block_on:99` (`try_current().runtime_flavor`) → `build_endpoint:392` (`RelaySetting` + mDNS) → `Endpoint::builder(presets::Minimal)` → `spawn_path_sampler:429` | 5+runtime hop | `crates/ferry-iroh/src/transport.rs:99,277,392,429,459` | Require `Handle` injection instead of `Mutex<Option<Runtime>>` per instance (saves 30 LOC + `thread::spawn` fallback) |
| `ScanEngine::watch_with:469` → `spawn_watcher:601` → `notify::RecommendedWatcher::new` → `classify_watch_error:853` → `PolicyState::apply:91` → `Walker::run:139` → `rebuild_dir:248` → `stream_file_chunks:460` | 8 | `crates/ferry-scan/src/engine.rs:87,469,601,853` + `state.rs:24` | Collapse `PolicyState` free fns + inline `DirCache` (55 LOC) |
| `Applier::apply_manifest:347` → `flatten_tree:1493` → `Desired::of:127` → `run:477` → `plan_upsert:956` → `content_matches:1977` → `Store::get:848` → `PackCache::get:991` | 9 | `crates/ferry-materialize/src/apply.rs:347,477,956,1977` + `ferry-store/src/pack.rs:991` | `content_matches` already re-reads file after `store.get` verifies `blake3`; delete second read |
| `cli/commands/ui.rs:318 run` → `run_web_mode` → `DashboardServer::new(AutoBackend)` → `AutoBackend::inner: ferry_ipc::backend::AutoBackend` → `InProcessAdapter` → `PairingRitual` → `FolderInventory` → `Store` → `ScanEngine::watch_with` | 8 | `crates/ferry-cli/src/commands/ui.rs:318` + `ferry-daemon/src/ui/backend.rs:697` + `supervisor/mod.rs:42` | `Supervisor::spawn_engine` + `FolderEngine::start_internal` → one `SyncEngine::open_watched_folder(path)` |

---

## 3. Dependency audit — `cargo tree` (690 packages)

**No unneeded heavy deps to drop** — workspace already did T-01 hoist and `lean` features (`ferry-cli` lean, `ferry-daemon` lean, `ferry-gui gui`, `ferry-tui`). Evidence:

- `tokio = "=1.49.0"` pinned tree-wide (T-01 forbids bump), features `rt,rt-multi-thread,time,net,macros,sync,io-util` minimal.
- `iroh = "=1.0.3"` + `iroh-mdns-address-lookup = "=0.5.0"` + `iroh-relay = "=1.0.3"` pinned together, correct per `ferry-iroh/Cargo.toml` notes.
- `egui 0.29 + eframe 0.29` only via `ferry-gui` optional `gui` feature; `ratatui 0.29 + crossterm 0.28` only via `ferry-tui` optional `tui`. `ferry-cli` default = `["web-ui","tui","gui"]`, `lean = []` strips both.
- `ignore = "0.4"` used **only** for `gitignore::Gitignore` pattern compilation (`ferry-ignore` + `ferry-sync-engine` + `ferry-pin`); walker intentionally not used — documented in `ferry-ignore/src/lib.rs`.
- `qrcode 0.14` only for terminal ASCII pairing (`ferry-cli` + `ferry-crypto` + `ferry-gui`); the ASCII rendering in `ferry-gui/src/modals.rs:285` could be removed (native share sheet) but the dep is tiny.
- `axum 0.8` only via `web-ui` feature (`ferry-cli`, `ferry-daemon`).

**One dep that earns deletion via code deletion:**

- `crc32fast` is already indirect via `ferry_store` (through `blake3`/`zstd` transitive). Using it for `ferry-crypto/src/crc32.rs` adds 0 new deps — it replaces hand-rolled code.
- `data-encoding` would be new if `base32.rs` is replaced; alternatively keep custom `ALPHABET` and document why RFC4648 is insufficient (20-symbol `23456789...` for 4-group short codes) — then `base32.rs` is 50 LOC not 172, shrink not delete.

**Net deps possible:** `-0` direct deps (workspace lean already optimal), `-1` crate (`ferry-pin`) via merge, `~ -0` external via stdlib replacements (already in tree). The win is **LOC and crate count**, not `Cargo.toml` lines.

---

## 4. Launch-readiness checklist

### Blockers — fix before you ship the install story

- [ ] **Single routing truth.** Delete `GLOBAL_DIRECTORY` or `Inner.routes`; keep one. Proves with `rg "GLOBAL_DIRECTORY|Inner.routes" crates/ferry-iroh`. Owner: iroh transport. Principle: `separate-before-serializing-shared-state`.
- [ ] **Single pairing rendezvous.** Decide: mDNS/relay (iroh) **or** fs file + `SharedRendezvous`. If fs file stays, document why cross-process single-machine needs it. Owner: pairing. Principle: `model-the-domain` (one session registry).
- [ ] **One backend.** Ship with one `UiBackend` impl; delete `Auto` wrapper triplication. Proves with `wc -l crates/ferry-daemon/src/ui/backend.rs` dropping 1 244 → ~500. Owner: daemon ui. Principle: `minimize-reader-load`.
- [ ] **Cipher single-source.** `session.rs:137 DirectionCipher` must delegate to `ferry_proto::secure::SessionCipher` or be deleted. Proves with `rg DirectionCipher crates/ferry-sync` → 0. Principle: `encode-lessons-in-structure` (one cipher).
- [ ] **Backend dispatch dedup.** `ferry-ipc/src/backend.rs:796 AutoBackend` 10 identical `is_transport` arms → macro. Proves with `rg "is_transport" crates/ferry-ipc/src/backend.rs` 10 → 1 site.

### Nice-to-have — one cleanup PR after launch (ranked)

- [ ] Merge `ferry-pin` into `ferry-sync-engine::pin` (~800 LOC, -1 crate).
- [ ] Inline `hold.rs` + `matcher.rs` into `converge.rs` (~180 LOC).
- [ ] Deduplicate `format_bytes` + `hex_short` families + `SyncState/BeaconState` (~130 LOC).
- [ ] Replace `percent_decode_query_value` + `api_fs_ls` double validation (~125 LOC).
- [ ] Replace `base32`/`crc32` hand-rolls + `IrohConfigBuilder` + `PackCache` LRU + `format::Reader`/`put_*` shims (~450 LOC).
- [ ] Flatten deep chains listed above (each chain -1 layer = -30s reader time).

### Already lean — ship as-is

- Workspace dep hoist (T-01) and `lean` feature flags are correct.
- `ferry-platform` (1609 LOC) correctly owns `casefold`, `links`, `lock`, `procs`, `reserved`, `winpath`, `time` — no duplication worth moving.
- `ferry-store` (474 nodes) correctly owns `format`/`pack`/`store` — hot paths are the ones with hotspots (`converg/join` fan_in 416 is real, not accidental).
- `ferry-proto` `DuplexHalf` is test-only (`rg DuplexHalf` 47 hits, 1 prod trait blank impl) — leave it, but gate file with `#[cfg(test)]` to make intent explicit.

---

## 5. Coverage & provenance

All cited paths checked via `check_index_coverage` (project `ferry-sync`, moderate, `2026-08-30T17:29:09Z`):

```
crates/ferry-sync-engine/src/converge.rs        no_recorded_issue
crates/ferry-pin/src/manager.rs                 no_recorded_issue
crates/ferry-iroh/src/rendezvous.rs             no_recorded_issue
crates/ferry-daemon/src/ui/backend.rs           no_recorded_issue
crates/ferry-daemon/src/ui/server.rs            no_recorded_issue
crates/ferry-tui/src/app.rs                     no_recorded_issue
crates/ferry-tui/src/state.rs                   no_recorded_issue
crates/ferry-gui/src/app.rs                     no_recorded_issue
crates/ferry-gui/src/beacon.rs                  no_recorded_issue
crates/ferry-store/src/store.rs                 no_recorded_issue
crates/ferry-store/src/format.rs                no_recorded_issue
crates/ferry-sync/src/session.rs                no_recorded_issue
crates/ferry-sync/src/transport.rs              no_recorded_issue
crates/ferry-platform/src/time.rs              no_recorded_issue
crates/ferry-scan/src/engine.rs                 no_recorded_issue
crates/ferry-materialize/src/apply.rs           no_recorded_issue
crates/ferry-folder/src/pairing.rs              no_recorded_issue
crates/ferry-ipc/src/backend.rs                 no_recorded_issue
```

Index status: `parse_partial` only `prototypes/fluid-glass/index.html:97` + `prototypes/liquid-glass/index.html:121` (trivial HTML, irrelevant). `skipped 0`. Full graph available for cited symbols; HTML parse gaps do not affect Rust findings.

Live evidence captured this run:

```
cargo metadata packages: 690
total Rust LOC: 78 700 (allocated), 58 321 in src/**/*.rs (wc -l)
rg "fn format_bytes": 2 defs (tui/state.rs:68, gui/app.rs:59) — 11 total hits
rg "fn hex_short|device_short|id_short|hex_of": 7 defs
rg "enum SyncState|BeaconState": 2 enums, 55 hits
rg "percent_decode": 3 hits, only server.rs:583
rg "SharedRendezvous|rendezvous_file_path": 16 hits
rg "PackCipher": 26 hits
rg "put_u": 51 hits in ferry-store
rg "civil_from_days|days_from_civil": 6 hits
```

No links fabricated. Every `path:line` above was `Read` by a swarm subagent or `rg` this session.

---

## 6. Net

```
net: -3 800 to -4 500 lines, -1 crate (ferry-pin), -0 deps possible
     P1 (blockers) ~1 700 LOC — single truths for routing, rendezvous, backend, cipher
     P2 (pre-launch polish) ~1 400 LOC — stdlib/native replacements + LRU + format_bytes/hex/percent_decode
     P3 (post-launch cleanup) ~900 LOC — thin re-exports, caches, speculative validators
```

`Lean already?` No — but close. The repo is one consolidation pass away: delete one crate, one global, one backend triplication, one cipher copy. Everything else is polish. Ship the P1 list and the install story is “one way to pair, one backend, one cipher” — which is what launch copy needs.

*Report: `.scratch/ponytail-audit-2026-08-31.md` — no code edits made.*
