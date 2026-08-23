# T-003: Manifests and tree snapshots

Status: ready-for-agent
Depends on: T-002

Manifests per the T-001 schema: deterministic serialization, hash-addressed,
tree nodes deduplicated like restic's trees. API: snapshot a directory into a
manifest; diff two manifests into a change set (added/removed/modified paths
with required blob lists). Manifest diff must not touch file bytes.

Acceptance: snapshot → mutate tree → resnapshot → diff shows exactly the
mutations; identical trees produce byte-identical manifests.
