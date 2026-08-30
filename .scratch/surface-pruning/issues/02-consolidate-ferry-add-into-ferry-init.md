# 02: Consolidate `ferry add` into `ferry init`

**What to build:** One Folder initialization surface. `ferry add <path>` is deleted. `ferry init [path]` remains the single authoritative Store bootstrap for a Folder. The CLI registry keeps `Init` and deletes `Add`. Help epilog, shell completions, and `docs/cli-json.md` show one init entry. Existing callers are migrated in the same wave. No alias or compat shim is left behind.

**Blocked by:** None (can start immediately)

**Status:** done

- [x] `ferry add --help` and `ferry add .` are rejected with `unknown subcommand` and hint to use `ferry init` — `Cli::try_parse_from(["ferry","add",...]).is_err()`; `grep -rn Command::Add crates` = 0
- [x] `ferry init --help` and `ferry init .` still parse and bootstrap a Store via `ferry-folder` Folder seam
- [x] `Command::Add` variant and its `main.rs` dispatch are deleted — `crates/ferry-cli/src/cli.rs:56`, `crates/ferry-cli/src/main.rs:120`
- [x] `AFTER_HELP` lists only `ferry init` in the five-minute path — `crates/ferry-cli/src/cli.rs:13`
- [x] `docs/cli-json.md` contains one init schema and no `add` schema — heading `## ferry init [path]` and `"command": "init"`
- [x] Table-driven `tests/cli_parse.rs` asserts `add` rejection and `init` success — `add_is_rejected` 13/13 pass
- [x] `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check` pass
