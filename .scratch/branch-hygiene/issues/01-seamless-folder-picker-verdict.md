# Ticket: Assessment of feat/seamless-folder-picker

Status: done
Depends on:
Blocks: branch deletion of feat/seamless-folder-picker

## Verdict

Do not port. The branch (single commit `9a4430d`, Aug 28, 51 files,
+6452/−354: seamless onboarding, TUI/web folder picker, multi-folder daemon
registry, in-band pairing, BIP39 mnemonics, daemon auto-spawn, e2e script)
is fully superseded by main. Assessed via subagent analysis on 2026-08-31
against main `ad246e1`.

## Feature-by-feature disposition

| Branch feature | Disposition | Main evidence |
|---|---|---|
| TUI folder picker (state.rs/ui.rs/app.rs, +320 tests) | In main, better | `ferry-tui/src/picker.rs` (323 lines, headless-TTY handling), `tests/picker_tests.rs` (519 lines) |
| Web folder picker + /api/fs/ls | In main, better | `ui/server.rs:108,637`; `assets/app.js:786+` modal with git-repo/already-synced warnings |
| Multi-folder registry (folders.toml) | In main, superseded design | `ferry-folder/src/inventory.rs` (locked registry, richer DirectoryEntry); daemon reworked into `device_daemon.rs` + `supervisor/engine.rs` |
| In-band network pairing | Obsolete | Ticket 05 unified rendezvous (`bb29ef0`); unified `PairingRitual`; the idea persists as planned work in `.scratch/deep-architecture-consolidation/issues/03` |
| BIP39 mnemonics | Obsolete | Rejected by ADR-0006 (6-word codes explicitly deferred) |
| Daemon auto-spawn (ipc.rs) | Mostly obsolete | Main has `bootstrap::ensure_daemon` (wired into `commands/ui.rs:330`); see companion ticket on the ensure_daemon product decision |
| zero-config e2e script | In main, rewritten | `scripts/zero-config-e2e.sh` adapted to share/join flow |

## Disposition of the branch

Branch deleted (local on sharzilx and origin) on 2026-08-31 after this
ticket was filed. Nothing unique survives: porting anything would mean
rewriting against crates that no longer exist in that shape (ferry-pin
merged into ferry-sync-engine, supervisor.rs replaced by supervisor/,
pre-FolderBackend IPC traits) only to re-arrive at code main already has.
