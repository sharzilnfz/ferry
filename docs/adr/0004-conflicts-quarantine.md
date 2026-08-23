# ADR-0004: Conflicts quarantine; base-state tracking instead of last-writer-wins

Status: proposed (2026-08-23)

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
- Agent-awareness (session pinning) layers on top later: while a device pins
  a session, competing remote edits are held and surfaced, not applied.

## Consequences

- No silent data loss is possible, which is the trust foundation for letting
  agents near it.
- Conflict files can litter trees during heavy concurrent work; the report
  command and tuned defaults (agents usually touch disjoint paths in
  practice) keep this rare enough to be tolerable.
