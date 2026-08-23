#!/usr/bin/env bash
# ferry M0 walking skeleton — end-to-end acceptance (T-006), verbatim:
#
#   "script starts both daemons, touches 50 random files including an
#    append-heavy log file, asserts convergence within N seconds, tears
#    down."
#
# Usage: scripts/skeleton-e2e.sh [TIMEOUT_SECONDS]   (default 30)
#
# Everything runs under an OS temp dir; daemons get retried random free
# ports; teardown happens via trap even on failure or Ctrl-C. Exit code:
# 0 converged and verified, non-zero otherwise. Portable across macOS
# (bash 3.2) and GNU/Linux: only POSIX tools plus seq/cksum/find.

set -u

N_SECONDS="${1:-30}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

BIN=""
for cand in "$REPO_ROOT/target/debug/ferry-sync" "$REPO_ROOT/target/release/ferry-sync"; do
    if [ -x "$cand" ]; then BIN="$cand"; break; fi
done
if [ -z "$BIN" ]; then
    echo "building ferry-sync (debug)...">&2
    (cd "$REPO_ROOT" && cargo build -q -p ferry-sync) >&2
    BIN="$REPO_ROOT/target/debug/ferry-sync"
fi

TMP="$(mktemp -d "${TMPDIR:-/tmp}/ferry-e2e.XXXXXX")"
A_LOG="$TMP/node-a.log"
B_LOG="$TMP/node-b.log"
A_TREE="$TMP/a/tree"; A_STORE="$TMP/a/store"
B_TREE="$TMP/b/tree"; B_STORE="$TMP/b/store"
mkdir -p "$A_TREE" "$B_TREE" "$A_STORE" "$B_STORE"
PIDS=""

cleanup() {
    trap - EXIT INT TERM
    if [ -n "$PIDS" ]; then
        # shellcheck disable=SC2086
        kill $PIDS >/dev/null 2>&1 || true
        wait $PIDS >/dev/null 2>&1 || true
    fi
    rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

fail() { echo "FAIL: $*" >&2; exit 1; }

POLY="$("$BIN" genpoly --seed 20260824)" || fail "genpoly"

# --- launch node A (listener) on a retried free port ----------------------
launch_a() {
    local attempt port
    for attempt in 1 2 3 4 5; do
        port=$((20000 + RANDOM % 20000))
        "$BIN" daemon --role listen --addr "127.0.0.1:$port" \
            --store "$A_STORE" --tree "$A_TREE" --tag node-a --poly "$POLY" \
            >"$A_LOG" 2>&1 &
        PIDS="$PIDS $!"
        sleep 0.4
        if grep -q '^LISTENING ' "$A_LOG" 2>/dev/null && kill -0 "$!" 2>/dev/null; then
            return 0
        fi
        # Port probably taken: drop this attempt's pid and retry.
        local dead; dead=$(tail -1 <<<"$PIDS")
        kill "$dead" >/dev/null 2>&1 || true
        wait "$dead" >/dev/null 2>&1 || true
        PIDS="${PIDS%$dead}"
        PIDS="${PIDS% }"
    done
    fail "could not bind node A after several attempts"
}
launch_a
A_ADDR="$(sed -n 's/^LISTENING //p' "$A_LOG" | head -1)"
[ -n "$A_ADDR" ] || fail "node A never reported its address"
echo "node A listening on $A_ADDR"

"$BIN" daemon --role connect --addr "$A_ADDR" \
    --store "$B_STORE" --tree "$B_TREE" --tag node-b --poly "$POLY" \
    >"$B_LOG" 2>&1 &
PIDS="$PIDS $!"

# --- mutate: 50 random files + one exec script ----------------------------
echo "writing 50 random files..."
i=0
while [ "$i" -lt 50 ]; do
    d="$((RANDOM % 3))"
    rel="data$i"
    case "$d" in
        1) rel="sub$((RANDOM % 3))/data$i" ;;
        2) rel="deep$((RANDOM % 2))/sub$((RANDOM % 3))/data$i" ;;
    esac
    mkdir -p "$(dirname "$A_TREE/$rel")"
    size=$((RANDOM % 8192))
    if [ "$size" -gt 0 ]; then
        head -c "$size" /dev/urandom > "$A_TREE/$rel"
    else
        : > "$A_TREE/$rel"
    fi
    i=$((i + 1))
