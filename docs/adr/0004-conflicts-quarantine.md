# ADR-0004: Conflicts quarantine; base-state tracking instead of last-writer-wins

Status: accepted (2026-08-23)

## Context

Agents write thousands of files at machine speed while humans edit elsewhere.
Cloud drives silently last-writer-win or spawn "conflicting copy" debris that
corrupts builds. Syncthing renames the loser to `*.sync-conflict-*`. Mutagen's
key insight is tracking the last-agreed state so every sync cycle is a
three-way merge between two endpoint states and their common ancestor.

## Decision

- Each folder tracks its last-agreed manifest (the common ancestor).
- When both sides diverge from the ancestor on one path: keep the newer side
  as the live file, save the other as `path.ferry-conflict.<device>-<ts>`,
  and add an entry to a structured conflict report (`ferry conflicts list`).
- Never auto-merge file contents. Ever. Binary or text makes no difference.
- Deletions versus edits conflict the same way: the edited file returns with
  a conflict marker rather than vanishing.
- Agent-awareness (session pinning) layers on top: while a device pins
  a session, competing remote edits are held and surfaced, not applied.

## Amendment (2026-08-31): Held Manifest Persistence During Session Pinning

When a session pin is active (`ferry pin start`), incoming remote modifications
held by the engine must have their raw manifest payloads immediately persisted
into the local blob store before returning the held outcome. On `ferry pin release`,
the engine loads the persisted held manifest and performs full three-way
reconciliation against the baseline and local working tree, cleanly applying
non-conflicting modifications and quarantining true conflicts without
`held-manifest-missing` errors.

## Consequences

- No silent data loss is possible, which is the trust foundation for letting
  agents near it.
- Conflict files can litter trees during heavy concurrent work; the report
  command and tuned defaults (agents usually touch disjoint paths in
  practice) keep this rare enough to be tolerable.
