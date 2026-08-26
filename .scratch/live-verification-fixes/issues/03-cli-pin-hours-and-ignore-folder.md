# 03: CLI Flag & Argument Parity (`--hours` & `ignore` folder targeting)

Status: ready-for-agent
Depends on: 02-daemon-ipc-server-pin-liveness.md
Blocks: 09-e2e-live-process-and-browser-verification.md

**What to build:**
Bring CLI argument handling into parity with documented interfaces. Support the documented `--hours` duration flag on `ferry pin start` to allow users and agents to declare time-bounded protection windows. Enable `ferry ignore` subcommands to accept an optional target directory path so rules and preset layers can be inspected or modified for any folder without requiring users to switch directories first.

**Blocked by:** 02: Daemon IPC Server Binding & Long-Lived Pin Ownership

### Acceptance Criteria

- [ ] `ferry pin start --hours <N>` successfully parses duration in hours (defaulting to 8 hours when omitted).
- [ ] Pinned session metadata records the calculated expiration timestamp based on the supplied duration.
- [ ] The background daemon evaluates pin expiration during scan cycles, automatically releasing holds once the duration has elapsed.
- [ ] `ferry ignore` accepts an optional folder argument across pattern additions, preset applications, and list operations (e.g. `ferry ignore --list /path/to/project`).
- [ ] Running ignore commands against an external directory resolves rules and displays active layers relative to the specified folder root.
- [ ] CLI tests verify parsing and behavior of `--hours` and target directory arguments without regressions.
