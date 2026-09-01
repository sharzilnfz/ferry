# Ticket 02: Zeroize Web UI Pairing Secrets and Fix Session PID Validation

Status: completed
Depends on:
Blocks: 10

## What to build

Fix two critical security and accuracy findings in the Web UI server and CLI token query:

1. **Pairing Secret Zeroization (ADR-0002 / ADR-0006)**:
   - In `crates/ferry-daemon/src/ui/server.rs` (`api_share`, `api_pair_create`, `api_pair_device`), pairing short codes, decrypted keys, and intermediate payload buffers are currently handled as un-zeroized raw `String`s.
   - Use `zeroize::Zeroizing<String>` or byte zeroization on pairing codes and intermediate QR render buffers. Ensure memory is cleared after generating responses.

2. **Web Session PID Directory Resolution**:
   - In `crates/ferry-cli/src/commands/ui.rs` (`read_web_session`), `ferry_platform::read_pid` is passed `session_file.parent()` (which is `.ferry`), causing `read_pid` to search for `.ferry/.ferry/daemon.pid`.
   - Update `read_web_session` to pass the folder root (`session_file.parent().and_then(|p| p.parent()).unwrap_or(session_file.parent().unwrap_or(session_file))`).
   - Ensure `is_pid_alive` fallback only triggers if the recorded PID is active and matches the expected binary name.

## Acceptance

- [x] `api_share` and `api_pair_create` wrap short codes and key buffers in `Zeroizing` containers.
- [x] `read_web_session` correctly resolves `.ferry/daemon.pid` from the project root.
- [x] `ferry ui token` returns the active URL with token when the web server is running and cleanly cleans up stale session files when the PID is dead.
- [x] `cargo test -p ferry-daemon --test server_tests` and `cargo test -p ferry-cli --test ui_server_tests` pass cleanly.

## Comments

Combines Standards finding #2 (pairing zeroization) and Spec finding #5 (PID directory check).
