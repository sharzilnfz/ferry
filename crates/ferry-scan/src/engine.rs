use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ferry_store::manifest::EntryPayload;
use ferry_store::snapshot::SnapshotIdentity;
use ferry_store::store::Store;
use ferry_store::BlobId;
use notify::{Error as NotifyError, Event, RecommendedWatcher, RecursiveMode, Watcher};

use crate::config::ScanConfig;
use crate::error::ScanError;
use crate::ignore::{EntryKind, IgnorePolicy};
use crate::policy::{Action, PolicyState, RelPath, Trigger, WatchSignal};
use crate::state::DirCache;
use crate::walk::{close_under_ancestors, PassStats, Walker};
use unicode_normalization::UnicodeNormalization;

#[derive(Clone)]
pub struct StoreHandle {
    pub store: Arc<Store>,

    pub poly: ferry_store::chunker::ValidatedPoly,
    pub folder_id: [u8; 16],
    pub device_id: [u8; 32],
}

#[derive(Clone, Debug)]
pub struct CurrentScan {
    pub manifest: ferry_store::manifest::RootManifest,
    pub manifest_id: BlobId,
    pub root_tree_id: BlobId,
    pub trigger: Trigger,
    pub stats: PassStats,
    pub finished_unix_secs: i64,
}

#[derive(Clone, Debug)]
pub enum ScanEvent {
    Updated(Arc<CurrentScan>),
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct ScanRun {
    pub published: Option<Arc<CurrentScan>>,

    pub stats: PassStats,
}

struct SignalQueue {
    q: Mutex<VecDeque<WatchSignal>>,
    cv: std::sync::Condvar,
}

impl SignalQueue {
    fn new() -> Self {
        SignalQueue {
            q: Mutex::new(VecDeque::new()),
            cv: std::sync::Condvar::new(),
        }
    }

    fn push(&self, s: WatchSignal) {
        self.q.lock().expect("signal queue").push_back(s);
        self.cv.notify_all();
    }

    fn wake(&self) {
        self.cv.notify_all();
    }

    fn drain(&self) -> Vec<WatchSignal> {
        let mut g = self.q.lock().expect("signal queue");
        let out: Vec<WatchSignal> = g.drain(..).collect();
        out
    }

    fn wait_nonempty(&self, stop: &AtomicBool, tick: Duration) -> bool {
        let mut g = self.q.lock().expect("signal queue");
        loop {
            if !g.is_empty() {
                return true;
            }
            if stop.load(Ordering::Relaxed) {
                return false;
            }
            let (g2, _) = self.cv.wait_timeout(g, tick).expect("signal queue");
            g = g2;
        }
    }

    fn wait_arrival(&self, stop: &AtomicBool, dur: Duration) -> bool {
        let deadline = Instant::now() + dur;
        let mut g = self.q.lock().expect("signal queue");
        loop {
            if !g.is_empty() {
                return true;
            }
            if stop.load(Ordering::Relaxed) {
                return false;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let (g2, t) = self
                .cv
                .wait_timeout(g, deadline - now)
                .expect("signal queue");
            g = g2;
            if t.timed_out() || stop.load(Ordering::Relaxed) {
                return !g.is_empty();
            }
        }
    }
}

struct Core {
    handle: StoreHandle,
    cfg: ScanConfig,
    ignore: Arc<dyn IgnorePolicy>,
    disk_root: PathBuf,
    cache: DirCache,
    policy: PolicyState,
    prev_manifest_id: BlobId,
    prev_root_tree_id: BlobId,
    root_gone: bool,

    last_pass: Option<(Trigger, PassStats)>,
}

struct Parts {
    queue: Arc<SignalQueue>,
    core: Arc<Mutex<Core>>,
    current: Arc<RwLock<Option<Arc<CurrentScan>>>>,
    subs: Arc<Mutex<Vec<Subscriber>>>,
}

const SUB_CHANNEL_BOUND: usize = 1;

struct Subscriber {
    tx: SyncSender<ScanEvent>,

