# Context

Ferry (working name): full-file sync of developer project directories across
machines, end-to-end encrypted, peer-to-peer first, built for humans and the
agents working beside them. Not version control; git stays in charge of source
history, Ferry carries everything else.

## Glossary

**Device**
One machine running Ferry with its own long-lived identity keypair. Devices
are paired explicitly; nothing syncs to an unpaired device.

**Store**
The per-machine database of unique content: hash-addressed blobs plus
manifests that describe directory trees. The unit of sync. Inspired by git's
object database and restic's repository design.

**Blob**
A chunk of file content, identified by the hash of its bytes. Files larger
than one chunk are split into chunks; identical chunks across files or machines
are stored once.

**Manifest**
The description of one directory snapshot: a tree of entries (path, mode,
size, chunk list, mtime). Two machines compare manifests to know exactly which
blobs to exchange. Comparing manifests is cheap; reading files is not needed
for delta detection.

**Tree**
The normal folder on disk that editors, build tools, and agents see.
Materialized from the store. Never synced directly.

**Materialize / hydrate**
Producing the tree from local store contents. Hydration prefers links and
copies over network fetches: if any local peer already has a blob, no bytes
cross the wire.

**Folder (synced folder)**
A user-declared project directory under sync. Each folder has its own manifest
line and its own encryption key derived per device pair, so devices can share
some folders and not others.

**Pairing**
The key-exchange ritual that lets two devices trust each other: scan a QR or
type a short code out-of-band, derive the shared folder keys, done.

**Relay**
An untrusted server that shuttles encrypted traffic between peers when direct
connections fail (both sides behind hard NAT). Relays see ciphertext and
metadata only.

**Conflict file**
What Ferry produces when two devices change the same path concurrently:
`path.ext.ferry-conflict.<device>-<timestamp>` next to a winner, plus an entry
in a conflict report. Nothing is ever auto-merged.

**Selective rules**
Per-folder include/exclude globs controlling what syncs. Shipped with tuned
defaults for dev directories (`node_modules` opt-in, caches opt-out, `.env`
opt-in with a loud warning) because defaults decide whether people trust the
tool.

**Agent state**
Files whose whole purpose is serving coding agents: `.claude/`, `.opencode/`,
`CLAUDE.md`, `AGENTS.md`, session logs. Synced as ordinary files but treated as
its own category so policies (and marketing) can talk about it clearly.

**Session pinning**
Declaring "the agent works on device A until I release it." While pinned,
edits arriving from other devices for paths the agent is actively touching are
held back and surfaced, instead of racing the agent mid-write.

**Scan**
Walking the tree, hashing changed files, updating manifests. Incremental via
file watching plus size/mtime short-circuit, with periodic full-hash audits.

**Ignored-but-hydrated**
Content excluded from *your* sync rules but present locally (a machine may
hold `node_modules` it does not share). Hydration can still serve it to a
paired peer that wants it, if policy allows.

## Open questions (for the grilling pass)

- Exact chunker: fixed 1 MiB vs content-defined. Decide with benchmarks.
- Relay protocol: reuse BEP-style framing over QUIC, or something simpler.
- Windows symlink story: default off with clear docs? Administrator gate?
- Hosted relay discovery service: who runs it, what metadata it may hold.
- Name: "Ferry" is a placeholder and probably collides with something.
