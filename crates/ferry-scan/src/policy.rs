//! Pure decision layer for watcher health. Everything here is unit-testable
//! without kernel timing: the platform glue in `engine` translates notify
//! events/errors into [`WatchSignal`]s, [`PolicyState`] decides what they
//! mean, and the worker executes the returned [`Action`]s.
//!
//! Design rule (crate docs, "correctness over precision"): unclassifiable
//! loss is overflow. The only signals treated as precise are explicit change
//! paths and poll ticks.

use std::collections::BTreeSet;

/// A path below the watched root as NFC components; `[]` is the root.
pub type RelPath = Vec<String>;

/// What caused a completed scan pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Trigger {
    /// First scan in the life of an engine (always a full snapshot).
    Initial,
    /// Debounced native filesystem events.
    #[default]
    Events,
    /// Poll-fallback sweep found mismatches.
    Poll,
    /// Recovery from event loss (overflow, root recreation).
    OverflowRecovery,
    /// Scheduled full-hash audit.
    Audit,
}

/// Something the platform layer observed. Relative paths only: the engine
/// strips the root prefix at the boundary so policy logic never sees
/// absolute paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchSignal {
    /// One or more paths changed (files, dirs, or vanished paths — callers
    /// need not know which). Coalesced by the debounce window before this
    /// signal is built.
    Changed(Vec<RelPath>),
    /// Same as [`WatchSignal::Changed`] but originating from a poll-fallback
    /// sweep; handled identically by policy, kept distinct so scan stats can
    /// attribute the pass to polling.
    PolledChanged(Vec<RelPath>),
    /// Kernel-level event loss or anything unclassifiable: inotify
    /// `Q_OVERFLOW`, Windows buffer overrun, FSEvents history drop, a
    /// watcher error that cannot be proven benign.
    Overflow { reason: String },
    /// A watch could not be established for this subtree (descriptor
    /// exhaustion on Linux). The subtree still gets polled.
    Unwatchable { subtree: RelPath, reason: String },
    /// Poll-fallback timer fired for a polled subtree. The engine diffs the
    /// subtree stat-only and synthesizes `Changed` signals from mismatches;
    /// policy does no IO, so this action is pure bookkeeping.
    PolledTick(RelPath),
    /// Periodic full-hash audit deadline.
    AuditDue,
    /// The watched root disappeared (deleted or renamed away).
    RootVanished,
    /// The watched root exists again after having been gone.
    RootReturned,
}

/// What to do about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Nothing,
    /// Rebuild these dirty subtrees incrementally.
    RescanSubtrees(Vec<RelPath>),
    /// Discard incremental state and snapshot from scratch. Repairs any
    /// drift by construction.
    FullRescan {
        reason: String,
    },
    /// Begin poll fallback for a subtree native watching could not cover.
    StartPolling {
        subtree: RelPath,
    },
    /// Run the scheduled full-hash audit now.
    RunAudit,
}

/// Bookkeeping for watch-health decisions across signals.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PolicyState {
    /// Subtrees currently on poll fallback because their watches could not
    /// be established.
    pub polling: BTreeSet<RelPath>,
}

impl PolicyState {
    pub fn apply(&mut self, signal: &WatchSignal) -> Action {
        match signal {
            WatchSignal::Changed(paths) | WatchSignal::PolledChanged(paths) => {
                match enclosing_dirs(paths) {
                    dirs if dirs.is_empty() => Action::Nothing,
                    dirs => Action::RescanSubtrees(dirs),
                }
            }
            WatchSignal::Overflow { reason } => Action::FullRescan {
                reason: reason.clone(),
            },
            WatchSignal::Unwatchable { subtree, reason } => {
                self.polling.insert(subtree.clone());
                let _ = reason;
                Action::StartPolling {
                    subtree: subtree.clone(),
                }
            }
            // The poll thread itself produces Changed signals from its sweep;
            // ticks only confirm the fallback stays armed.
            WatchSignal::PolledTick(_) => Action::Nothing,
            WatchSignal::AuditDue => Action::RunAudit,
            // Root liveness is enforced by the worker (pause/resume); policy
            // records the recovery rescan decision.
            WatchSignal::RootVanished => Action::Nothing,
            WatchSignal::RootReturned => Action::FullRescan {
                reason: "watched root reappeared".to_string(),
            },
        }
    }
}