    staged: Option<ScanEvent>,
}

impl Subscriber {
    fn offer(&mut self, ev: ScanEvent) -> bool {
        self.staged = Some(ev);
        match self
            .tx
            .try_send(self.staged.as_ref().expect("just staged").clone())
        {
            Ok(()) => self.staged = None,

            Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => return false,
        }
        true
    }
}

impl Parts {
    fn execute(
        &self,
        signals: Vec<WatchSignal>,
        fallback_trigger: Trigger,
    ) -> Result<ScanRun, ScanError> {
        let run = self.execute_inner(signals, fallback_trigger)?;
        self.core.lock().expect("core").last_pass = Some((run.stats.trigger, run.stats.clone()));
        Ok(run)
    }

    fn execute_inner(
        &self,
        signals: Vec<WatchSignal>,
        fallback_trigger: Trigger,
    ) -> Result<ScanRun, ScanError> {
        let mut dirty: Vec<RelPath> = Vec::new();
        let mut full_reason: Option<String> = None;
        let mut audit = false;
        let mut trigger = fallback_trigger;

        {
            let mut c = self.core.lock().expect("core");
            for s in &signals {
                match s {
                    WatchSignal::Overflow { .. } | WatchSignal::RootReturned => {
                        trigger = Trigger::OverflowRecovery;
                    }
                    WatchSignal::AuditDue => {
                        if trigger == Trigger::Events {
                            trigger = Trigger::Audit;
                        }
                    }
                    WatchSignal::PolledChanged(_) if trigger == Trigger::Events => {
                        trigger = Trigger::Poll;
                    }
                    _ => {}
                }
                match c.policy.apply(s) {
                    Action::Nothing => {}
                    Action::RescanSubtrees(dirs) => dirty.extend(dirs),
                    Action::FullRescan { reason } => {
                        if full_reason.is_none() {
                            full_reason = Some(reason);
                        }
                    }
                    Action::StartPolling { .. } => {}
                    Action::RunAudit => audit = true,
                }
            }
        }

        if let Some(reason) = full_reason {
            return self.run_full(trigger, &reason);
        }
        if audit {
            return self.run_full(Trigger::Audit, "scheduled audit");
        }
        if dirty.is_empty() {
            return Ok(ScanRun {
                published: None,
                stats: PassStats {
                    trigger,
                    ..PassStats::default()
                },
            });
        }
        let closed = close_under_ancestors(&dirty);
        self.run_incremental(closed, trigger)
    }

    fn identity_now(core: &Core) -> SnapshotIdentity {
        let (sec, nsec) = unix_now();
        SnapshotIdentity {
            folder_id: core.handle.folder_id,
            device_id: core.handle.device_id,
            parent_manifest_id: core.prev_manifest_id,
            created_sec: sec,
            created_nsec: nsec,
        }
    }

    fn run_full(&self, trigger: Trigger, reason: &str) -> Result<ScanRun, ScanError> {
        let _ = reason;
        let _started = Instant::now();
        let mut core = self.core.lock().expect("core");
        if !core.disk_root.is_dir() {
            core.root_gone = true;
            return Ok(idle_run(trigger));
        }
        let identity = Self::identity_now(&core);
        let mut fresh_cache = DirCache::new();
        let mut closed = BTreeSet::new();
        closed.insert(Vec::new());
        let mut stats = PassStats {
            trigger,
            ..PassStats::default()
        };
        let out = Walker::run(
            &core.handle.store,
            core.handle.poly,
            core.ignore.as_ref(),
            &core.disk_root,
            &mut fresh_cache,
            &closed,
            trigger,
            &identity,
            core.prev_root_tree_id,
            &mut stats,
        )?;

        core.cache = fresh_cache;

        let published = match out {
            Some(out) => {
                core.prev_root_tree_id = out.root_tree_id;
                core.prev_manifest_id = out.manifest_id;
                let cur = Arc::new(CurrentScan {
                    manifest: out.manifest,
                    manifest_id: out.manifest_id,
                    root_tree_id: out.root_tree_id,
                    trigger,
                    stats: stats.clone(),
                    finished_unix_secs: unix_now().0,
                });
                self.publish(cur.clone());
                Some(cur)
            }
            None => None,
        };
        Ok(ScanRun { published, stats })
    }

