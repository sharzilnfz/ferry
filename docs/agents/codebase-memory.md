# Codebase memory

This repo will be indexed in the codebase-memory MCP under the project name
`ferry-sync`. Until implementation code exists there is nothing to index; once
the first crates land, use it in every session:

## Before implementing a ticket

- Call `index_repository` on this repo if code just landed in another session;
  the index does not auto-refresh mid-session.
- Use `get_architecture` and `search_graph` to orient instead of re-reading
  files grep-first.
- Check `check_index_coverage` before claiming anything about files you did
  not open yourself.

## After landing work

- Re-run `index_repository` so the next parallel worker sees your symbols.
- If you changed a public trait (store, transport, reconciliation), note it in
  your final report; downstream tickets depend on those signatures.
