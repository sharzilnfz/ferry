## Flake log

- 2026-08-25 wave2/t04 gate: forced_relay_mode_convergence assert failed once under load; passed isolated + full re-run.
- 2026-08-25 post-wave3/t19 merge: fifty_random_files_plus_append_heavy_log_converge_within_n_seconds failed once while 3 background worktrees were actively compiling (CPU starvation of wall-clock deadlines); passed isolated twice + full re-run after.

Pattern: convergence/relay tests use wall-clock budgets and are load-sensitive. Not observed on idle machines. If a third sighting occurs on an IDLE tree, file a determinism ticket (scale budgets or poll-count based assertions).
- 2026-08-25 T-06 dev: pin_enforcement acceptance test flaked ~1-in-10 on 30s convergence wait during development; hardened assertions + diagnostics; clean across ~65 isolated + 14 full runs since. Same load-sensitivity class.
- 2026-08-25 wave2/T-20 gate: tofu_pinned_identity_survives_engine_restart failed once under full-suite load (Node B sync deadline after engine restart); passed isolated + full re-run.
- 2026-08-25 wave2/T-20 gate re-run: engine_holds_pinned_peer_changes_and_release_recovers_them (pin_enforcement, 30s wall-clock budget) hit its deadline once under full-suite load; passed isolated (0.81s) + full re-run.
- 2026-08-25 post-T-20 wrap-up gates: tofu_first_connect_pins_identity_and_second_different_keypair_is_refused failed once in full workspace run; passed isolated + full re-run (load-sensitive wall-clock class).
- 2026-08-25 procs.rs child-token test: same-jiffy birth collision seen once on ubuntu-24.04 runner (test binary <10ms old at spawn); made deterministic via one retry after 25ms.