    fn run_incremental(
        &self,
        closed: BTreeSet<RelPath>,
        trigger: Trigger,
    ) -> Result<ScanRun, ScanError> {
        let mut core = self.core.lock().expect("core");
        if !core.disk_root.is_dir() {
            core.root_gone = true;
            return Ok(idle_run(trigger));
        }
        let identity = Self::identity_now(&core);

        let Core {
            handle,
            ignore,
            disk_root,
            cache,
            prev_root_tree_id,
            ..
        } = &mut *core;
        let mut stats = PassStats {
            trigger,
            ..PassStats::default()
        };
        let out = Walker::run(
            &handle.store,
            handle.poly,
            ignore.as_ref(),
            disk_root,
            cache,
            &closed,
            trigger,
            &identity,
            *prev_root_tree_id,
            &mut stats,
        )?;

        match out {
            Some(out) => {
                core.prev_root_tree_id = out.root_tree_id;
                core.prev_manifest_id = out.manifest_id;
                let cur = Arc::new(CurrentScan {
                    manifest: out.manifest,
                    manifest_id: out.manifest_id,
                    root_tree_id: out.root_tree_id,
                    trigger,
                    stats: stats.clone(),
                    finished_unix_secs: unix_now().0,
                });
                drop(core);
                self.publish(cur.clone());
                Ok(ScanRun {
                    published: Some(cur),
                    stats,
                })
            }
            None => Ok(ScanRun {
                published: None,
                stats,
            }),
        }
    }

    fn publish(&self, cur: Arc<CurrentScan>) {
        *self.current.write().expect("current lock") = Some(cur.clone());
        self.deliver(ScanEvent::Updated(cur));
    }

    fn report_failure(&self, err: &ScanError) {
        self.deliver(ScanEvent::Failed(err.to_string()));
    }

    fn deliver(&self, ev: ScanEvent) {
        let mut subs = self.subs.lock().expect("subs lock");
        subs.retain_mut(|sub| sub.offer(ev.clone()));
    }
}

pub struct ScanEngine {
    parts: Arc<Parts>,
    stop: Arc<AtomicBool>,
    handles: Mutex<Vec<std::thread::JoinHandle<()>>>,

    _watcher: Option<RecommendedWatcher>,
}

impl ScanEngine {
    pub fn watch(root: impl Into<PathBuf>, handle: StoreHandle) -> Result<Self, ScanError> {
        Self::watch_with(
            root,
            handle,
            ScanConfig::default(),
            Arc::new(crate::ignore::NoIgnores),
        )
    }

    pub fn watch_with(
        root: impl Into<PathBuf>,
        handle: StoreHandle,
        cfg: ScanConfig,
        ignore: Arc<dyn IgnorePolicy>,
    ) -> Result<Self, ScanError> {
        let disk_root = root.into();
        if !disk_root.is_dir() {
            return Err(ScanError::Watch(format!(
                "watch root {} is not a directory",
                disk_root.display()
            )));
        }

        let disk_root = std::fs::canonicalize(&disk_root)
            .map_err(|e| ScanError::Watch(format!("cannot resolve watch root: {e}")))?;

        let prev_manifest_id = cfg.parent_manifest_id.unwrap_or([0u8; 32]);
        let parts = Arc::new(Parts {
            queue: Arc::new(SignalQueue::new()),
            core: Arc::new(Mutex::new(Core {
                handle,
                cfg,
                ignore: ignore.clone(),
                disk_root: disk_root.clone(),
                cache: DirCache::new(),
                policy: PolicyState::default(),
                prev_manifest_id,
                prev_root_tree_id: [0u8; 32],
                root_gone: false,
                last_pass: None,
            })),
            current: Arc::new(RwLock::new(None)),
            subs: Arc::new(Mutex::new(Vec::new())),
        });

        let mut engine = ScanEngine {
            parts,
            stop: Arc::new(AtomicBool::new(false)),
            handles: Mutex::new(Vec::new()),
            _watcher: None,
        };

        engine
            .parts
            .run_full(Trigger::Initial, "initial snapshot")?;

        engine.spawn_watcher()?;
        engine.spawn_worker();
        engine.spawn_poller();
        engine.spawn_auditor();
        Ok(engine)
    }

    pub fn scan_once(&self) -> Result<ScanRun, ScanError> {
        let mut signals = self.parts.queue.drain();
        if signals.is_empty() {
            let mismatches = {
                let c = self.parts.core.lock().expect("core");
                stat_sweep(&c.disk_root, &Vec::new(), &c.cache, c.ignore.as_ref())
            };
            if !mismatches.is_empty() {
                signals.push(WatchSignal::PolledChanged(mismatches));
            }
        }
        self.parts.execute(signals, Trigger::Events)
    }

