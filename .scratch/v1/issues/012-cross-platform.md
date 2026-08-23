# T-012: Cross-platform guardrails and CI matrix

Status: ready-for-agent
Depends on: T-005, T-010

Case-conflict detection at scan time (case-folding index per folder), NFC name
normalization everywhere, Windows long paths via `\\?\` prefixes, explicit
symlink policy (sync as link where safe; refuse junction/symlink-to-dir on
Windows unless developer mode documented), reserved-name handling. GitHub
Actions matrix: macOS arm64, Ubuntu x64, Windows x64; the walking-skeleton and
reconciliation suites must pass on all three.

Acceptance: adversarial fixture tree (unicode names, case-only rename,
deep nesting past 260 chars, symlink chains) syncs correctly or fails loudly
with an actionable message on every OS.
