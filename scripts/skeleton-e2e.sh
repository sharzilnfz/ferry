#!/usr/bin/env bash

set -u

N_SECONDS="${1:-60}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

FERRY_BIN=""
RELAY_BIN=""
for kind in debug release; do
    if [ -z "$FERRY_BIN" ] && [ -x "$REPO_ROOT/target/$kind/ferry-sync" ]; then
        FERRY_BIN="$REPO_ROOT/target/$kind/ferry-sync"
    fi
    if [ -z "$RELAY_BIN" ] && [ -x "$REPO_ROOT/target/$kind/ferry-relay" ]; then
        RELAY_BIN="$REPO_ROOT/target/$kind/ferry-relay"
    fi
done
if [ -z "$FERRY_BIN" ]; then
    echo "building ferry-sync (debug)..." >&2
    (cd "$REPO_ROOT" && cargo build -q -p ferry-daemon) >&2
    FERRY_BIN="$REPO_ROOT/target/debug/ferry-sync"
fi
if [ -z "$RELAY_BIN" ]; then
    echo "building ferry-relay (debug)..." >&2
    (cd "$REPO_ROOT" && cargo build -q -p ferry-relay) >&2
    RELAY_BIN="$REPO_ROOT/target/debug/ferry-relay"
fi

TMP="$(mktemp -d "${TMPDIR:-/tmp}/ferry-e2e.XXXXXX")"
PIDS=""
MODES_LOG="$TMP/modes.log"

cleanup() {
    trap - EXIT INT TERM
    if [ -n "$PIDS" ]; then
        kill $PIDS >/dev/null 2>&1 || true
        wait $PIDS >/dev/null 2>&1 || true
    fi
    if [ "${FERRY_E2E_KEEP_TMP:-0}" != "1" ]; then
        rm -rf "$TMP"
    else
        echo "keeping tempdir: $TMP (logs inside)" >&2
    fi
}
trap cleanup EXIT INT TERM

fail() { echo "FAIL: $*" >&2; exit 1; }

POLY="$("$FERRY_BIN" genpoly --seed 20260824)" || fail "genpoly"

last_field_of() { # file -> value of key= from the newest STATE line
    awk '/ STATE /{for(k=1;k<=NF;k++) if ($k ~ /^'"$2"'=/){v=substr($k,length("'"$2"'")+2)}} END{if(v=="")v="none"; print v}' "$1"
}
tree_listing() {
    (cd "$1" && find . -type f | LC_ALL=C sort | while IFS= read -r p; do cksum "$p"; done)
}

wait_converged() { # a_log b_log tree_a tree_b deadline_s -> sets CONVERGED=1/0, AGREED
    local a_log="$1" b_log="$2" ta="$3" tb="$4" budget="$5"
    START=$SECONDS
    DEADLINE=$((SECONDS + budget))
    CONVERGED=0
    AGREED="-"
    while [ "$SECONDS" -lt "$DEADLINE" ]; do
        AGREED_A="$(last_field_of "$a_log" agreed)"
        AGREED_B="$(last_field_of "$b_log" agreed)"
        if [ -n "$AGREED_A" ] && [ "$AGREED_A" = "$AGREED_B" ] \
           && [ "$AGREED_A" != "none" ] && [ "${#AGREED_A}" -ge 32 ]; then
            if [ -f "$tb/logs/app.log" ] && [ "$(wc -l < "$tb/logs/app.log" | tr -d ' ')" -eq 250 ]; then
                CONVERGED=1
                AGREED="$AGREED_A"
                break
            fi
        fi
        sleep 0.2
    done
}

verify_trees() { # tree_a tree_b log_lines total_files_min
    local ta="$1" tb="$2" lines="$3" minfiles="$4" rel="logs/app.log" tries=0
    [ -f "$tb/$rel" ] || fail "[${MODE}] log missing on node B"
    while [ "$tries" -lt 150 ]; do
        GOT_LINES="$(wc -l < "$tb/$rel" | tr -d ' ')"
        [ "$GOT_LINES" -eq "$lines" ] && break
        sleep 0.2
        tries=$((tries + 1))
    done
    [ "$GOT_LINES" -eq "$lines" ] || fail "[${MODE}] torn log tail never settled within 30s: expected $lines lines, got $GOT_LINES"
    cmp "$ta/$rel" "$tb/$rel" || fail "[${MODE}] log bytes differ between nodes"
    cmp "$ta/scripts/run.sh" "$tb/scripts/run.sh" || fail "[${MODE}] exec script differs"
    LAST_EXPECTED="$(printf 'e2e log line %03d payload-%04x' "$lines" "$((lines * 7919 % 65536))")"
    LAST_GOT="$(tail -n 1 "$tb/$rel")"
    [ "$LAST_GOT" = "$LAST_EXPECTED" ] || fail "[${MODE}] unexpected final log line: $LAST_GOT"

    LIST_A="$(tree_listing "$ta")"
    LIST_B="$(tree_listing "$tb")"
    if [ "$LIST_A" != "$LIST_B" ]; then
        diff <(printf '%s\n' "$LIST_A") <(printf '%s\n' "$LIST_B") >&2 | head -20
        fail "[${MODE}] trees differ between nodes"
    fi
    FILE_COUNT="$(find "$ta" -type f | wc -l | tr -d ' ')"
    [ "$FILE_COUNT" -ge "$minfiles" ] || fail "[${MODE}] expected >=${minfiles} files, found $FILE_COUNT"
}

