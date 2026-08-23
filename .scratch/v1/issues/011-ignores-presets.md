# T-011: Ignore rules and dev presets

Status: done
Depends on: T-004

Gitignore-syntax ignore files (`ferry.ignore`, plus honoring `.gitignore`
opt-in), tuned defaults per SPEC: caches out, `node_modules` opt-in,
`.env` opt-in with a loud warning. Agent-state presets: `ferry preset claude`
configures `~/.claude` selective rules (memory/skills/CLAUDE.md in, session
logs/caches out) per the research findings on agent state portability.

Acceptance: preset round-trips the documented include/exclude set; secret
scan heuristic flags likely-credential `.env` inclusion with a warning at
share time.

## Comments

### Engine choice (requirement 1)

New crate `crates/ferry-ignore`. Pattern matching delegates to the `ignore`
crate (BurntSushi), version 0.4.33, **using only
`ignore::gitignore::Gitignore` + `GitignoreBuilder::add_line()`** — pattern
compilation and single-file matching. Its directory walker is deliberately
NOT used: ferry-scan owns walking/event filtering, and this crate only
answers "is this relative path ignored?". License is `Unlicense OR MIT`,
compatible with this workspace (MIT). Hand-rolling was rejected: gitignore
edge cases (anchoring vs basename matching at depth, literal_separator so
`*` never crosses `/`, dir-only flags, negation quirks under excluded dirs)
are exactly where hand-rolled engines rot; BurntSushi's matcher is the
de-facto reference implementation (ripgrep). Verified against a semantics
table: anchoring, unanchored-at-every-depth, partial anchoring, dir-only,
doublestar forms (`**/x`, `x/**`, `a/**/b.md`, `**/temp/*.cache`),
last-match-wins, nested-file depth precedence, no-reinclude-under-excluded-
dir, comments/blanks, invalid-glob skip-and-count, NFC normalization of
pattern lines and (defensively) query paths.

### Precedence model (requirement 5), defined precisely

Four ROOT-level layers compile into one ordered gitignore; last-match-wins
gives exactly:

    built-in defaults < root ferry.ignore < applied presets < user overrides

Per-directory rule files BELOW the root (`ferry.ignore` in any subdir;
`.gitignore` too when `honor_gitignore = true`) stack AFTER the whole root
chain, shallowest first — i.e. git's own depth-first precedence: a deeper
file wins within its subtree over everything shallower, including presets.
This mirrors git exactly (the root chain plays the role of the top-level
ignore file). Within one directory, `.gitignore` compiles first and
`ferry.ignore` second (Ferry intent wins ties). Once any ancestor dir
verdict is Ignore, descendants stay ignored — git's documented quirk, which
our pruned walk enforces structurally anyway. Two exclusions sit outside all
layers: quarantine files (`*.ferry-conflict.*`) are NEVER ignorable
(ADR-0004), and `.ferry/` stays hard-excluded by the scan walker itself.

`.gitignore` honoring defaults OFF, honestly documented both directions in
`IgnoreConfig::honor_gitignore`: ON respects VCS intent but silently drops
the files users most want synced (`.env`-class — research archetypes 5/9)
and couples sync to git whims; OFF matches Ferry's thesis ("carry what git
refuses") at the cost of syncing deliberately-git-ignored junk unless it is
mirrored into `ferry.ignore`; the share-time secret scan covers the
dangerous subset.

### Default-set rationale (requirement 3)

Each entry documented inline in `defaults.rs`: `.DS_Store` (Finder churn),
`Thumbs.db` + `desktop.ini` (Windows metadata), `*.swp` + `*~` (editor
droppings) are OUT unconditionally; `node_modules/` OPT-IN (file count,
lockfile-restorable, historically destroys naive sync tools — cross-cutting
finding 1); `.env` + `.env.*` OPT-IN with LOUD share-time warning (54% of
.env files contain detectable secrets, GitGuardian via archetype 9).
Opting back in is one line: `!node_modules/`, `!.env` — cross-layer
negation tests cover both, including that `!.env` does NOT drag in
`.env.local`.

### Presets (requirements 5)

`Preset { id, description, includes, excludes }` with serde JSON round-trip
(stable serialization; acceptance test asserts byte-identical round-trip for
both builtins). Compiled to gitignore lines excludes-first then includes as
`!` negations, so includes rescue from broad OUT globs. Sources (research
archetype 8): claude IN `CLAUDE.md settings.json memory/** skills/**
commands/** agents/** projects/*/memory/**` (Lhotka's merge-aware sync
target), OUT `projects/**/sessions/** **/*.log statsig/ telemetry/ cache/
shell-snapshots/ downloads/` (nickang's "machine-specific junk" split);
opencode mirrors the shape (AGENTS.md/opencode.json/agent-command-plugin-
skill-memory dirs in; sessions, logs, cache/tmp/node_modules out). Unknown
preset ids fail construction loudly.

### Secret scan heuristic (requirement 4)

`scan_for_secrets(rules, root) -> Vec<Warning>` for share time. Only paths
the effective rules INCLUDE are scanned (excluded files never leave the
machine → silent by design). Path classes: `.env*`, `*.pem`, `*.key`,
`id_rsa*`, `credentials.json`, `.npmrc` — each yields a path-level warning
when included. Content classes (only inside those files): AWS
`AKIA[0-9A-Z]{16}`, OpenAI `sk-[A-Za-z0-9]{20,}` (ticket-verbatim),
GitHub `ghp_[A-Za-z0-9]{36}`, PEM private-key headers, generic
`(api[_-]?key|secret|token|password)\s*[=:]\s*\S+` case-insensitive; Slack
tokens require ≥10 chars after `xox[baprs]-` — the single deliberate
tightening over the ticket prefix, to keep prose from false-warning.
Warnings carry file, 1-based line, class label, REDACTED preview (first 4
chars + length; asserted never to contain the full secret); capped at 32
content warnings per file and an 8 MiB read cap; ignored dirs pruned;
symlinks not followed.

### Integration & tests (requirements 1, 6)

`FerryIgnore` implements `ferry_scan::IgnorePolicy`. The trait signature
carries no file/dir bit, so dir-only patterns resolve by double evaluation
(two cheap matches; ONE `symlink_metadata` spent only when they disagree —
vanished paths resolve as files). Integration tests drive the real engine:
mixed-content tree → manifest contains EXACTLY the allowed paths (including
rule files themselves, the opted-in `.env`, negated-back `node_modules`,
and depth-rescued `sub/keep.log`); an injected watcher event under an
ignored subtree publishes nothing and hashes zero bytes; an allowed change
publishes with correct manifest lineage. Totals: 38 unit + 2 integration
tests in ferry-ignore; workspace `cargo test --workspace` green (238
tests), clippy `--all-targets` clean, fmt applied.
