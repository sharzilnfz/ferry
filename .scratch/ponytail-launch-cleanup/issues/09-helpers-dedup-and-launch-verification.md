# 09: Shared helpers dedup and launch verification

Status: ready-for-agent
Depends on: 05, 06, 08
Blocks: None

**What to build:** A single owning place for every shared helper so a helper edit touches one file and every helper deletion is proven by the higher seams. From the user perspective byte formatting, ID shortening, status badges, and popups render identically across TUI, GUI, and web. From the maintainer perspective there is one helper table to audit.

**Blocked by:** 05, 06, 08

**Status:** ready-for-agent

- [ ] Duplicated `format_bytes` and `hex`/`short_hex`/`device_short` families unified under the store and platform formatting seams; duplicated `SyncState` vs `BeaconState` unified to one sync state enum with one badge table; hand-rolled `percent_decode_query_value` and double `api_fs_ls` validation unified to the single `ferry-folder` inventory path guard
- [ ] Hand-rolled helpers replaced by stdlib or native crates already in the tree: bitwise CRC-32 via `crc32fast`, custom base32 via `data-encoding` or a documented short-code alphabet, Gregorian calendar via `chrono`/`time`, cursor and put helpers via `std::io::Cursor` and `hex`, `PackCache` manual LRU via `lru::LruCache`, `StagingPools` branched once, `winpath` and colon guards gated to Windows, `FakeBackend` moved to tests/`#[cfg(test)]`
- [ ] External behavior unchanged: pairing codes still verify with single-character typo detection, packs still evict correctly, status rendering is identical across frontends, and the backend still enforces the folder `is_initialized` guard uniformly
- [ ] Launch checklist passes in one run: crate graph shows one fewer crate than baseline, deleted-symbol text searches are zero, `cargo test` for the convergence, transport, daemon backend, and IPC seams passes, and no new heavy dependency was introduced

## Comments

Final vertical slice and verification frontier. Depends on pairing single-truth (05), cipher single-truth (06), and flattened chains (08) because helpers touch all three domains. This is where the ponytail net of about 3 800–4 500 lines and minus one crate is proven with the harness from 01.