done
mkdir -p "$A_TREE/scripts"
printf '#!/bin/sh\necho skeleton\n' > "$A_TREE/scripts/run.sh"
chmod 755 "$A_TREE/scripts/run.sh"

# --- append-heavy log writer (runs WHILE sync converges) ------------------
TOTAL_LINES=250
LOG_REL="logs/app.log"
mkdir -p "$A_TREE/logs"
(
    j=0
    while [ "$j" -lt "$TOTAL_LINES" ]; do
        j=$((j + 1))
        printf 'e2e log line %03d payload-%04x\n' "$j" "$((j * 7919 % 65536))" >> "$A_TREE/$LOG_REL"
        sleep 0.01
    done
) &
WRITER_PID=$!

# --- helpers ---------------------------------------------------------------
last_field_of() { # file -> agreed= value from the newest STATE line (raw hex)
    awk '/ STATE /{for(k=1;k<=NF;k++) if ($k ~ /^agreed=/){v=substr($k,9)}} END{if(v=="")v="none"; print v}' "$1"
}
root_of() {
    awk '/ STATE /{for(k=1;k<=NF;k++) if ($k ~ /^root=/){v=substr($k,7)}} END{if(v=="")v="none"; print v}' "$1"
}
tree_listing() {
    (cd "$1" && find . -type f | LC_ALL=C sort | while IFS= read -r p; do cksum "$p"; done)
}

# The writer keeps appending while the daemons sync in the background.
# Convergence is asserted only once writes have STOPPED, so the receiving
# copy must contain every appended line.
wait "$WRITER_PID"

# --- poll until both manifest ids equal ------------------------------------
START=$SECONDS
DEADLINE=$((SECONDS + N_SECONDS))
AGREED_A="-"; AGREED_B="-"
converged=0
while [ "$SECONDS" -lt "$DEADLINE" ]; do
    AGREED_A="$(last_field_of "$A_LOG")"
    AGREED_B="$(last_field_of "$B_LOG")"
    ROOT_A="$(root_of "$A_LOG")"
    ROOT_B="$(root_of "$B_LOG")"
    if [ -n "$AGREED_A" ] && [ "$AGREED_A" = "$AGREED_B" ] \
       && [ -n "$ROOT_A" ] && [ "$ROOT_A" = "$ROOT_B" ] \
       && [ "${#AGREED_A}" -ge 32 ] && [ "${#ROOT_A}" -ge 32 ]; then
        converged=1
        break
    fi
    sleep 0.2
done

[ "$converged" = 1 ] || fail "manifest ids did not converge within ${N_SECONDS}s (a=$AGREED_A b=$AGREED_B)"
ELAPSED=$((SECONDS - START))
echo "manifest ids converged in <=${ELAPSED}s (agreed=$AGREED_A)"

# --- byte-for-byte verification incl. complete log tail --------------------
[ -f "$B_TREE/$LOG_REL" ] || fail "log missing on node B"
GOT_LINES="$(wc -l < "$B_TREE/$LOG_REL" | tr -d ' ')"
[ "$GOT_LINES" -eq "$TOTAL_LINES" ] || fail "torn log tail: expected $TOTAL_LINES lines, got $GOT_LINES"
cmp "$A_TREE/$LOG_REL" "$B_TREE/$LOG_REL" || fail "log bytes differ between nodes"
cmp "$A_TREE/scripts/run.sh" "$B_TREE/scripts/run.sh" || fail "exec script differs"
LAST_EXPECTED="$(printf 'e2e log line %03d payload-%04x' "$TOTAL_LINES" "$((TOTAL_LINES * 7919 % 65536))")"
LAST_GOT="$(tail -n 1 "$B_TREE/$LOG_REL")"
[ "$LAST_GOT" = "$LAST_EXPECTED" ] || fail "unexpected final log line: $LAST_GOT"

LIST_A="$(tree_listing "$A_TREE")"
LIST_B="$(tree_listing "$B_TREE")"
if [ "$LIST_A" != "$LIST_B" ]; then
    diff <(printf '%s\n' "$LIST_A") <(printf '%s\n' "$LIST_B") >&2 | head -20
    fail "trees differ between nodes"
fi

FILE_COUNT="$(find "$A_TREE" -type f | wc -l | tr -d ' ')"
[ "$FILE_COUNT" -ge 51 ] || fail "expected >=51 files, found $FILE_COUNT"

echo "OK: full tree byte-identical ($FILE_COUNT files incl. complete $TOTAL_LINES-line log)"
exit 0
