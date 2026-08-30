








use std::collections::BTreeSet;


pub type RelPath = Vec<String>;


#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Trigger {
    
    Initial,
    
    #[default]
    Events,
    
    Poll,
    
    OverflowRecovery,
    
    Audit,
}




#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchSignal {
    
    
    
    Changed(Vec<RelPath>),
    
    
    
    PolledChanged(Vec<RelPath>),
    
    
    
    Overflow { reason: String },
    
    
    Unwatchable { subtree: RelPath, reason: String },
    
    
    
    PolledTick(RelPath),
    
    AuditDue,
    
    RootVanished,
    
    RootReturned,
}


#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Nothing,
    
    RescanSubtrees(Vec<RelPath>),
    
    
    FullRescan {
        reason: String,
    },
    
    StartPolling {
        subtree: RelPath,
    },
    
    RunAudit,
}


#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PolicyState {
    
    
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
            
            
            WatchSignal::PolledTick(_) => Action::Nothing,
            WatchSignal::AuditDue => Action::RunAudit,
            
            
            WatchSignal::RootVanished => Action::Nothing,
            WatchSignal::RootReturned => Action::FullRescan {
                reason: "watched root reappeared".to_string(),
            },
        }
    }
}

















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
        parts.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn changed_paths_mark_self_and_parent() {
        let a = PolicyState::default().apply(&WatchSignal::Changed(vec![
            p(&["a.txt"]),         
            p(&["src", "lib.rs"]), 
            p(&["src", "sub"]),    
        ]));
        match a {
            Action::RescanSubtrees(dirs) => {
                let mut got = dirs.clone();
                got.sort();
                assert_eq!(
                    got,
                    vec![
                        p(&[]),
                        p(&["a.txt"]), 
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
