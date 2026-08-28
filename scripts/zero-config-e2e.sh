#!/usr/bin/env bash
# zero-config-e2e.sh — Zero-Configuration 2-Minute Quickstart E2E Acceptance Test
#
# Tests zero-configuration synchronization across two independent $FERRY_HOME
# device environments using automatic daemon bootstrapping:
#   1. Device A: `ferry share` (auto-bootstraps daemon A, emits 6-word code)
#   2. Device B: `ferry join <code> <dest>` (auto-bootstraps daemon B, connects)
#   3. Verifies initial files and continuous live mutations sync byte-for-byte.

set -euo pipefail

TIMEOUT_SECONDS="${1:-60}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
START_TS="$(date +%s)"

echo "Building ferry binary..." >&2
(cd "$REPO_ROOT" && cargo build -q -p ferry-cli) >&2
BIN="$REPO_ROOT/target/debug/ferry"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/ferry-zero-config.XXXXXX")"
HOME_A="$TMP/device-a"
HOME_B="$TMP/device-b"
TREE_A="$TMP/tree-a"
TREE_B="$TMP/tree-b"
mkdir -p "$HOME_A" "$HOME_B" "$TREE_A" "$TREE_B"

cleanup() {
    trap - EXIT INT TERM
    # Kill any child daemons spawned during test
    if [ -f "$HOME_A/daemon.sock" ] || [ -f "$HOME_B/daemon.sock" ]; then
        pkill -f "$HOME_A" 2>/dev/null || true
        pkill -f "$HOME_B" 2>/dev/null || true
    fi
    if [ "${FERRY_KEEP:-0}" = "1" ]; then
        echo "(FERRY_KEEP=1: leaving $TMP in place)" >&2
        return
    fi
    rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

fail() {
    echo "FAIL: $*" >&2
    echo "=== DAEMON A LOG ===" >&2
    cat "$HOME_A/daemon.log" 2>/dev/null >&2 || true
    echo "=== DAEMON B LOG ===" >&2
    cat "$HOME_B/daemon.log" 2>/dev/null >&2 || true
    exit 1
}
step() { printf '\n== %s\n' "$1" >&2; }

run_a() { FERRY_HOME="$HOME_A" "$BIN" "$@"; }
run_b() { FERRY_HOME="$HOME_B" "$BIN" "$@"; }

wait_for_file() {
    file_path="$1"
    timeout_s="$2"
    deadline=$(( $(date +%s) + timeout_s ))
    while [ ! -e "$file_path" ]; do
        if [ "$(date +%s)" -ge "$deadline" ]; then return 1; fi
        sleep 0.1
    done
    return 0
}

step "1. Prepare project files in Tree A"
echo "hello zero-config world" > "$TREE_A/hello.txt"
mkdir -p "$TREE_A/src"
cat > "$TREE_A/src/index.js" << 'JS'
console.log("Ferry zero-config quickstart working!");
JS

step "2. Device A: run 'ferry share' with auto-daemon bootstrapping"
SHARE_JSON="$( cd "$TREE_A" && run_a share --json )" || fail "ferry share failed on Device A"
echo "Share output: $SHARE_JSON" >&2

# Verify background daemon socket on Device A
[ -e "$HOME_A/daemon.sock" ] || fail "Background daemon socket not created on Device A"

# Extract 6-word pairing code
PAIRING_CODE="$(echo "$SHARE_JSON" | grep -o '"code":"[^"]*"' | cut -d'"' -f4)"
[ -n "$PAIRING_CODE" ] || fail "No pairing code extracted from share output"
echo "Extracted pairing code: $PAIRING_CODE"

step "3. Device B: run 'ferry join <code>' with auto-daemon bootstrapping"
JOIN_JSON="$( run_b join "$PAIRING_CODE" "$TREE_B" --json )" || fail "ferry join failed on Device B"
echo "Join output: $JOIN_JSON" >&2

# Verify background daemon socket on Device B
[ -e "$HOME_B/daemon.sock" ] || fail "Background daemon socket not created on Device B"

step "3.5. Status on Device A and Device B"
run_a status --json || true
run_b status --json || true

step "4. Wait for initial sync convergence to Tree B"
wait_for_file "$TREE_B/hello.txt" "$TIMEOUT_SECONDS" || fail "hello.txt never arrived on Device B"
wait_for_file "$TREE_B/src/index.js" "$TIMEOUT_SECONDS" || fail "src/index.js never arrived on Device B"

# Compare content
diff -u "$TREE_A/hello.txt" "$TREE_B/hello.txt" || fail "hello.txt contents differ between A and B"
diff -u "$TREE_A/src/index.js" "$TREE_B/src/index.js" || fail "src/index.js contents differ between A and B"
echo "Initial synchronization verified byte-for-byte."

step "5. Test live continuous synchronization (A -> B)"
echo "appended line while running" >> "$TREE_A/hello.txt"
echo "newly minted file" > "$TREE_A/live_update.txt"

wait_for_file "$TREE_B/live_update.txt" 15 || fail "live_update.txt never arrived on Device B"
diff -u "$TREE_A/hello.txt" "$TREE_B/hello.txt" || fail "live edit to hello.txt did not propagate to B"
diff -u "$TREE_A/live_update.txt" "$TREE_B/live_update.txt" || fail "live_update.txt contents differ"
echo "Live update synchronization verified."

step "6. Inspect status on Device B"
STATUS_JSON="$( cd "$TREE_B" && run_b status --json )" || fail "status check failed on Device B"
echo "Status output on B: $STATUS_JSON" >&2

ELAPSED=$(( $(date +%s) - START_TS ))
echo ""
echo "========================================================"
echo "SUCCESS: Zero-config quickstart workflow verified in ${ELAPSED}s"
echo "========================================================"