    pub fn current(&self) -> Option<Arc<CurrentScan>> {
        self.parts.current.read().expect("current lock").clone()
    }

    pub fn last_pass(&self) -> Option<(Trigger, PassStats)> {
        self.parts.core.lock().expect("core").last_pass.clone()
    }

    pub fn set_parent_manifest_id(&self, id: BlobId) {
        let mut core = self.parts.core.lock().expect("core lock");
        core.prev_manifest_id = id;
    }

    pub fn subscribe(&self) -> std::sync::mpsc::Receiver<ScanEvent> {
        let (tx, rx) = sync_channel(SUB_CHANNEL_BOUND);
        self.parts
            .subs
            .lock()
            .expect("subs lock")
            .push(Subscriber { tx, staged: None });
        rx
    }

    #[doc(hidden)]
    pub fn debug_inject_signal(&self, s: WatchSignal) {
        self.parts.queue.push(s);
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.parts.queue.wake();
        let handles = std::mem::take(&mut *self.handles.lock().expect("handles"));
        for h in handles {
            let _ = h.join();
        }
    }

    fn spawn_watcher(&mut self) -> Result<(), ScanError> {
        let queue = self.parts.queue.clone();
        let ignore = {
            let core = self.parts.core.lock().expect("core");
            core.ignore.clone()
        };
        let root = {
            let core = self.parts.core.lock().expect("core");
            core.disk_root.clone()
        };
        let root_for_cb = root.clone();

        let mut watcher = RecommendedWatcher::new(
            move |res: Result<Event, NotifyError>| match res {
                Ok(ev) => {
                    if ev.paths.is_empty() {
                        queue.push(WatchSignal::Overflow {
                            reason: "synthetic watcher event without paths".into(),
                        });
                        return;
                    }
                    let rels: Vec<RelPath> = ev
                        .paths
                        .iter()
                        .filter_map(|p| abs_to_rel(&root_for_cb, p))
                        .collect();
                    let filtered: Vec<RelPath> = rels
                        .into_iter()
                        .filter(|r| !any_prefix_ignored(r, ignore.as_ref()))
                        .collect();
                    if !filtered.is_empty() {
                        queue.push(WatchSignal::Changed(filtered));
                    }
                }
                Err(e) => match classify_watch_error(&e) {
                    ErrClass::Unwatchable(subtree) => queue.push(WatchSignal::Unwatchable {
                        subtree,
                        reason: e.to_string(),
                    }),
                    ErrClass::Loss => queue.push(WatchSignal::Overflow {
                        reason: format!("watcher error treated as event loss: {e}"),
                    }),
                },
            },
            notify::Config::default(),
        )
        .map_err(|e| ScanError::Watch(e.to_string()))?;

        watcher
            .watch(&root, RecursiveMode::Recursive)
            .map_err(|e| ScanError::Watch(format!("recursive watch failed: {e}")))?;
        self._watcher = Some(watcher);
        Ok(())
    }

    fn spawn_worker(&self) {
        let parts = self.parts.clone();
        let stop = self.stop.clone();
        let quiet = parts.core.lock().expect("core").cfg.quiet_window;
        let h = std::thread::Builder::new()
            .name("ferry-scan-worker".into())
            .spawn(move || loop {
                if !parts.queue.wait_nonempty(&stop, Duration::from_millis(200)) {
                    return;
                }

                let mut batch = parts.queue.drain();
                let mut deadline = Instant::now() + quiet;
                'debounce: while !stop.load(Ordering::Relaxed) {
                    let now = Instant::now();
                    if now >= deadline {
                        break;
                    }
                    if parts.queue.wait_arrival(&stop, deadline - now) {
                        batch.extend(parts.queue.drain());
                        deadline = Instant::now() + quiet;
                    } else {
                        break 'debounce;
                    }
                }
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                if batch.is_empty() {
                    continue;
                }
                if let Err(e) = parts.execute(batch, Trigger::Events) {
                    parts.report_failure(&e);
                }
            })
            .expect("spawn worker");
        self.handles.lock().expect("handles").push(h);
    }

