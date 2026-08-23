# Domain Docs

How the engineering skills should consume this repo's domain documentation when exploring the codebase.

## Before exploring, read these

- **`CONTEXT.md`** at the repo root
- **`docs/adr/`**: read ADRs that touch the area you're about to work in

If any of these files don't exist, **proceed silently**. The `/domain-modeling` skill creates them lazily when terms or decisions actually get resolved.

## Layout

Single-context repo:

```
/
├── CONTEXT.md
├── docs/adr/
└── src/
```

## Use the glossary's vocabulary

When your output names a domain concept, use the term as defined in `CONTEXT.md`. Don't drift to synonyms the glossary explicitly avoids. Key terms here: store, blob, manifest, tree, materialize, hydrate, folder, pairing, relay, conflict file, session pinning.

If a term you need isn't in the glossary yet, note it for `/domain-modeling` rather than inventing quietly.

## Flag ADR conflicts

If your output contradicts an existing ADR (0001–0005 currently), surface it explicitly rather than silently overriding:

> _Contradicts ADR-0004 (conflicts quarantine), but worth reopening because…_