populate_tree() { # tree_a  -> writes the shared scenario content
    local ta="$1" i d rel size
    i=0
    while [ "$i" -lt 50 ]; do
        d=$((RANDOM % 3))
        rel="data$i"
        case "$d" in
            1) rel="sub$((RANDOM % 3))/data$i" ;;
            2) rel="deep$((RANDOM % 2))/sub$((RANDOM % 3))/data$i" ;;
        esac
        mkdir -p "$(dirname "$ta/$rel")"
        size=$((RANDOM % 8192))
        if [ "$size" -gt 0 ]; then
            head -c "$size" /dev/urandom > "$ta/$rel"
        else
            : > "$ta/$rel"
        fi
        i=$((i + 1))
    done
    mkdir -p "$ta/scripts"
    printf '#!/bin/sh\necho skeleton\n' > "$ta/scripts/run.sh"
    chmod 755 "$ta/scripts/run.sh"
}

append_log_while_syncing() { # tree_a -> sets WRITER_PID
    local ta="$1" j=0
    mkdir -p "$ta/logs"
    (
        j=0
        while [ "$j" -lt "$TOTAL_LINES" ]; do
            j=$((j + 1))
            printf 'e2e log line %03d payload-%04x\n' "$j" "$((j * 7919 % 65536))" >> "$ta/$LOG_REL"
            sleep 0.01
        done
    ) &
    WRITER_PID=$!
}

TOTAL_LINES=250
LOG_REL="logs/app.log"

MODE=tcp
A_TREE="$TMP/tcp-a/tree"; A_STORE="$TMP/tcp-a/store"
B_TREE="$TMP/tcp-b/tree"; B_STORE="$TMP/tcp-b/store"
mkdir -p "$A_TREE" "$B_TREE" "$A_STORE" "$B_STORE"
A_LOG="$TMP/node-a-tcp.log"; B_LOG="$TMP/node-b-tcp.log"

launch_a_tcp() {
    local attempt port
    for attempt in 1 2 3 4 5; do
        port=$((20000 + RANDOM % 20000))
        "$FERRY_BIN" daemon --transport tcp --role listen --addr "127.0.0.1:$port" \
            --store "$A_STORE" --tree "$A_TREE" --tag node-a --poly "$POLY" \
            >"$A_LOG" 2>&1 &
        PIDS="$PIDS $!"
        sleep 0.4
        if grep -q '^LISTENING ' "$A_LOG" 2>/dev/null && kill -0 "$!" 2>/dev/null; then
            return 0
        fi
        local dead; dead=$(tail -1 <<<"$PIDS")
        kill "$dead" >/dev/null 2>&1 || true
        wait "$dead" >/dev/null 2>&1 || true
        PIDS="${PIDS%$dead}"
        PIDS="${PIDS% }"
    done
    fail "could not bind node A after several attempts"
}
launch_a_tcp
A_ADDR="$(sed -n 's/^LISTENING //p' "$A_LOG" | head -1)"
[ -n "$A_ADDR" ] || fail "[tcp] node A never reported its address"
echo "== mode tcp: node A listening on $A_ADDR"

"$FERRY_BIN" daemon --transport tcp --role connect --addr "$A_ADDR" \
    --store "$B_STORE" --tree "$B_TREE" --tag node-b --poly "$POLY" \
    >"$B_LOG" 2>&1 &
PIDS="$PIDS $!"

populate_tree "$A_TREE"
append_log_while_syncing "$A_TREE"
wait "$WRITER_PID"

wait_converged "$A_LOG" "$B_LOG" "$A_TREE" "$B_TREE" "$N_SECONDS"
[ "$CONVERGED" = 1 ] || fail "[tcp] manifest ids did not converge within ${N_SECONDS}s (a=$AGREED_A b=$AGREED_B)"
TCP_AGREED="$AGREED"
ELAPSED=$((SECONDS - START))
echo "== mode tcp: converged in <=${ELAPSED}s (agreed=$TCP_AGREED)"
verify_trees "$A_TREE" "$B_TREE" "$TOTAL_LINES" 51
grep -q "encrypted=yes" "$A_LOG" || fail "[tcp] node A never ran an ENCRYPTED v1 session"
grep -q "encrypted=yes" "$B_LOG" || fail "[tcp] node B never ran an ENCRYPTED v1 session"
if grep -q "encrypted=no" "$A_LOG" "$B_LOG"; then
    fail "[tcp] a session ran with encryption OFF; default must be sealed"
