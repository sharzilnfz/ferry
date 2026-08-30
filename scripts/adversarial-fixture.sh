#!/usr/bin/env bash

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

echo "== building ==" >&2
cargo build -q -p ferry-sync-engine --tests

echo "== running adversarial fixture ==" >&2
exec cargo test -p ferry-sync-engine --test adversarial_fixture -- --nocapture
