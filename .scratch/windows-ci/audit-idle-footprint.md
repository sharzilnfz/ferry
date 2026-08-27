# Audit B — daemon idle footprint on Windows (RESEARCH-ONLY)

Branch `fix/windows-ci` @ 8182962. Scope: `ferry-daemon` main.rs,
`ferry-iroh`, `ferry-relay`, tokio runtime setup. Target: near-zero idle
CPU/wakeups. Everything cited; MEASURED = read directly from source (ours or
vendored registry at `E:\Users\sharz\.cargo\registry\src\index.crates.io-…`);
SUSPECTED = reasoned, not verified by running.

## 1. Periodic timers / poll loops

| Loop | Interval | Wakes / does | Runs on | Cite |
|---|---|---|---|---|
| Engine poll loop | 200 ms default (`--poll-ms`, engine.rs:56-59) | **Full tree snapshot every tick** (`Ctx::tick` → `snapshot_dir` walk+chunk+hash, ferry-sync\src\engine.rs:840-894) + prints `STATE root=…` line | dedicated OS thread `<tag>-poll` (engine.rs:1739-1748, 1856-1868) | MEASURED |
| Opportunistic dial | every 5 ticks (=1 s default, `DEFAULT_OPPORTUNISTIC_EVERY=5`, engine.rs:59, 884-892) | full iroh QUIC connect + session when connector role idle-diverged OR unconditionally every 5th tick for connectors | poll thread → session thread | MEASURED |
| Accept-loop accept slices | 100 ms | `block_on(timeout(100ms, ep.accept()))`; times out and loops forever while idle | `<tag>-accept` thread + tokio worker (ferry-iroh\src\transport.rs:446-481) | MEASURED |
| `join_until_signal` park | 200 ms | flag check only; never condvar-blocked | main thread (engine.rs:1938-1945) | MEASURED |
| Path sampler per connection | 50 ms | polls `conn.paths()` while a connection lives; exits on `conn.closed()` | spawned tokio task on transport runtime (transport.rs:368-393) | MEASURED (idle-safe: none exist with no peer) |
| Accept-error backoff | 50 ms sleep | only after accept errors | accept thread (engine.rs:1850) | MEASURED |
| mDNS announce/query | tau ≈ 10 s randomized announce (`swarm-discovery` 0.6.3 lib.rs:237, sender.rs:55-59); GC every ~12.3 s (updater.rs:30) | multicast announce/query packets | swarm-discovery task inside mdns lookup actor | MEASURED (source), firing SUSPECTED (only with `--discovery-mdns`) |
| Relay client ping | 15 s ± random (iroh-relay-1.0.3 protos\relay.rs:36, client.rs:339-386); QUIC relay transport keep-alive 25 s (quic.rs:297) | KEEP_ALIVE frames over the relay connection | relay client task on endpoint runtime | MEASURED source; active-at-idle SUSPECTED (only when a relay is configured AND connection maintained) |
| NetReport full re-report | 300 s (iroh-1.0.3 net_report.rs:132) | QAD probes against relays | net_report actor | MEASURED constant; trigger condition SUSPECTED |
| Portmapper renewal | UPnP lease 2 h, renew at half-life = 1 h (portmapper-0.19.1 upnp.rs:19-23); PCP/NAT-PMP half-lifetime renewals (pcp.rs:71, nat_pmp.rs:58) | gateway probe/renew traffic; initial probe at startup | portmapper actor | MEASURED constants; enabled because `iroh = "=1.0.3"` keeps default feature `portmapper` (Cargo.toml:35; iroh Cargo.toml:68-80) and `presets::Minimal` sets only the crypto provider (iroh presets.rs:61-79) |

## 2. iroh stack idle behavior (iroh 1.0.3)

- Endpoint built via `Endpoint::builder(presets::Minimal)` +
  `RelayMode::Disabled` by default (transport.rs:331-346;
  ferry-iroh\src\config.rs:70). With no `--relay` flags the daemon has **no**
  relay connection → no 15 s pings. With `--relay` it maintains a persistent
  relay session with pings even with zero peers (SUSPECTED but consistent
  with iroh-relay client.rs design).
