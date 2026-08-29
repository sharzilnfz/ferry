# T-04: Poison-tolerant locks in long-running processes

Status: done

~12 `lock().unwrap()` sites turn one panicked thread into a total daemon
crash via mutex poisoning: `crates/ferry-relay/src/lib.rs:61,66,161,172,218,
244`, `crates/ferry-iroh/src/directory.rs:54-67`, `crates/ferry-iroh/src/
transport.rs:101,562,580`, `crates/ferry-cli/src/commands/daemon.rs:227`.
The guarded state (append-only ledger, route maps) tolerates recovery.

Fix: one small shared helper per affected crate (or a tiny internal module in
ferry-platform if both relay+iroh already depend on it):
`fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> { m.lock().unwrap_or_else(
PoisonError::into_inner) }` and use it everywhere those mutexes are taken.
Do not change locking semantics or add parking_lot.

Acceptance: rg finds no bare `.lock().unwrap()` left in ferry-relay /
ferry-iroh / ferry-cli daemon command; a regression test poisons the mutex
(deliberately panic while holding it in a spawned thread, catch_unwind) and
asserts subsequent operations still work.
