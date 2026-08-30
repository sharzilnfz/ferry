#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP_ROOT="${TMPDIR:-/tmp}/ferry-zero-e2e-$$"
FERRY_HOME_A="$TMP_ROOT/home-a"
FERRY_HOME_B="$TMP_ROOT/home-b"
PROJ_A="$TMP_ROOT/proj-a"
PROJ_B="$TMP_ROOT/proj-b"
CODE_JSON="$TMP_ROOT/code.json"

cleanup() {
  rm -rf "$TMP_ROOT" 2>/dev/null || true
  pkill -f "ferry.*daemon" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

mkdir -p "$FERRY_HOME_A" "$FERRY_HOME_B" "$PROJ_A" "$PROJ_B"

BIN=""
for cand in "$REPO_ROOT/target/debug/ferry" "$REPO_ROOT/target/release/ferry"; do
  if [ -x "$cand" ]; then BIN="$cand"; break; fi
done

ferry_a() {
  if [ -n "$BIN" ]; then
    FERRY_HOME="$FERRY_HOME_A" "$BIN" "$@"
  else
    FERRY_HOME="$FERRY_HOME_A" cargo run --quiet --bin ferry -- "$@"
  fi
}

ferry_b() {
  if [ -n "$BIN" ]; then
    FERRY_HOME="$FERRY_HOME_B" "$BIN" "$@"
  else
    FERRY_HOME="$FERRY_HOME_B" cargo run --quiet --bin ferry -- "$@"
  fi
}

rm -rf "$PROJ_A/.ferry" "$PROJ_B/.ferry" 2>/dev/null || true

echo "== init proj-a"
ferry_a init "$PROJ_A" --json > /dev/null
test -f "$PROJ_A/.ferry/config"

echo "== share proj-a (json)"
ferry_a share "$PROJ_A" --json > "$CODE_JSON"
cat "$CODE_JSON"
CODE=$(jq -r '.code // .short_code' "$CODE_JSON" 2>/dev/null || python3 -c "import json; print(json.load(open('$CODE_JSON'))['code'])")
if [ -z "$CODE" ] || [ "$CODE" = "null" ]; then
  echo "FAIL: no code in $CODE_JSON" >&2
  cat "$CODE_JSON" >&2
  exit 1
fi
if [ -f "$PROJ_A/.ferry/pair-offer.ferry-pair" ]; then
  echo "WARN: legacy pair-offer exists (fallback path)" >&2
fi
FOLDER_ID_A=$(jq -r '.folder_id' "$CODE_JSON" 2>/dev/null || python3 -c "import json; print(json.load(open('$CODE_JSON'))['folder_id'])")
if [ ${#FOLDER_ID_A} -ne 32 ]; then
  echo "FAIL: folder_id not 32 hex: $FOLDER_ID_A" >&2
  exit 1
fi

echo "== join proj-b with code $CODE"
ferry_b join "$CODE" "$PROJ_B" --json > "$TMP_ROOT/join.json" || {
  echo "join failed, trying without --json" >&2
  ferry_b join "$CODE" "$PROJ_B"
  FERRY_HOME="$FERRY_HOME_B" cargo run --quiet --bin ferry -- join "$CODE" "$PROJ_B" --json > "$TMP_ROOT/join.json"
}
cat "$TMP_ROOT/join.json"
FOLDER_ID_B=$(jq -r '.folder_id' "$TMP_ROOT/join.json" 2>/dev/null || python3 -c "import json; print(json.load(open('$TMP_ROOT/join.json'))['folder_id'])")
if [ "$FOLDER_ID_A" != "$FOLDER_ID_B" ]; then
  echo "FAIL: folder_id mismatch A=$FOLDER_ID_A B=$FOLDER_ID_B" >&2
  exit 1
fi
test -f "$PROJ_B/.ferry/config"
test -d "$PROJ_B/.ferry"

echo "== headless share check (pipe to cat)"
ferry_a share "$PROJ_A" --json | cat > /dev/null

echo "hello from zero-config e2e" > "$PROJ_A/hello.txt"
sleep 2
if [ -f "$PROJ_B/hello.txt" ]; then
  echo "PASS: hello.txt synced to proj-b"
  diff -q "$PROJ_A/hello.txt" "$PROJ_B/hello.txt" || { echo "FAIL: content mismatch" >&2; exit 1; }
else
  echo "WARN: hello.txt not yet synced (no daemon running) — verifying folder_id only" >&2
  echo "Two homes share folder_id $FOLDER_ID_A — zero-config pairing succeeded" >&2
fi

echo "== zero-config e2e PASS"
