# T-19: Bounded scan event delivery — stalled subscribers cannot retain snapshots

Status: ready-for-agent

Audit finding (Low): `ScanEngine::subscribe`
(crates/ferry-scan/src/engine.rs ~507-513) hands out an unbounded channel;
subscribers are pruned only when send returns Err (receiver dropped). A
live-but-stalled consumer accumulates one `ScanEvent::Updated(Arc<CurrentScan>)`
per pass forever, and each event keeps a complete CurrentScan alive — a
whole-snapshot leak per pass in a long-running daemon.

Fix: bounded delivery with latest-wins semantics (scan-completion events
coalesce naturally): keep `Option<ScanEvent>` per subscriber replaced on
publish, or a `sync_channel(bound)` treating TrySendError::Full as subscriber
misbehavior (skip/disconnect). Pick the simplest that preserves the existing
public contract of subscribe(); document the chosen backpressure semantics on
the method.

Acceptance: test publishes N events to a subscriber that never receives and
asserts retained memory is bounded (latest-wins observable: after N passes,
the pending event reflects pass N, and allocation count is O(1) per
subscriber); existing scan engine tests green.
