# Ticket 10: Full-Suite Verification and Zero-Warning Hygiene

Status: ready-for-agent
Depends on: 01, 02, 03, 04, 05, 06, 07, 08, 09
Blocks:

## What to build

Run full workspace verification and ensure zero compiler warnings, zero clippy warnings, and 100% green test passes across all workspace crates:

1. **Workspace Compilation & Lints**:
   - `cargo check --all-targets` with 0 warnings.
   - `cargo clippy --all-targets -- -D warnings` clean across all crates.
   - `cargo fmt -- --check` clean.

2. **Full Test Suite Execution**:
   - `cargo test --workspace` passes cleanly on an idle machine.
   - `cargo test -p ferry-cli --test network_pairing_e2e` passes.
   - `cargo test -p ferry-cli --test ui_server_tests` passes.
   - `cargo test -p ferry-cli --test daemon_lifecycle_tests` passes.

## Acceptance

- [ ] `cargo check --all-targets` produces 0 warnings.
- [ ] `cargo clippy --workspace --all-targets` is clean.
- [ ] `cargo test --workspace` is 100% green.

## Comments

Final verification gate for the `live-testing-remediation` milestone.
