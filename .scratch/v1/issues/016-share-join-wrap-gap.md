# T-016: `share`/`join` short-code pairing never completes the sharer's key wrap

Status: ready-for-agent
Blocks: live two-device sync via the zero-config path

## Symptom (reproduced 2026-08-29, target/debug @ 19:2x)

`ferry share` (zero-file code transport, ticket 08) prints a short code and
returns immediately. `ferry join <CODE>` on the second device adopts the
folder — both sides end up with the same `folder_id` — but the sharer's
`CONFIG_HEAD` still contains only its own `device_pub` entry (1 entry vs
the joiner's 2). The sync engine seeds its allow-list from `CONFIG_HEAD`
wrapped-key entries (deny-unknown, `crates/ferry-sync/src/engine.rs`
`from_config_head`), so the sharer's daemon denies the joiner's handshake:
sessions fail forever, `status --json` on the joiner shows `"peers":[]`,
and no file ever converges.

The legacy file-offer flow does not have this gap: `ferry pair` polls for
the responder and completes the FMK wrap on both sides
(`crates/ferry-cli/src/commands/pairing.rs::initiate`), and the full
pair→daemon→converge path was verified working (byte-for-byte convergence,
exec bit, `.env` exclusion).

## Expected

After `join <CODE>`, the sharer's `CONFIG_HEAD` includes the joiner's
`device_pub` (either `join` writes a response the sharer picks up, or
`share` stays alive to complete the wrap like `pair` does), and two
daemons converge.

## Repro

```sh
mkdir -p /tmp/g/{a-h,b-h,a-t,b-t}
( cd /tmp/g/a-t && FERRY_HOME=/tmp/g/a-h ferry init && ferry share --json )  # note code
( cd /tmp/g/b-t && FERRY_HOME=/tmp/g/b-h ferry join <CODE> /tmp/g/b-t )
# entry counts in the two .ferry/config files differ: 1 vs 2
( cd /tmp/g/a-t && FERRY_HOME=/tmp/g/a-h ferry daemon --listen 127.0.0.1:44001 ) &
( cd /tmp/g/b-t && FERRY_HOME=/tmp/g/b-h ferry daemon --peer-url 127.0.0.1:44001 --interval-secs 1 ) &
echo hi > /tmp/g/a-t/x.txt   # never appears in /tmp/g/b-t
```

## Note

`scripts/quickstart-e2e.sh` waits for `.ferry/pair-offer.ferry-pair` after
`share` and is stale against the current CLI (share emits a code, not an
offer file); it needs `pair` like everything else.
