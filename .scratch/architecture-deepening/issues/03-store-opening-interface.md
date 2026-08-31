# 03: Store-opening interface in ferry-folder

**What to build:** Exactly one module decides how a folder's Store opens. The
folder module derives the folder master key from the config head, selects the
cipher (ChaCha20-Poly1305 only), and returns a typed error on any failure. The
silent plaintext fallback, the zero-key constant, and every call-site cipher
choice disappear: the sync engine, the scan engine, and the daemon supervisor
open folders through this one interface and never name a cipher themselves. A
folder whose key cannot be unwrapped fails loudly instead of reopening
unencrypted.

**Blocked by:** None (can start immediately).

**Status:** done

- [x] One interface opens a folder's store: derive key, pick cipher, fail loud
- [x] Key-unwrap failure is a typed error, never a plaintext or zero-key reopen
- [x] The plaintext cipher and zero master key are unreachable from sync, scan, and daemon call sites
- [x] Sync engine, scan engine, and supervisor open stores through the folder interface
- [x] Tests through the interface assert: valid folder opens encrypted, missing or stale key fails with the typed error, no fallback path exists
- [x] Existing at-rest encryption tests (pin enforcement, store format) pass
