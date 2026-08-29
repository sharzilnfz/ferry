# T-16: Protocol hardening — session-wide receive budgets + crash-safe pack ingest

Status: done

Two audit findings in crates/ferry-proto/src/engine.rs:

1. **Unbounded receive loops** (Medium). Individual frames are capped
   (MAX_FRAME_BODY, MAX_ROWS=2048) but sequence-level loops are not:
   - `recv_advert_map` (~1062-1074) loops while the peer keeps sending
     `more=1`, inserting every row into an unbounded BTreeMap — a hostile or
     corrupted peer pins daemon memory indefinitely and never terminates.
   - `read_item_batches` (~1157-1180) and the client side of
     `fetch_via_packs`'s inner loop (~1223-1255): the client's loop runs until
     the peer decides to stop.
   Fix: thread a session-wide budget through these loops — cap total advert
   rows per folder and batches/items per request round (constants, e.g.
   MAX_ADVERT_ROWS_TOTAL), returning `ProtoError::ResourceLimit` when exceeded
   so it maps to the existing typed BYE path. Mirror how MAX_BFS_ROUNDS bounds
   the tree walk (~1312). Pick caps generously above what legitimate folders
   produce today; document each constant's rationale.

2. **Fixed cross-process temp name + no fsync on pack ingest** (Medium).
   `ingest_pack` (~1262-1275) writes `pull-<hex>.pack` — deterministic per
   pack, shared by every process on the same store dir (CLI sync while daemon
   runs is a supported topology). Two processes pulling the same pack
   interleave writes; whoever renames lands interleaved bytes under a valid
   BLAKE3 name that rebuild_index then vouches for. No `sync_all` before
   rename either, so crashes can leave torn packs.
   Fix: unique temp name (pid + fresh entropy — reuse the existing
   temp_name_for/fresh_entropy pattern), write via File::create + write_all +
   sync_all, rename, fsync the packs dir where the platform allows. Match the
   durability discipline ferry-materialize's write_temp_then_rename already
   implements.

Acceptance: tests assert (a) a peer streaming endless more=1 advert frames
hits ResourceLimit instead of OOM (bounded row counter observable); (b) two
concurrent ingest_pack calls of the same pack both succeed and the final
on-disk pack parses and matches its BLAKE3 name; (c) existing protocol suites
green on both sides of the budget constants.
