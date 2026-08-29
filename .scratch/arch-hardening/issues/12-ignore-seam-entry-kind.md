# T-12: Ignore seam carries entry kind — kill the stat-in-hot-path adapter

Status: done

ferry-scan's `IgnorePolicy::ignored(rel: &[String])` dropped `is_dir`, so the
adapter in ferry-ignore/policy.rs:299 double-evaluates both interpretations
and spends one symlink_metadata per consulted path during walks/watch
registration; it also re-implements quarantine-name exemption. gitignore's
dir/file duality belongs INSIDE ferry-ignore, not at the seam.

Fix: change the scan-side seam to carry entry kind (ignored(rel, kind) or a
small Decision enum incl. parent-verdict reuse if cheap); make FerryIgnore's
adapter a one-line delegation to decided(); keep decided() public for the
secret-scanning consumer. Update watch-registration paths to pass the right
kind (directories report Dir even before descending).

Note: this is the "one adapter = hypothetical seam" smell resolved BEFORE a
second divergent adapter appears. If materialize wants a pre-check later, it
plugs in without stats.

Acceptance: ignore-decision unit tests parameterized over entry kind without
touching the filesystem; the double-evaluation branch is gone (adapter has
no fallback stat); selective-rules e2e behavior unchanged (node_modules
opt-in etc. still respected — covered by existing scan tests).
