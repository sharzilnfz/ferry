#!/usr/bin/env bash

set -u

N_SECONDS="${1:-60}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
START_TS="$(date +%s)"

BIN=""
for cand in "$REPO_ROOT/target/debug/ferry-sync" "$REPO_ROOT/target/release/ferry-sync"; do
    if [ -x "$cand" ]; then BIN="$cand"; break; fi
done
if [ -z "$BIN" ]; then
    echo "building ferry-sync (debug)...">&2
    (cd "$REPO_ROOT" && cargo build -q -p ferry-daemon) >&2
    BIN="$REPO_ROOT/target/debug/ferry-sync"
fi

TMP="$(mktemp -d "/tmp/ferry-dashboard.XXXXXX")"
STORE_A="$TMP/node-a/store"; TREE_A="$TMP/node-a/tree"
STORE_B="$TMP/node-b/store"; TREE_B="$TMP/node-b/tree"
mkdir -p "$STORE_A" "$TREE_A" "$STORE_B" "$TREE_B"
LOG_A="$TMP/daemon-a.log"; LOG_B="$TMP/daemon-b.log"
PIDS=""

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

fail() {
    echo "FAIL: $*" >&2
    echo "--- node A log tail ---" >&2; tail -5 "$LOG_A" >&2 2>/dev/null || true
    echo "--- node B log tail ---" >&2; tail -5 "$LOG_B" >&2 2>/dev/null || true
    exit 1
}

step() { printf '\n== %s\n' "$1" >&2; }

pick_port() { echo $((20000 + RANDOM % 20000)); }

get_status() { # url out_file -> http_code
    curl -sS -m 3 -o "$2" -w '%{http_code}' "$1/api/status" 2>/dev/null || echo 000
}

post_json() { # url json_body out_file -> http_code
    curl -sS -m 5 -o "$3" -w '%{http_code}' -X POST \
        -H 'content-type: application/json' -d "$2" "$1" 2>/dev/null || echo 000
}

step "genpoly"
POLY="$("$BIN" genpoly)" || fail "genpoly"
echo "poly: $POLY"

step "boot node A (--role listen, own --ui)"
UI_PORT_A="$(pick_port)"; TCP_PORT_A="$(pick_port)"
attempt=0
while [ "$attempt" -lt 5 ]; do
    rm -f "$LOG_A"
    "$BIN" daemon --transport tcp --role listen \
        --addr "127.0.0.1:$TCP_PORT_A" \
        --store "$STORE_A" --tree "$TREE_A" --tag node-a --poly "$POLY" \
        --ui "127.0.0.1:$UI_PORT_A" \
        >"$LOG_A" 2>&1 &
    PIDS="$PIDS $!"
    sleep 0.6
    if grep -q '^LISTENING ' "$LOG_A" 2>/dev/null && grep -q '^UI LISTENING ' "$LOG_A" 2>/dev/null; then
        break
    fi
    dead=$(tail -1 <<<"$PIDS"); kill "$dead" >/dev/null 2>&1 || true
    wait "$dead" >/dev/null 2>&1 || true
    PIDS="${PIDS%"$dead"}"; PIDS="${PIDS% }"
    attempt=$((attempt + 1))
    TCP_PORT_A="$(pick_port)"; UI_PORT_A="$(pick_port)"
done
[ "$attempt" -lt 5 ] || fail "could not bind daemon A (tcp or ui port)"
A_ADDR="$(sed -n 's/^LISTENING //p' "$LOG_A" | head -1)"
[ -n "$A_ADDR" ] || fail "no LISTENING address from A"
UI_A="http://127.0.0.1:$UI_PORT_A"
echo "node A sync=$A_ADDR ui=$UI_A"

step "boot node B (--role connect, own --ui)"
UI_PORT_B="$(pick_port)"
attempt=0
while [ "$attempt" -lt 5 ]; do
    rm -f "$LOG_B"
    "$BIN" daemon --transport tcp --role connect --addr "$A_ADDR" \
        --store "$STORE_B" --tree "$TREE_B" --tag node-b --poly "$POLY" \
        --ui "127.0.0.1:$UI_PORT_B" \
        >"$LOG_B" 2>&1 &
    PIDS="$PIDS $!"
    sleep 0.6
    if grep -q '^UI LISTENING ' "$LOG_B" 2>/dev/null && kill -0 "$(tail -1 <<<"$PIDS")" 2>/dev/null; then
        break
    fi
    dead=$(tail -1 <<<"$PIDS"); kill "$dead" >/dev/null 2>&1 || true
    wait "$dead" >/dev/null 2>&1 || true
    PIDS="${PIDS%"$dead"}"; PIDS="${PIDS% }"
    attempt=$((attempt + 1))
    UI_PORT_B="$(pick_port)"
done
[ "$attempt" -lt 5 ] || fail "could not start daemon B"
UI_B="http://127.0.0.1:$UI_PORT_B"
echo "node A sync=$A_ADDR ui=$UI_A"
echo "node B sync=(dialed $A_ADDR) ui=$UI_B"

step "wait out warming-up: /api/status 200 on BOTH dashboards"
READY_A=0; READY_B=0
deadline=$(( SECONDS + (N_SECONDS * 2) / 5 ))
while [ "$SECONDS" -lt "$deadline" ]; do
    if [ "$READY_A" = 0 ]; then
        code="$(get_status "$UI_A" "$TMP/status-a.json")"
        [ "$code" = "200" ] && READY_A=1
    fi
    if [ "$READY_B" = 0 ]; then
        code="$(get_status "$UI_B" "$TMP/status-b.json")"
        [ "$code" = "200" ] && READY_B=1
    fi
    [ "$READY_A" = 1 ] && [ "$READY_B" = 1 ] && break
    sleep 0.4
