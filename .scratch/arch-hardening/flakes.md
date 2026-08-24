## Flake log

- 2026-08-25 wave2/t04 gate: forced_relay_mode_convergence assert failed once under load; passed isolated + full re-run.
- 2026-08-25 post-wave3/t19 merge: fifty_random_files_plus_append_heavy_log_converge_within_n_seconds failed once while 3 background worktrees were actively compiling (CPU starvation of wall-clock deadlines); passed isolated twice + full re-run after.

Pattern: convergence/relay tests use wall-clock budgets and are load-sensitive. Not observed on idle machines. If a third sighting occurs on an IDLE tree, file a determinism ticket (scale budgets or poll-count based assertions).