    fn spawn_poller(&self) {
        let parts = self.parts.clone();
        let stop = self.stop.clone();
        let interval = parts.core.lock().expect("core").cfg.poll_interval;
        let h = std::thread::Builder::new()
            .name("ferry-scan-poller".into())
            .spawn(move || loop {
                sleep_slices(&stop, interval);
                if stop.load(Ordering::Relaxed) {
                    return;
                }

                {
                    let mut c = parts.core.lock().expect("core");
                    let exists = c.disk_root.is_dir();
                    if exists && c.root_gone {
                        c.root_gone = false;
                        drop(c);
                        parts.queue.push(WatchSignal::RootReturned);
                        continue;
                    }
                    if !exists && !c.root_gone {
                        c.root_gone = true;
                        drop(c);
                        parts.queue.push(WatchSignal::RootVanished);
                        continue;
                    }
                }

                let subtrees: Vec<RelPath> = {
                    let c = parts.core.lock().expect("core");
                    c.policy.polling.iter().cloned().collect()
                };
                for st in subtrees {
                    let mismatches = {
                        let c = parts.core.lock().expect("core");
                        stat_sweep(&c.disk_root, &st, &c.cache, c.ignore.as_ref())
                    };
                    if !mismatches.is_empty() {
                        parts.queue.push(WatchSignal::PolledChanged(mismatches));
                    }
                }
            })
            .expect("spawn poller");
        self.handles.lock().expect("handles").push(h);
    }

    fn spawn_auditor(&self) {
        let parts = self.parts.clone();
        let stop = self.stop.clone();
        let interval = parts.core.lock().expect("core").cfg.audit_interval;
        let h = std::thread::Builder::new()
            .name("ferry-scan-audit".into())
            .spawn(move || loop {
                sleep_slices(&stop, interval);
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                parts.queue.push(WatchSignal::AuditDue);
            })
            .expect("spawn auditor");
        self.handles.lock().expect("handles").push(h);
    }
}

impl Drop for ScanEngine {
    fn drop(&mut self) {
        self.stop();
    }
}

fn idle_run(trigger: Trigger) -> ScanRun {
    ScanRun {
        published: None,
        stats: PassStats {
            trigger,
            ..PassStats::default()
        },
    }
}

fn unix_now() -> (i64, u32) {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, d.subsec_nanos()),
        Err(e) => (-(e.duration().as_secs() as i64), 0),
    }
}

fn sleep_slices(stop: &AtomicBool, total: Duration) {
    let mut remaining = total;
    while remaining > Duration::ZERO && !stop.load(Ordering::Relaxed) {
        let slice = remaining.min(Duration::from_millis(100));
        std::thread::sleep(slice);
        remaining = remaining.saturating_sub(slice);
    }
}

fn abs_to_rel(root: &Path, p: &Path) -> Option<RelPath> {
    let stripped = p.strip_prefix(root).ok().or_else(|| {
        let p_str = p.to_str()?;
        let root_str = root.to_str()?;
        let clean_root = root_str.strip_prefix("/private").unwrap_or(root_str);
        let clean_p = p_str.strip_prefix("/private").unwrap_or(p_str);
        clean_p
            .strip_prefix(clean_root)
            .map(|rest| Path::new(rest.trim_start_matches('/')))
    })?;
    let mut rel = Vec::with_capacity(stripped.as_os_str().len());
    for c in stripped.components() {
        let s = c.as_os_str().to_str()?;
        rel.push(s.nfc().collect::<String>());
    }
    Some(rel)
}

fn any_prefix_ignored(rel: &RelPath, ignore: &dyn IgnorePolicy) -> bool {
    for c in rel {
        if crate::walk::is_store_component(c) {
            return true;
        }
    }
    for i in 1..rel.len() {
        if ignore.ignored(&rel[..i], EntryKind::Dir) {
            return true;
        }
    }
    ignore.ignored(rel, EntryKind::Dir) && ignore.ignored(rel, EntryKind::File)
}

enum ErrClass {
    Unwatchable(RelPath),

    Loss,
}

fn classify_watch_error(e: &NotifyError) -> ErrClass {
    use notify::ErrorKind::*;
    let subtree_of = |paths: &[std::path::PathBuf]| -> Option<RelPath> {
        paths
            .first()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(|s| vec![s.nfc().collect::<String>()])
    };
    match &e.kind {
        MaxFilesWatch => ErrClass::Unwatchable(Vec::new()),
        Io(ioe) => match ioe.raw_os_error() {
            Some(28 | 24 | 23) => ErrClass::Unwatchable(subtree_of(&e.paths).unwrap_or_default()),
            _ => ErrClass::Loss,
        },
        _ => ErrClass::Loss,
    }
}

