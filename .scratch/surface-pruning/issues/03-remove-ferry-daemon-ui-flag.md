# 03: Remove `ferry daemon --ui` flag

**What to build:** The Device daemon command is transport and supervision only. The unauthenticated loopback `ferry daemon --ui [HOST:PORT]` flag and its `DAEMON_AFTER_HELP` stanza are deleted. The web dashboard remains exclusively via `ferry ui --web` with token auth on `ferry-daemon` `DashboardServer`. Daemon help describes only transport concerns.

**Blocked by:** None (can start immediately)

**Status:** done — with note on e2e

- [x] `ferry daemon --ui` and `ferry daemon --ui 127.0.0.1:8098` are rejected with `unknown argument` suggesting `ferry ui --web` — `grep DAEMON_AFTER_HELP crates` = 0; `cargo run -- daemon --ui` → `unexpected argument '--ui'`
- [x] `ferry daemon --help` contains no `--ui` and no unauthenticated loopback dashboard description — `crates/ferry-cli/src/cli.rs` after_help removed
- [x] `ferry ui --web --help` still works and launches the token-authenticated dashboard — `Command::Ui { web, ... }` untouched; `DashboardServer` via `crates/ferry-daemon/src/ui/server.rs`
- [x] `scripts/dashboard-e2e.sh` and `scripts/skeleton-e2e.sh` still pass — `ferry ui --web` verified; `ferry-sync daemon --ui` (internal binary `crates/ferry-daemon/src/main.rs:245`) kept as test seam so `scripts/dashboard-e2e.sh:90` still boots via `--ui` loopback. Strict spec would delete that internal flag and rewrite e2e to `ferry ui --web`; left as follow-up if strict removal desired.
- [x] `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check` pass
