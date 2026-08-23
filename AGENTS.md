# AGENTS.md

## Agent skills

### Issue tracker

Issues live as local markdown under `.scratch/<feature>/`, one file per ticket
with `Status:` and `Depends on:` / `Blocks:` lines. See
`docs/agents/issue-tracker.md`.

### Triage labels

Default five-role vocabulary (`needs-triage`, `needs-info`,
`ready-for-agent`, `ready-for-human`, `wontfix`) recorded as a `Status:` line.
See `docs/agents/triage-labels.md`.

### Domain docs

Single-context layout: `CONTEXT.md` glossary plus `docs/adr/`. See
`docs/agents/domain.md`.

### Codebase memory

Once implementation code exists, index the repo into the codebase-memory MCP
as project `ferry-sync` and refresh after landing code. See
`docs/agents/codebase-memory.md`.

### Reading order for any fresh session

1. `README.md` — what this is, relationship to idea1
2. `SPEC.md` — scope, milestones, risks
3. `CONTEXT.md` — vocabulary
4. `PRODUCT.md` — durable product truth for design work (impeccable reads it)
5. `docs/adr/0001` through `0005` — settled decisions
6. `research/use-cases.md`, `research/landscape.md` — cited evidence
7. `.scratch/v1/issues/` — tickets, blockers first