/// Map changed paths to the dirty directories that must rebuild.
///
/// For every changed path we mark BOTH its nearest enclosing directory and
/// the path itself:
///
/// - parent marking catches deletions/renames-away (the parent's rebuilt
///   listing simply lacks the entry),
/// - self marking makes a changed DIRECTORY's own listing rebuild; without
///   it, a parent rebuild would see the cached child node and reuse stale
///   state.
///
/// Marking a file path "as if it were a dir" is harmless: the walker skips
/// non-directory targets, and the parent marking did the real work.
///
/// Ancestors above the marked dirs are added by the caller when closing the
/// dirty set under ancestry (`walk` requires a transitivity-closed set).
pub fn enclosing_dirs(paths: &[RelPath]) -> Vec<RelPath> {
    let mut out: BTreeSet<RelPath> = BTreeSet::new();
    for p in paths {
        match p.len() {
            0 => {
                out.insert(Vec::new());
            }
            _ => {
                out.insert(p.clone());
                out.insert(p[..p.len() - 1].to_vec());
            }
        }
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(parts: &[&str]) -> RelPath {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn changed_paths_mark_self_and_parent() {
        let a = PolicyState::default().apply(&WatchSignal::Changed(vec![
            p(&["a.txt"]),         // file: parent [] does the work
            p(&["src", "lib.rs"]), // nested file
            p(&["src", "sub"]),    // dir itself must rebuild too
        ]));
        match a {
            Action::RescanSubtrees(dirs) => {
                let mut got = dirs.clone();
                got.sort();
                assert_eq!(
                    got,
                    vec![
                        p(&[]),
                        p(&["a.txt"]), // file marks itself too; harmless
                        p(&["src"]),
                        p(&["src", "lib.rs"]),
                        p(&["src", "sub"])
                    ],
                    "self + nearest enclosing dir for each path"
                );
            }
            other => panic!("expected RescanSubtrees, got {other:?}"),
        }
    }

    #[test]
    fn empty_change_batch_is_idle() {
        assert_eq!(
            PolicyState::default().apply(&WatchSignal::Changed(vec![])),
            Action::Nothing
        );
    }

    #[test]
    fn root_only_change_rescans_root() {
        let a = PolicyState::default().apply(&WatchSignal::Changed(vec![p(&[])]));
        assert_eq!(a, Action::RescanSubtrees(vec![p(&[])]));
    }

    #[test]
    fn overflow_always_triggers_full_rescan_with_reason() {
        let a = PolicyState::default().apply(&WatchSignal::Overflow {
            reason: "inotify queue overflow".into(),
        });
        assert_eq!(
            a,
            Action::FullRescan {
                reason: "inotify queue overflow".into()
            },
            "lost events can only be repaired by a from-scratch pass"
        );
    }

    #[test]
    fn unwatchable_subtree_starts_polling_and_is_remembered() {
        let mut st = PolicyState::default();
        let a = st.apply(&WatchSignal::Unwatchable {
            subtree: p(&["node_modules"]),
            reason: "ENOSPC".into(),
        });
        assert_eq!(
            a,
            Action::StartPolling {
                subtree: p(&["node_modules"])
            }
        );
        assert!(
            st.polling.contains(&p(&["node_modules"])),
            "state remembers the fallback"
        );
        // A second failure for another subtree accumulates.
        st.apply(&WatchSignal::Unwatchable {
            subtree: p(&["big2"]),
            reason: "EMFILE".into(),
        });
        assert_eq!(st.polling.len(), 2);
    }

    #[test]
    fn poll_ticks_are_bookkeeping_only() {
        assert_eq!(
            PolicyState::default().apply(&WatchSignal::PolledTick(p(&["node_modules"]))),
            Action::Nothing,
            "the poll thread synthesizes Changed signals from real mismatches"
        );
    }

    #[test]
    fn audit_is_requested_when_due() {
        assert_eq!(
            PolicyState::default().apply(&WatchSignal::AuditDue),
            Action::RunAudit
        );
    }

    #[test]
    fn root_return_means_full_rescan_vanish_pauses() {
        assert_eq!(
            PolicyState::default().apply(&WatchSignal::RootVanished),
            Action::Nothing
        );
        assert_eq!(
            PolicyState::default().apply(&WatchSignal::RootReturned),
            Action::FullRescan {
                reason: "watched root reappeared".into()
            }
        );
    }
}
