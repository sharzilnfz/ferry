# T-004: Scan pipeline and watchers

Status: ready-for-agent
Depends on: T-003

Incremental scan: fs events (notify crate: FSEvents / inotify /
ReadDirectoryChangesW) plus size/mtime short-circuit, debounced, with
periodic full-hash audit and a poll fallback for Linux descriptor exhaustion
(Mutagen's policy, see research). Must survive inotify queue overflow without
losing correctness (rescan on overflow). Ignore-rule filtering hooks land here
(T-011 fills them).

Benchmark gate: initial scan of a 100k-file / 500 MiB synthetic fixture
under 60 s on CI hardware; incremental rescan after 100 changed files under
2 s. Record numbers in `benchmarks/`.
