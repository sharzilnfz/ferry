# NAT acceptance validation — T-009

**Status: MANUAL-UNRUN.** The verbatim T-009 acceptance —

> two machines behind separate home NATs sync through relay then upgrade or
> stay direct per iroh's negotiation; relay logs contain no plaintext.

— cannot execute on a single dev machine. Everything runnable locally HAS run
(see "Local evidence already executed" below); this document is the precise
runbook for the real thing. Budget ~45 minutes once both machines are up.

## Why the local proof transfers

The local test suite (`crates/ferry-iroh/tests/relay_forced.rs`) proves the
mechanism, not the topology:

| Acceptance clause | Local proof (executed) | What remains manual |
|---|---|---|
| "sync through relay" | `force_relay` config strips every IP transport (`Endpoint::clear_ip_transports`), so two same-host engines exchange **all** bytes via a running ferry-relay and converge. Path sampling asserted relay-selected paths and zero IP-selected paths. | That iroh's hole punch fails across REAL NATs, forcing sustained relay use. Same code path; different network geometry. |
| "upgrade or stay direct per iroh's negotiation" | Normal-mode pair converged while path sampling observed iroh selecting a direct path after initial contact. | Hole-punch timing across symmetric/cone NATs (iroh's QAD + relay-assisted punching). |
| "relay logs contain no plaintext" | Relay stdout/stderr captured + structured connection ledger scanned for content markers, filenames, secret-shaped strings: zero hits, with metadata presence asserted (scan not vacuous). | Nothing structural; re-run the same scan against the cloud relay's logs. |

## Prerequisites

1. One VPS with a public IPv4 (2 vCPU / 1 GB is plenty; relay traffic for two
   folders is trivial). Ports: 22 (you), 3340/tcp (relay).
2. Machine A: your laptop on home Wi-Fi.
3. Machine B: a phone hotspot (different NAT, ideally different carrier) with
   a laptop or the ferry binary built for it.
4. The same ferry-sync build on all three roles:

```sh
cargo build --release -p ferry-daemon -p ferry-relay   # exact pins in Cargo.toml
```

## Step 1 — start the blind relay on the VPS

```sh
scp target/release/ferry-relay you@vps:/usr/local/bin/
ssh you@vps
ferry-relay --http-bind 0.0.0.0:3340 2>&1 | tee -a /var/log/ferry-relay.log
# expect:
#   RELAY http://<vps-ip>:3340/
#   listening on 0.0.0.0:3340 (blind ciphertext pipe)
```

Production note: plain HTTP is acceptable for validation. For a permanent
deployment terminate TLS at a reverse proxy (or use iroh-relay's ACME support)
and hand clients the https URL; nothing else changes.

## Step 2 — machine A (home NAT), listen role

```sh
mkdir -p ~/ferry-a && cd ~/ferry-a
./ferry-sync daemon --role listen \
    --store store --tree tree --tag node-a \
    --poly "$("$FERRY_BIN" genpoly --seed 20260824)" \
    --relay http://<vps-ip>:3340 2>&1 | tee node-a.log
# expect immediately:
#   ENDPOINT <hex64>          <- A's public endpoint id (derived from device identity)
#   LISTENING 127.0.0.1:<n>   <- route alias (not network-meaningful)
```

Record `<hex64>` — it is the ONLY address B needs. No IPs are exchanged by
hand at any step (ADR-0003).

## Step 3 — machine B (phone hotspot), connect role

```sh
mkdir -p ~/ferry-b && cd ~/ferry-b
./ferry-sync daemon --role connect --peer <A's hex64> \
    --store store --tree tree --tag node-b \
    --poly "<same poly hex>" \
    --relay http://<vps-ip>:3340 2>&1 | tee node-b.log
```

Both sides log `STATE root=<hex> agreed=<hex>` as they converge.

## Step 4 — transfer and observe the negotiation

On A, while the daemons run:

```sh
for i in $(seq 1 20); do head -c $((RANDOM % 4096)) /dev/urandom > tree/nat-$i.bin; done
mkdir -p tree/logs && (for j in $(seq 1 200); do echo "nat line $j" >> tree/logs/app.log; sleep 0.05; done) &
```

Expected within ~30 s, on BOTH machines' logs:

```
STATE root=<same-hex-on-both> agreed=<same-hex-on-both>
```

Pass criteria (negotiation): both sides eventually log sessions with
`SESSION complete`. To see WHICH path carried the data, run either daemon
with `RUST_LOG=iroh=debug` and look for path events; a healthy outcome is
either of:
- early relay traffic, then `path ... selected ... ip` (upgraded to direct), or
- sustained relay-only flow (hole punch failed — also an accepted outcome,
  per "stay direct per iroh's negotiation").

## Step 5 — plaintext-absence check at the relay

Back on the VPS:

```sh
for needle in "nat-" "app.log" "nat line"; do
    grep -c "$needle" /var/log/ferry-relay.log && echo "FAIL: plaintext leaked"
done
grep -c "relay client connected" /var/log/ferry-relay.log   # must be >= 2
```

Pass criteria (blindness):
- **Zero** matches for any transferred filename or file-content marker.
- **At least two** client-connect lines (A and B, logged by endpoint public
  key only). Metadata the relay legitimately holds: endpoint public keys,
  source IPs/ports, connection times, byte counts. If even THAT is missing,
  the scan is vacuous — fix logging before trusting the pass.

Byte counts alone cannot prove which path won the negotiation; that comes
from Step 4's client-side path logs.

## Failure triage

| Symptom | Likely cause |
|---|---|
| B never resolves A | Wrong `--peer`; or B cannot reach the relay (check `curl http://<vps>:3340/healthz`). |
| Converges but stays on relay forever | Expected when either side is behind a hard NAT (symmetric + UDP blocked). Not a failure of this acceptance. |
| Hotspot blocks UDP entirely | Relay still works (it is TCP/websocket-based); direct upgrade will simply never happen. |
| Relay log shows plaintext | STOP — that is a correctness bug in ferry-sync's framing, not a transport issue. File it before anything else. |

## Results

Record here when executed (date, machines/NAT types, negotiated outcome,
scan output):

- [ ] executed, pass
- [ ] executed, notes below