- mDNS (`--discovery-mdns`): one `swarm-discovery` Discoverer + one actor
  task (iroh-mdns-address-lookup-0.5.0 lib.rs:452, 472-509). Announces ~every
  10 s randomized; queries are on-demand (resolve) not periodic — periodic
  query behavior SUSPECTED.
- No holepunch/ICE timer work observed in iroh 1.0.3 sources for an idle
  endpoint with zero remotes; per-remote actors spawn per EndpointId
  (socket.rs:339-344), so nothing exists before first contact. Idle cost is
  dominated by our own loops, not iroh's. (SUSPECTED — no runtime trace.)
- Windows-specific: portmapper's initial UPnP/PCP/NAT-PMP gateway probing
  runs at bind time and renews hourly; on typical Windows home routers this
  is silent, but firewall prompts are possible (iroh docs note this in
  iroh src\portmapper.rs:15).

## 3. Thread/handle inventory at idle (daemon startup path, listener role)

OS threads:
1. main (parked in `join_until_signal`)
2. `<tag>-poll`
3. `<tag>-accept`
4-5. 2 tokio workers for the transport runtime (transport.rs:153-157)
6+. tokio blocking pool as needed (mdns/socket internals) — count SUSPECTED

Spawned tasks (tokio): mdns actor + discoverer (if enabled), endpoint state
actor, socket/transports actors, portmapper, net_report (SUSPECTED exact set;
no leak found — all are bound to endpoint lifetime via AbortOnDropHandle /
endpoint close, transport.rs:288-294 closes the endpoint).

Leaks checked:
- `joins` Vec grows with every accepted session thread and is only popped in
  `shutdown` (engine.rs:1725, 1843, 1922) — memory growth per session, not a
  thread leak (threads exit). Minor.
- `observations` HashMap (transport.rs:90, 102-107) grows one entry per peer
  ever seen, never pruned. Bounded by peer count; cosmetic.
- Path-sampler tasks exit on `conn.closed()` (transport.rs:386-389) — OK.
- Condvar watchers (`FolderState.changed`, `SharedState.live_idle`,
  engine.rs:352, 529) are notified on shutdown (engine.rs:1917, 608) — OK.
- No channel found that stays open across restarts in-process.

## 4. Recent poll-for-marker / poll-for-staleness commits

- ce671d3 (pin.rs tests): 10 ms poll up to 2 s for `Liveness::Stale`. Test-only
  (`mod tests`, pin.rs:485+) — never runs in the daemon. MEASURED.
- 5923e35 (relay_forced.rs test): `wait_markers_landed` polls disk ≤30 s.
  Test-only. MEASURED.
- f6a473d: gates that pin staleness assert behind `#[cfg(unix)]`; widens a
  test budget 15→30 s (peer_policy.rs). Test-only. MEASURED.
- Conclusion: none of the three adds daemon-runtime polling. The *pre-existing*
  200 ms full-tree poll loop (§1 row 1) remains the only always-on poll.

## 5. Ranked low-risk wins (no wire-protocol or store-layout changes)

1. **Change-detect before snapshotting** (biggest win): in `Ctx::tick`
   (engine.rs:840) skip `snapshot_dir` unless tree mtime/size signature or a
   cheap recursive mtime-max changed since last tick. Pure local scheduling;
   wire behavior identical because manifests only mint on real change anyway.
   Turns ~5 wakeups/s + full walk into ~0.
2. **Event-driven waits instead of fixed sleeps**: replace
   `join_until_signal`'s 200 ms spin (engine.rs:1943) with
   `SharedState.shutdown` condvar wait; replace the iroh accept 100 ms timeout
   slice (transport.rs:469-471) with a long `ep.accept().await` raced against
   a `closed` watcher/select on the `wake` flag — removes 10 wakeups/s on the
   accept path without touching frames.
3. **Raise default `--poll-ms` / make opportunistic dial conditional**: bump
   default poll to 1000 ms and dial only on baseline divergence (drop the
   unconditional `n % opportunistic_every == 0` branch, engine.rs:886);
   config-default change only, protocol untouched. Also consider
   `portmapper_config(Disabled)` (builder option exists, iroh endpoint.rs:786)
   to kill hourly gateway churn.

MEASURED baseline: idle listener today wakes ≥12×/s (poll 5/s + accept 10/s
overlapped on separate threads + main 5/s) and performs a full chunked tree
scan 5×/s regardless of changes.
