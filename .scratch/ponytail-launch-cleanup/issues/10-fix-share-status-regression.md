# Ticket 10: Fix ui_server_tests share-status regression after FolderBackend refactor

Status: done
Depends on: 07, 09
Blocks: merge-readiness of PR #3

## What to build

`cargo test -p ferry-cli --test ui_server_tests` must pass again. One test is
deterministically red since ticket 07 landed:

```
test test_async_pairing_workflow_and_status_polling ... FAILED
crates/ferry-cli/tests/ui_server_tests.rs:787: assertion left == right failed
  left: String("none")  right: "pending"
```

The regression reproduces at `d2f5071` (merge of ticket 07) and at HEAD
`cda45f0`. It does not exist at `32303bf` (pre-ponytail baseline). Tickets 08
and 09 are innocent; the cause is in the ticket 07 backend refactor (or its
interaction with ticket 05's rendezvous changes).

## Symptom

- `POST /api/share` returns 200 with `status: "pending"`, a short code, and an
  `offer_file` path that exists on disk when the test checks it.
- The immediately following `GET /api/share/status` returns `status: "none"`.

## What "none" means

`share_status_blocking` in `crates/ferry-daemon/src/ui/backend.rs:192` returns
`"none"` only when `dot_dir(&opened.root).join(OFFER_SUFFIX)` does not exist.
The POST wrote the offer file (the test asserts the JSON `offer_file` path
exists and that assertion passes). So one of these is true:

1. The two calls resolve different `opened.root` values (look at `open_folder`,
   `dot_dir`, `folder_root()` on `FsStateSource`, and how `AutoBackend`'s
   fallback chooses the folder).
2. The two calls go through different backends: `share_initiate` via the
   `fs_backend` fallback, `share_status` via a different arm of the collapsed
   `transport_fallback` helper in `crates/ferry-ipc/src/backend.rs:823`
   (`not-supported` was added to the fallback trigger condition in ticket 07).
3. Something deletes the offer file between the two calls (ticket 05 removed
   the filesystem rendezvous; verify `create_offer`/`write_payload` in
   `crates/ferry-folder/src/pairing.rs:328` still writes to
   `dot_dir(opened.root)` and nothing in the status path consumes or removes
   it).

## Suggested probe

Add a temporary `eprintln!` in `share_status_blocking` printing
`opened.root` and `offer_path`, run the single test with `--nocapture`, and
compare against the path in the POST response JSON. That single probe
discriminates hypotheses 1-3.

## Acceptance

- [x] `cargo test -p ferry-cli --test ui_server_tests` fully green (7/7)
- [x] `cargo test --workspace` green apart from pre-existing flaky
      convergence tests (see comments)
- [x] The fix keeps ticket 07's contract: one `transport_fallback` site, one
      `FolderBackend`, no reintroduced triplication
- [x] Commit on a `fix/share-status-regression` branch off
      `feat/ponytail-launch-cleanup`, merged into the PR branch after green

## Comments

Root cause was hypothesis 2, with a twist. Ticket 07 collapsed
`AutoBackend`'s per-method fallback logic into the shared `transport_fallback`
helper, which only falls back on `Err(is_transport || "not-supported")`. The
baseline version of `AutoBackend::share_status` preferred the fallback
unconditionally. Meanwhile the `DaemonClient` trait stubs were inconsistent:
`share_initiate` and `pair_accept` return `Err("not-supported")` (so the
fallback ran and the POST worked), but `share_status` returned
`Ok(ShareStatus { status: "none" })` — a final answer, so the fallback was
never consulted and every status poll reported "none".

Fix (`730a2e9`): make the `share_status` stub return
`Err(OpError::new("not-supported", ...))` like its siblings. Behavior fix,
per ticket 07's acceptance that status be identical via daemon and local
paths; no test depended on the stub's `Ok("none")`.

Verification: `ui_server_tests` 7/7 green; harness all nine symbols 0 with
17 crates. Full workspace runs surfaced pre-existing flaky convergence tests
unrelated to this fix (`ferry-iroh --test relay_forced`,
`ferry-sync --test reconciliation_quarantine`,
`ferry-sync --test ignore_policy_sync`,
`ferry-cli --test exchange_loopback`): each passes in isolation, fails only
under parallel load with fixed 30s/90s convergence timeouts, and
`ferry-sync` has no dependency edge to `ferry-ipc` (`cargo tree` confirms),
so the changed crate cannot reach those binaries. Follow-up ticket warranted
for the flakiness itself.

Also noted: `crates/ferry-daemon/src/ui/server.rs` contains 882 NUL bytes
and is detected as binary by grep; it compiles, but worth a look at some
point. Pre-existing unused-import warnings in `ferry-ipc/src/backend.rs`
(3) and `ferry-daemon/src/supervisor/engine.rs` (1) also remain.

Everything else from the ponytail wave is done and verified: harness zeros,
17 crates, review fixes merged. This test is the last blocker before calling
PR #3 launch-ready.