done
[ "$READY_A" = 1 ] || fail "node A /api/status never left warming-up (last code ${code:-?})"
[ "$READY_B" = 1 ] || fail "node B /api/status never left warming-up"
echo "both dashboards serving live status"

step "drop a file with known bytes into tree A"
REL="e2e/live-probe.txt"
KNOWN_BYTES="dashboard-e2e $(date +%s) ferry-sync convergence probe"
mkdir -p "$TREE_A/$(dirname "$REL")"
printf '%s\n' "$KNOWN_BYTES" > "$TREE_A/$REL"

step "wait for byte convergence of $REL in BOTH trees"
CONVERGED=0
deadline=$(( SECONDS + (N_SECONDS * 2) / 5 ))
while [ "$SECONDS" -lt "$deadline" ]; do
    if [ -f "$TREE_B/$REL" ] && cmp -s "$TREE_A/$REL" "$TREE_B/$REL"; then
        CONVERGED=1; break
    fi
    sleep 0.3
done
if [ "$CONVERGED" != 1 ]; then
    ls -la "$TREE_B/e2e" >&2 2>/dev/null || echo "(tree B has no e2e dir yet)" >&2
    cmp "$TREE_A/$REL" "$TREE_B/$REL" >&2 || true
    fail "file did not byte-converge into tree B within budget"
fi
echo "converged byte-for-byte: $(wc -c < "$TREE_B/$REL" | tr -d ' ') bytes identical"

step "assert agreement green in BOTH /api/status documents"
AGREED=0
deadline=$(( SECONDS + (N_SECONDS * 2) / 5 ))
while [ "$SECONDS" -lt "$deadline" ]; do
    get_status "$UI_A" "$TMP/status-a.json" >/dev/null
    get_status "$UI_B" "$TMP/status-b.json" >/dev/null
    if python3 - "$TMP/status-a.json" "$TMP/status-b.json" <<'PYEOF' 2>/dev/null; then
import json, re, sys

HEX64 = re.compile(r"^[0-9a-f]{64}$")
docs = []
for path in sys.argv[1:3]:
    with open(path) as f:
        docs.append(json.load(f))

for label, d in (("A", docs[0]), ("B", docs[1])):
    root = d.get("manifest_id")
    assert isinstance(root, str) and HEX64.match(root), f"{label}: manifest_id missing/not hex64: {root!r}"
    pending = d.get("pending_changes")
    assert pending == 0, f"{label}: pending_changes expected 0 (settled agreement), got {pending!r}"
    peers = d.get("peers")
    assert isinstance(peers, list) and len(peers) >= 1, f"{label}: peers empty — no agreement recorded: {peers!r}"
    for p in peers:
        agreed = p.get("last_agreed_manifest_id")
        assert isinstance(agreed, str) and HEX64.match(agreed), \
            f"{label}: peer last_agreed_manifest_id not hex64: {agreed!r}"

assert docs[0]["manifest_id"] == docs[1]["manifest_id"], (
    f"roots disagree across nodes: A={docs[0]['manifest_id']} B={docs[1]['manifest_id']}")
print("agreement OK: roots equal, pending_changes=0, peers present on both nodes")
print(f"  root A/B: {docs[0]['manifest_id']}")
print(f"  peers A: {[p['device_id'][:8] for p in docs[0]['peers']]}")
print(f"  peers B: {[p['device_id'][:8] for p in docs[1]['peers']]}")
PYEOF
        AGREED=1; break
    fi
    sleep 0.4
done
if [ "$AGREED" != 1 ]; then
    echo "--- last /api/status from node A ---" >&2; cat "$TMP/status-a.json" >&2
    echo "--- last /api/status from node B ---" >&2; cat "$TMP/status-b.json" >&2
    fail "agreement never went green in both /api/status within budget"
fi

step "POST round-trip: /api/pin/start then /api/pin/stop on node A"
code="$(post_json "$UI_A/api/pin/start" '{"folder":null,"paths":null}' "$TMP/pin-start.json")"
[ "$code" = "200" ] || { cat "$TMP/pin-start.json" >&2; fail "pin/start returned HTTP $code (want 200)"; }
python3 - "$TMP/pin-start.json" <<'PYEOF' || fail "pin/start document assertion failed"
import json, sys
with open(sys.argv[1]) as f:
    d = json.load(f)
assert d.get("command") == "pin", d
assert d.get("action") == "start", d
assert isinstance(d.get("pid"), int) and d["pid"] > 0, d
assert d.get("paths") == ["*"], d
print("pin/start OK:", json.dumps(d)[:160])
PYEOF

code="$(post_json "$UI_A/api/pin/stop" '{"folder":null}' "$TMP/pin-stop.json")"
[ "$code" = "200" ] || { cat "$TMP/pin-stop.json" >&2; fail "pin/stop returned HTTP $code (want 200)"; }
python3 - "$TMP/pin-stop.json" <<'PYEOF' || fail "pin/stop document assertion failed"
import json, sys
with open(sys.argv[1]) as f:
    d = json.load(f)
assert d.get("command") == "pin", d
assert d.get("action") == "stop", d
assert d.get("was_pinned") is True, d
print("pin/stop OK:", json.dumps(d)[:160])
PYEOF

TOTAL_SECS=$(( $(date +%s) - START_TS ))
[ "$TOTAL_SECS" -le "$((N_SECONDS + 30))" ] || fail "took ${TOTAL_SECS}s (budget blown)"
echo ""
echo "PASS: dashboard e2e converged in ${TOTAL_SECS}s"
echo "PASS: node A dashboard $UI_A"
echo "PASS: node B dashboard $UI_B"
exit 0
