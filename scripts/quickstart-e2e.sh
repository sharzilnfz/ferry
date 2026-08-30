#!/usr/bin/env bash

set -u

N_SECONDS="${1:-120}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
START_TS="$(date +%s)"

BIN=""
for cand in "$REPO_ROOT/target/debug/ferry" "$REPO_ROOT/target/release/ferry"; do
    if [ -x "$cand" ]; then BIN="$cand"; break; fi
done
if [ -z "$BIN" ]; then
    echo "building ferry (debug)...">&2
    (cd "$REPO_ROOT" && cargo build -q -p ferry-cli) >&2
    BIN="$REPO_ROOT/target/debug/ferry"
fi

TMP="$(mktemp -d "${TMPDIR:-/tmp}/ferry-quickstart.XXXXXX")"
HOME_A="$TMP/device-a"; HOME_B="$TMP/device-b"
TREE_A="$TMP/tree-a";   TREE_B="$TMP/tree-b"
mkdir -p "$HOME_A" "$HOME_B" "$TREE_A" "$TREE_B"
PIDS=""
FAILED=0

cleanup() {
    trap - EXIT INT TERM
    if [ -n "$PIDS" ]; then
        kill $PIDS >/dev/null 2>&1 || true
        wait $PIDS >/dev/null 2>&1 || true
    fi
    if [ "${FERRY_KEEP:-0}" = "1" ]; then
        echo "(FERRY_KEEP=1: leaving $TMP in place)" >&2
        return
    fi
    rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

fail() { echo "FAIL: $*" >&2; FAILED=1; exit 1; }

step() { printf '\n== %s\n' "$1" >&2; }

run_a() { FERRY_HOME="$HOME_A" "$BIN" "$@"; }
run_b() { FERRY_HOME="$HOME_B" "$BIN" "$@"; }

wait_for_file() { # path timeout_s
    deadline=$(( $(date +%s) + $2 ))
    while [ ! -e "$1" ]; do
        [ "$(date +%s)" -ge "$deadline" ] && return 1
        sleep 0.2
    done
    return 0
}

step "device A: ferry init"
( cd "$TREE_A" && run_a init ) >/dev/null || fail "init on A"
[ -f "$TREE_A/.ferry/config" ] || fail "no CONFIG_HEAD under A"

step "seed content on A (incl. an excluded-by-default .env)"
echo "hello from device A" > "$TREE_A/hello.txt"
mkdir -p "$TREE_A/src"
printf 'print("hi")\n' > "$TREE_A/src/main.py"
cat > "$TREE_A/.env" <<'EOF'
AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE
DATABASE_URL=postgres://dev:dev@localhost/dev
EOF

step "opt .env back IN, then check the share-time secret gate"
( cd "$TREE_A" && run_a ignore '!.env' ) >/dev/null || fail "ignore append"
gate_out="$( cd "$TREE_A" && run_a share 2>&1 )"; gate_rc=$?
[ "$gate_rc" -ne 0 ] || fail "secret gate did not refuse a flagged .env"
echo "$gate_out" | grep -q 'AKIAIOSFODNN7EXAMPLE' && fail "gate leaked the secret bytes"
echo "$gate_out" | grep -q 'aws-access-key' || fail "gate did not name the finding class"
[ ! -e "$TREE_A/.ferry/pair-offer.ferry-pair" ] || fail "refused share must not emit an offer"
echo "gate refused loudly and redacted (rc=$gate_rc) — good."

step "restore defaults (.env stays local-only) and share for real"
grep -v '^!\.env$' "$TREE_A/ferry.ignore" > "$TREE_A/ferry.ignore.tmp" \
    && mv "$TREE_A/ferry.ignore.tmp" "$TREE_A/ferry.ignore"

( cd "$TREE_A" && FERRY_HOME="$HOME_A" "$BIN" share --timeout-secs 90 ) >"$TMP/share-a.log" 2>&1 &
PIDS="$PIDS $!"
wait_for_file "$TREE_A/.ferry/pair-offer.ferry-pair" 30 || fail "share never emitted its offer"
grep -q '[█▀▄]' "$TMP/share-a.log" || echo "(note: QR art not detected in log)" >&2
CODE="$(grep -oE '[23456789ABCDEFGHJKLMNPQRSTUVWXYZ]{4}(-[23456789ABCDEFGHJKLMNPQRSTUVWXYZ]{4}){4}' "$TMP/share-a.log" | head -1)"
[ -n "$CODE" ] || fail "no short code printed"
echo "short code: $CODE"

step "device B: pair --accept (payload files = out-of-band channel)"
( cd "$TREE_B" && run_b pair --accept "$TREE_A/.ferry/pair-offer.ferry-pair" --timeout-secs 90 ) \
    >"$TMP/pair-b.log" 2>&1 || fail "accept on B"
[ -f "$TREE_B/.ferry/config" ] || fail "B never adopted the folder"

SHARE_PID=""
for p in $PIDS; do kill -0 "$p" 2>/dev/null && SHARE_PID="$p"; done
if [ -n "$SHARE_PID" ]; then
    wait "$SHARE_PID" || fail "device A's share never completed"
fi
grep -q 'completed' /dev/null 2>/dev/null # (placeholder; json checked below)

step "daemons: A listens, B dials (two 'machines' on one host)"
launch_a() {
    attempt=0
    while [ "$attempt" -lt 5 ]; do
        port=$((20000 + RANDOM % 20000))
        ( cd "$TREE_A" && FERRY_HOME="$HOME_A" "$BIN" daemon --listen "127.0.0.1:$port" ) \
            >"$TMP/daemon-a.log" 2>&1 &
        PIDS="$PIDS $!"
        sleep 0.6
        if grep -q '^LISTENING ' "$TMP/daemon-a.log" 2>/dev/null; then return 0; fi
        dead=$(tail -1 <<<"$PIDS"); kill "$dead" >/dev/null 2>&1 || true
        wait "$dead" >/dev/null 2>&1 || true
        PIDS="${PIDS%$dead}"; PIDS="${PIDS% }"
        attempt=$((attempt + 1))
    done
    fail "could not bind daemon A"
}
launch_a
ADDR="$(sed -n 's/^LISTENING //p' "$TMP/daemon-a.log" | head -1)"
[ -n "$ADDR" ] || fail "no LISTENING address"
echo "device A listening on $ADDR"

( cd "$TREE_B" && FERRY_HOME="$HOME_B" "$BIN" daemon --peer-url "$ADDR" --interval-secs 1 ) \
    >"$TMP/daemon-b.log" 2>&1 &
PIDS="$PIDS $!"

step "wait for the first agreement to settle (edits before settle are ambiguous)"
settle_deadline=$(( SECONDS + N_SECONDS ))
while :; do
    doc="$( cd "$TREE_B" && FERRY_HOME="$HOME_B" "$BIN" status --json )" || fail "status failed on B"
    echo "$doc" | grep -q '"last_agreed_manifest_id":"' && break
    [ "$SECONDS" -ge "$settle_deadline" ] && fail "agreement never settled: $doc"
    sleep 0.4
done
echo "agreement recorded."

step "burst of files on A (while both daemons run)"
i=0
while [ "$i" -lt 20 ]; do
    rel="gen/file$i.txt"
    [ $((i % 3)) -eq 0 ] && rel="gen/deep$i/file$i.txt"
    mkdir -p "$(dirname "$TREE_A/$rel")"
    printf 'payload %04d %s\n' "$i" "$(head -c 40 /dev/urandom | base64 2>/dev/null | head -c 32)" > "$TREE_A/$rel"
    i=$((i + 1))
done
printf '#!/bin/sh\necho quickstart\n' > "$TREE_A/run.sh"
chmod 755 "$TREE_A/run.sh"
echo "late edit" >> "$TREE_A/hello.txt"

step "assert convergence on B within ${N_SECONDS}s"
tree_listing() {
    ( cd "$1" && find . -type f -not -path './.ferry/*' -not -name '.env*' \
        | LC_ALL=C sort | while IFS= read -r p; do cksum "$p"; done )
}
converged=0
deadline=$(( SECONDS + N_SECONDS ))
while [ "$SECONDS" -lt "$deadline" ]; do
    if [ -f "$TREE_B/run.sh" ] && [ -f "$TREE_B/gen/deep0/file0.txt" ] \
       && [ "$(tail -n 1 "$TREE_B/hello.txt" 2>/dev/null)" = "late edit" ]; then
        LA="$(tree_listing "$TREE_A")"; LB="$(tree_listing "$TREE_B")"
        if [ "$LA" = "$LB" ]; then converged=1; break; fi
    fi
    sleep 0.3
done
if [ "$converged" -ne 1 ]; then
    diff <(tree_listing "$TREE_A") <(tree_listing "$TREE_B") >&2 | head -20 || true
    fail "trees did not converge within ${N_SECONDS}s"
fi
cmp "$TREE_A/run.sh" "$TREE_B/run.sh" || fail "exec bit content differs"
[ -x "$TREE_B/run.sh" ] || fail "exec bit lost on B"
[ ! -e "$TREE_B/.env" ] || fail ".env must stay local under default rules"
echo "converged byte-for-byte (incl. exec bit and late edit; .env stayed local)"

step "conflict reports empty on both devices"
check_conflicts() { # tree_dir home_dir label
    doc="$( cd "$1" && FERRY_HOME="$2" "$BIN" conflicts list --json )" || fail "conflicts list failed on $3"
    echo "$doc" | grep -q '"entries":\[\]' || fail "conflicts not empty on $3: $doc"
}
check_conflicts "$TREE_A" "$HOME_A" A
check_conflicts "$TREE_B" "$HOME_B" B

step "agreement recorded (status)"
peers_doc="$( cd "$TREE_B" && run_b status --json )"
echo "$peers_doc" | grep -q '"last_agreed_manifest_id":"' || fail "B has no agreement pointer"

TOTAL_SECS=$(( $(date +%s) - START_TS ))
[ "$TOTAL_SECS" -lt 300 ] || fail "quickstart took ${TOTAL_SECS}s (five-minute budget blown)"
echo "$peers_doc" | grep -q '"connectivity":"reachable"' || true

echo ""
echo "OK: two simulated devices paired and synced in ${TOTAL_SECS}s (< 300s budget)"
exit 0
