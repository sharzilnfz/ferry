# T-01: Workspace hygiene — [workspace.dependencies] + [workspace.lints]

Status: done

Hoist duplicated dependency declarations into `[workspace.dependencies]` in the
root Cargo.toml (serde/rand/blake3/tempfile/qrcode/etc. are hand-copied across
10+ crates; `qrcode = "0.14"` appears in both ferry-cli and ferry-crypto).
Members switch to `foo.workspace = true`.

Add a root `[workspace.lints]` table and `lints.workspace = true` in every
crate:
```toml
[workspace.lints.rust]
unsafe_op_in_unsafe_fn = "deny"
[workspace.lints.clippy]
correctness = { level = "deny", priority = -1 }
suspicious = { level = "warn", priority = -1 }
pedantic = { level = "warn", priority = -1 }
```
Ramp pedantic pragmatically: fix cheap mechanical warnings (needless clones,
redundant closures, doc formatting); where a pedantic lint fights existing
design, add a targeted `#[allow]` with a one-line reason rather than
rewriting. Do NOT enable lints that force mass rewrites (e.g. module_name_
repetitions) — allow them explicitly so the tree stays warning-free under
`-D warnings`. Keep every crate's version choices identical to today's
resolved versions (no dependency bumps).

Acceptance: `cargo clippy --workspace --all-targets -- -D warnings` green;
no duplicate version strings across crates/*/Cargo.toml for shared deps;
zero workspace-level lint warnings.