fn stat_sweep(
    disk_root: &Path,
    subtree: &RelPath,
    cache: &DirCache,
    ignore: &dyn IgnorePolicy,
) -> Vec<RelPath> {
    let mut out = Vec::new();
    let mut start = disk_root.to_path_buf();
    for c in subtree {
        start.push(c);
    }
    if !start.is_dir() {
        let parent = subtree[..subtree.len().saturating_sub(1)].to_vec();
        out.push(parent);
        return out;
    }
    sweep_dir(disk_root, subtree, cache, ignore, &mut out);

    let mut dedup: std::collections::BTreeSet<RelPath> = out.drain(..).collect();
    for (dir_rel, cached) in cache.iter_within(subtree) {
        for e in &cached.node.entries {
            if e.name.contains('/') {
                continue;
            }
            let mut full = dir_rel.clone();
            full.push(e.name.clone());

            let kind = match &e.payload {
                EntryPayload::Dir { .. } => EntryKind::Dir,
                _ => EntryKind::File,
            };
            if ignore.ignored(&full, kind) {
                continue;
            }
            let mut p = disk_root.to_path_buf();
            for c in &full {
                p.push(c);
            }
            let exists = match &e.payload {
                EntryPayload::Dir { .. } => p.is_dir(),
                _ => p.symlink_metadata().is_ok(),
            };
            if !exists {
                dedup.insert(dir_rel.clone());
                break;
            }
        }
    }
    out.extend(dedup);
    out
}

