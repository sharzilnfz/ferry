# 04: Encapsulate Iroh transport runtime isolation

**What to build:** Formalize and clean up runtime isolation within `crates/ferry-iroh/src/transport.rs`. Ensure synchronous trait invocations (`dial`, `listen`, `close`) cleanly interface with the underlying endpoint tasks without leaking runtime state, unhandled drop edge cases, or potential thread deadlocks.

**Blocked by:** None (can start immediately).

**Status:** resolved

- [x] `IrohTransport` encapsulates its background runtime lifecycle with clean shutdown on drop
- [x] No nested Tokio runtime panic paths exist in synchronous transport calls
- [x] Integration tests in `crates/ferry-iroh/tests/` pass reliably
