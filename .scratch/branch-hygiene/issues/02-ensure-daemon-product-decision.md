# Ticket: Should more CLI commands auto-spawn the daemon?

Status: needs-triage
Depends on:
Blocks:

## Context

Main's daemon lifecycle is explicit (`ferry daemon --listen` + `DaemonLock`);
`bootstrap::ensure_daemon` exists and is wired into exactly one command site
(`commands/ui.rs:330`). `share`/`join` run the pairing ritual in-process and
need no daemon. A deleted branch (`feat/seamless-folder-picker`, see ticket
01 in this directory) contained a heavier auto-spawn design that main does
not need.

## Decision to make

Whether `sync`/`status`-style commands should auto-spawn the daemon via the
existing `ensure_daemon` primitive (≤5 lines per site, arguably behind a
`--no-spawn` flag), or keep the explicit lifecycle.

## How to decide

Use the app manually. If running `ferry daemon --listen` before
`sync`/`status` is recurring friction, wire `ensure_daemon` into those sites.
If the friction never appears, keep the explicit design and close this as
wontfix.

## Acceptance

- [ ] Decision recorded here after manual-testing experience
- [ ] If wiring: tests cover spawn-when-absent and reuse-when-running