fi
echo "== mode tcp: OK, full tree byte-identical, v1 sealed sessions verified"
echo "tcp agreed=$TCP_AGREED" >> "$MODES_LOG"

PIDS_A="$PIDS"; PIDS=""
for p in $PIDS_A; do kill "$p" >/dev/null 2>&1 || true; wait "$p" 2>/dev/null || true; done

MODE=iroh
A_TREE="$TMP/iroh-a/tree"; A_STORE="$TMP/iroh-a/store"
B_TREE="$TMP/iroh-b/tree"; B_STORE="$TMP/iroh-b/store"
mkdir -p "$A_TREE" "$B_TREE" "$A_STORE" "$B_STORE"
A_LOG="$TMP/node-a-iroh.log"; B_LOG="$TMP/node-b-iroh.log"
RELAY_LOG="$TMP/relay-iroh.log"

"$RELAY_BIN" --http-bind 127.0.0.1:0 >"$RELAY_LOG" 2>&1 &
PIDS="$!"
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
    sleep 0.5
    RELAY_URL="$(sed -n 's/^RELAY //p' "$RELAY_LOG" | head -1)"
    [ -n "$RELAY_URL" ] && break
done
[ -n "${RELAY_URL:-}" ] || fail "[iroh] ferry-relay never reported its URL"
echo "== mode iroh: relay at $RELAY_URL"

"$FERRY_BIN" daemon --transport iroh --role listen \
    --store "$A_STORE" --tree "$A_TREE" --tag node-a --poly "$POLY" \
    --relay "$RELAY_URL" \
    >"$A_LOG" 2>&1 &
PIDS="$PIDS $!"
for _ in 1 2 3 4 5 6 7 8 9 10; do
    sleep 0.4
    grep -q '^ENDPOINT ' "$A_LOG" 2>/dev/null && break
done
A_ENDPOINT="$(sed -n 's/^ENDPOINT //p' "$A_LOG" | head -1)"
[ -n "$A_ENDPOINT" ] || fail "[iroh] node A never announced ENDPOINT id"
echo "== mode iroh: node A endpoint ${A_ENDPOINT%%\ *}..."

"$FERRY_BIN" daemon --transport iroh --role connect --peer "$A_ENDPOINT" \
    --store "$B_STORE" --tree "$B_TREE" --tag node-b --poly "$POLY" \
    --relay "$RELAY_URL" \
    >"$B_LOG" 2>&1 &
PIDS="$PIDS $!"

populate_tree "$A_TREE"
append_log_while_syncing "$A_TREE"
wait "$WRITER_PID"

IROH_BUDGET=$((N_SECONDS + 15))
wait_converged "$A_LOG" "$B_LOG" "$A_TREE" "$B_TREE" "$IROH_BUDGET"
[ "$CONVERGED" = 1 ] || fail "[iroh] manifest ids did not converge within ${IROH_BUDGET}s (a=$AGREED_A b=$AGREED_B)"
IROH_AGREED="$AGREED"
ELAPSED=$((SECONDS - START))
echo "== mode iroh: converged in <=${ELAPSED}s (agreed=$IROH_AGREED)"
verify_trees "$A_TREE" "$B_TREE" "$TOTAL_LINES" 51
grep -q "encrypted=yes" "$A_LOG" || fail "[iroh] node A never ran an ENCRYPTED v1 session"
grep -q "encrypted=yes" "$B_LOG" || fail "[iroh] node B never ran an ENCRYPTED v1 session"
if grep -q "encrypted=no" "$A_LOG" "$B_LOG"; then
    fail "[iroh] a session ran with encryption OFF; default must be sealed"
fi
echo "== mode iroh: OK, full tree byte-identical"

MARKERS_FOUND=0
for needle in "e2e log line" "payload-" "#!/bin/sh" "skeleton"; do
    if grep -qF -- "$needle" "$RELAY_LOG" 2>/dev/null; then
        echo "FAIL: [iroh] PLAINTEXT MARKER '$needle' visible in relay output" >&2
        MARKERS_FOUND=1
    fi
done
[ "$MARKERS_FOUND" = 0 ] || fail "[iroh] relay saw plaintext it must not see"
grep -q "^RELAY " "$RELAY_LOG" || fail "[iroh] relay log unexpectedly empty (scan would be vacuous)"
echo "== mode iroh: relay-side output contains no transferred plaintext"
echo "iroh agreed=$IROH_AGREED" >> "$MODES_LOG"

echo ""
echo "OK: both transports converge identically ($MODES_LOG):"
cat "$MODES_LOG"
exit 0
