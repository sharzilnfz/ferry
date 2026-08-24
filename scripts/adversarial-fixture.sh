#!/usr/bin/env bash
# T-012 acceptance runner — adversarial fixture tree:
#
#   unicode names (NFD spellings), case-only rename, deep nesting past
#   260 chars, symlink chains.
#
# Generates the fixture twice (two simulated devices) and asserts:
#   1. snapshot -> materialize -> resnapshot reproduces the identical root
#      tree id (names, exec bits, file/dir/symlink mtimes); a case-only
#      rename propagates; folding hosts refuse the guarded form loudly and
#      converge via the unguarded apply;
#   2. one reconciliation conflict inside the fixture converges with zero
#      data loss, correct winner bytes, NFC-consistent quarantine naming,
#      and a fixed point.
#
# Usage: scripts/adversarial-fixture.sh
# Exit code: 0 all assertions pass; non-zero otherwise (cargo propagates).
#
# Portable: pure bash around a cargo invocation, so it runs under git-bash
# on Windows runners too. The Rust test itself is platform-safe (deep paths
# go through \\?\ prefixing on Windows; symlink creation is probe-gated).

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

echo "== building ==" >&2
cargo build -q -p ferry-sync-engine --tests

echo "== running adversarial fixture ==" >&2
exec cargo test -p ferry-sync-engine --test adversarial_fixture -- --nocapture
