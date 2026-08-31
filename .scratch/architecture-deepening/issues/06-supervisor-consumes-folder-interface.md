# 06: Supervisor consumes the folder interface

**What to build:** The daemon supervisor stops re-deriving folder facts. Its
hash-guess polynomial derivation and its direct walk of the folder config file
are deleted. It opens folders through the folder module's interface, which
returns the store's real chunker polynomial, and leaves peer-policy resolution
to the sync engine. The Store is the single source of truth for polynomial
configuration, so chunks line up across devices after either side rebuilds.

**Blocked by:** 03 (the supervisor consumes the store-opening interface).

**Status:** done

- [x] The polynomial hash-guess helper is deleted; polynomials come from the store
- [x] The supervisor no longer reads the folder config file directly
- [x] Both devices reconcile correctly when one binary was rebuilt between scans (polynomial stability test)
- [x] The supervisor passes only path and identity into folder opening
- [x] Tests cover the folder-opening branch of supervision that was previously untested