fn sweep_dir(
    disk_root: &Path,
    rel: &RelPath,
    cache: &DirCache,
    ignore: &dyn IgnorePolicy,
    out: &mut Vec<RelPath>,
) {
    let mut disk = disk_root.to_path_buf();
    for c in rel {
        disk.push(c);
    }
    let Ok(rd) = std::fs::read_dir(&disk) else {
        out.push(rel[..rel.len().saturating_sub(1)].to_vec());
        return;
    };
    let mut names: Vec<std::ffi::OsString> = rd.flatten().map(|e| e.file_name()).collect();
    names.sort_by(|a, b| a.as_encoded_bytes().cmp(b.as_encoded_bytes()));
    for name in names {
        let Some(component) = name.to_str().map(|s| s.nfc().collect::<String>()) else {
            continue;
        };
        if crate::walk::is_store_component(&component) {
            continue;
        }
        let mut child_rel = rel.clone();
        child_rel.push(component.clone());
        let child_disk = disk.join(&name);

        let Ok(meta) = std::fs::symlink_metadata(&child_disk) else {
            out.push(rel.clone());
            continue;
        };
        let ft = meta.file_type();
        let kind = if ft.is_dir() {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        if ignore.ignored(&child_rel, kind) {
            continue;
        }
        if ft.is_dir() {
            sweep_dir(disk_root, &child_rel, cache, ignore, out);
        } else if ft.is_file() {
            let exec = live_exec_bit(&meta);
            let name_str = child_rel.last().expect("non-empty").as_str();
            let matches = cache.child_entry(rel, name_str).is_some_and(|prev| {
                matches!(&prev.payload, EntryPayload::File { size, .. } if
                    prev.exec == exec
                        && prev.mtime_sec == meta_mtime_sec(&meta)
                        && prev.mtime_nsec == meta_mtime_nsec(&meta)
                        && *size == meta.len())
            });
            if !matches {
                out.push(child_rel);
            }
        } else {
            out.push(child_rel);
        }
    }
}

fn live_exec_bit(meta: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

fn meta_mtime_sec(meta: &std::fs::Metadata) -> i64 {
    meta.modified().map_or((0, 0), ferry_platform::split_unix).0
}

fn meta_mtime_nsec(meta: &std::fs::Metadata) -> u32 {
    meta.modified().map_or((0, 0), ferry_platform::split_unix).1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    fn rel(parts: &[&str]) -> RelPath {
        parts.iter().map(std::string::ToString::to_string).collect()
    }

    fn seeded_cache(dir: &std::path::Path) -> (tempfile::TempDir, Store, DirCache) {
        let store_dir = tempfile::tempdir().unwrap();
        let store = Store::create(
            store_dir.path(),
            fmk(),
            Box::new(ferry_store::crypto::PassthroughCipher),
        )
        .unwrap();
        let id = ferry_store::snapshot::SnapshotIdentity {
            folder_id: [1; 16],
            device_id: [2; 32],
            parent_manifest_id: [0; 32],
            created_sec: 1,
            created_nsec: 0,
        };
        let _ = id;
        let mut cache = DirCache::new();
        let mut closed = BTreeSet::new();
        closed.insert(Vec::new());
        let mut stats = PassStats::default();
        Walker::run(
            &store,
            poly_of(3),
            &crate::ignore::NoIgnores,
            dir,
            &mut cache,
            &closed,
            Trigger::Initial,
            &ferry_store::snapshot::SnapshotIdentity {
                folder_id: [1; 16],
                device_id: [2; 32],
                parent_manifest_id: [0; 32],
                created_sec: 1,
                created_nsec: 0,
            },
            [0u8; 32],
            &mut stats,
        )
        .unwrap()
        .expect("seed pass publishes");
        (store_dir, store, cache)
    }

    #[test]
    fn stat_sweep_flags_changed_new_and_deleted_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("t");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        write_file(&root.join("same.txt"), b"aaa", false, (1, 0));
        write_file(&root.join("changed.txt"), b"bbb", false, (2, 0));
        write_file(&root.join("sub/deep.txt"), b"ccc", true, (3, 0));

        let (_sd, _store, cache) = seeded_cache(&root);

        write_file(&root.join("changed.txt"), b"CHANGED", false, (99, 9));
        std::fs::remove_file(root.join("sub/deep.txt")).unwrap();
        write_file(&root.join("fresh.txt"), b"new", false, (5, 5));

        let found = super::stat_sweep(&root, &Vec::new(), &cache, &crate::ignore::NoIgnores);
        let mut got = found.clone();
        got.sort();

        assert!(
            got.contains(&rel(&["changed.txt"])),
            "content change flagged: {got:?}"
        );
        assert!(
            got.contains(&rel(&["fresh.txt"])),
            "new file flagged: {got:?}"
        );
        assert!(
            got.contains(&rel(&[])) || got.contains(&rel(&["sub"])),
            "deleted file surfaces via its parent dir: {got:?}"
        );
        assert!(
            !got.contains(&rel(&["same.txt"])),
            "untouched file must NOT be flagged: {got:?}"
        );
    }

    #[test]
    fn stat_sweep_respects_ignore_policy() {
        struct SkipAll;
        impl crate::ignore::IgnorePolicy for SkipAll {
            fn ignored(&self, _rel: &[String], _kind: EntryKind) -> bool {
                true
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("t");
        std::fs::create_dir_all(&root).unwrap();
        write_file(&root.join("x.txt"), b"a", false, (1, 0));
        let (_sd, _store, cache) = seeded_cache(&root);
        write_file(&root.join("x.txt"), b"different", false, (2, 0));
        let found = super::stat_sweep(&root, &Vec::new(), &cache, &SkipAll);
        assert!(found.is_empty(), "ignored paths are never swept: {found:?}");
    }

    #[test]
    fn stalled_subscriber_retention_is_bounded_latest_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("t");
        std::fs::create_dir_all(&root).unwrap();
        write_file(&root.join("a.txt"), b"a", false, (1, 0));
        let (_sd, store) = fresh_store();
        let engine = ScanEngine::watch(
            root,
            StoreHandle {
                store: Arc::new(store),
                poly: poly_of(3),
                folder_id: [1; 16],
                device_id: [2; 32],
            },
        )
        .unwrap();
        let baseline = engine.current().expect("initial pass published");

        let rx = engine.subscribe();
        const N: usize = 64;
        for _ in 0..N {
            engine.parts.publish(baseline.clone());
        }

        assert!(matches!(rx.try_recv(), Ok(ScanEvent::Updated(_))));
        assert!(rx.try_recv().is_err(), "retention must be bounded");

        let subs = engine.parts.subs.lock().expect("subs lock");
        assert_eq!(subs.len(), 1);
        match &subs[0].staged {
            Some(ScanEvent::Updated(cur)) => assert!(
                Arc::ptr_eq(cur, &baseline),
                "staged event must reflect the latest pass"
            ),
            other => panic!("expected staged Updated event, got {other:?}"),
        }
        drop(subs);

        drop(rx);
        engine.parts.publish(baseline.clone());
        assert!(engine.parts.subs.lock().expect("subs lock").is_empty());

        engine.stop();
    }
}
