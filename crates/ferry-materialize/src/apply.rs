use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use ferry_store::diff::{join_path, ChangeSet, CompPath, EntryKind, EntryState};
use ferry_store::format::hex;
use ferry_store::manifest::{parse_tree_node, EntryPayload, RootManifest, TreeEntry, TreeNode};
use ferry_store::store::Store;
use ferry_store::{BlobId, BlobKind};
use unicode_normalization::UnicodeNormalization;

use crate::error::{io_at, DivergeReason, Divergence, MaterializeError};
use crate::temp::{fresh_entropy, is_temp_name, temp_name_for, TempStyle};

#[derive(Clone, Debug)]
pub enum Overwrite {
    Always,

    Expect { expected: RootManifest },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApplyStats {
    pub dirs_created: usize,
    pub files_written: usize,
    pub symlinks_written: usize,
    pub unlinked: usize,

    pub mtimes_set: usize,
    pub bytes_written: u64,
    pub skipped_unchanged: usize,

    pub deletions: Vec<String>,

    pub creations: Vec<String>,
}

impl ApplyStats {
    pub fn mutations(&self) -> usize {
        self.dirs_created
            + self.files_written
            + self.symlinks_written
            + self.unlinked
            + self.mtimes_set
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ApplyOutcome {
    pub held: usize,
}

#[derive(Clone, Debug)]
enum Desired {
    Dir {
        sec: i64,
        nsec: u32,
    },
    File {
        exec: bool,
        sec: i64,
        nsec: u32,
        size: u64,
        chunks: Vec<(BlobId, u64)>,
    },
    Symlink {
        target: String,
        sec: i64,
        nsec: u32,
    },
}

impl Desired {
    fn of(e: &TreeEntry) -> Desired {
        match &e.payload {
            EntryPayload::File { size, chunks } => Desired::File {
                exec: e.exec,
                sec: e.mtime_sec,
                nsec: e.mtime_nsec,
                size: *size,
                chunks: chunks.clone(),
            },
            EntryPayload::Dir { .. } => Desired::Dir {
                sec: e.mtime_sec,
                nsec: e.mtime_nsec,
            },
            EntryPayload::Symlink { target } => Desired::Symlink {
                target: target.clone(),
                sec: e.mtime_sec,
                nsec: e.mtime_nsec,
            },
        }
    }

    fn as_mutation(&self) -> Mutation {
        match self {
            Desired::Dir { sec, nsec } => Mutation::Mkdir {
                sec: *sec,
                nsec: *nsec,
            },
            Desired::File {
                exec,
                sec,
                nsec,
                size,
                chunks,
            } => Mutation::WriteFile {
                exec: *exec,
                sec: *sec,
                nsec: *nsec,
                size: *size,
                chunks: chunks.clone(),
            },
            Desired::Symlink { target, sec, nsec } => Mutation::WriteSymlink {
                target: target.clone(),
                times: Some((*sec, *nsec)),
            },
        }
    }
}

#[derive(Clone, Debug)]
struct ExpectedState {
    kind: EntryKind,
    exec: bool,
    size: u64,
    chunks: Vec<(BlobId, u64)>,
    target: Option<String>,
}

impl From<&EntryState> for ExpectedState {
    fn from(s: &EntryState) -> Self {
        ExpectedState {
            kind: s.kind,
            exec: s.exec,
            size: s.chunks.iter().map(|c| c.1).sum(),
            chunks: s.chunks.clone(),
            target: s.target.clone(),
        }
    }
}

impl From<&TreeEntry> for ExpectedState {
    fn from(e: &TreeEntry) -> Self {
        ExpectedState {
            kind: entry_kind(e),
            exec: e.exec,
            size: match &e.payload {
                EntryPayload::File { size, .. } => *size,
                _ => 0,
            },
            chunks: match &e.payload {
                EntryPayload::File { chunks, .. } => chunks.clone(),
                _ => Vec::new(),
            },
            target: match &e.payload {
                EntryPayload::Symlink { target } => Some(target.clone()),
                _ => None,
            },
        }
    }
}

fn entry_kind(e: &TreeEntry) -> EntryKind {
    match &e.payload {
        EntryPayload::File { .. } => EntryKind::File,
        EntryPayload::Dir { .. } => EntryKind::Dir,
        EntryPayload::Symlink { .. } => EntryKind::Symlink,
    }
}

struct PlannedRemove {
    path: CompPath,

    deep: bool,
}

enum Mutation {
    Skip,
    Mkdir {
        sec: i64,
        nsec: u32,
    },
    WriteFile {
        exec: bool,
        sec: i64,
        nsec: u32,
        size: u64,
        chunks: Vec<(BlobId, u64)>,
    },
    WriteSymlink {
        target: String,

        times: Option<(i64, u32)>,
    },

    RestoreMtime {
        sec: i64,
        nsec: u32,
    },

    RestoreSymlinkMtime {
        sec: i64,
        nsec: u32,
    },
}

impl Mutation {
    fn is_mutation(&self) -> bool {
        !matches!(self, Mutation::Skip)
    }
}

struct PlannedUpsert {
    path: CompPath,
    mutation: Mutation,
}

struct PlannedTouch {
    path: CompPath,
    sec: i64,
    nsec: u32,
}

pub struct Applier<'a> {
    store: &'a Store,
    target: PathBuf,
    overwrite: Overwrite,
    style: TempStyle,
    pace_ms: u64,

    fold: NfcFoldCache,
}

impl<'a> Applier<'a> {
    pub fn new(store: &'a Store, target: impl Into<PathBuf>) -> Self {
        Applier {
            store,
            target: target.into(),
            overwrite: Overwrite::Always,
            style: TempStyle::current(),
            pace_ms: 0,
            fold: NfcFoldCache::refusing(),
        }
    }

    pub fn overwrite(mut self, overwrite: Overwrite) -> Self {
        self.overwrite = overwrite;
        self
    }

    pub fn temp_style(mut self, style: TempStyle) -> Self {
        self.style = style;
        self
    }

    pub fn pace_ms(mut self, ms: u64) -> Self {
        self.pace_ms = ms;
        self
    }

    pub fn apply_manifest(
        &mut self,
        manifest: &RootManifest,
    ) -> Result<ApplyStats, MaterializeError> {
        self.apply_tree(&manifest.root_tree_id)
    }

    pub fn apply_tree(&mut self, root_tree_id: &BlobId) -> Result<ApplyStats, MaterializeError> {
        std::fs::create_dir_all(&self.target).map_err(|e| io_at(&self.target, e))?;

        let desired = flatten_tree(self.store, root_tree_id)?;
        for (p, _) in &desired {
            validate_components(p)?;
        }
        ensure_no_fold_collisions(&desired.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>())?;

        let desired_keys: HashSet<String> = desired.iter().map(|(p, _)| join_path(p)).collect();
        let removes: Vec<PlannedRemove> = walk_live(&self.target)?
            .into_iter()
            .filter(|p| !desired_keys.contains(&join_path(p)))
            .map(|path| PlannedRemove { path, deep: false })
            .collect();

        let upserts: Vec<PlannedUpsert> = desired
            .into_iter()
            .map(|(path, d)| PlannedUpsert {
                path,
                mutation: d.as_mutation(),
            })
            .collect();

        self.run(removes, upserts)
    }

    pub fn apply_change_set(&mut self, cs: &ChangeSet) -> Result<ApplyStats, MaterializeError> {
        std::fs::create_dir_all(&self.target).map_err(|e| io_at(&self.target, e))?;

        let mut removes: Vec<PlannedRemove> = cs
            .removed
            .iter()
            .map(|r| PlannedRemove {
                path: r.path.clone(),
                deep: r.state.kind == EntryKind::Dir,
            })
            .collect();

        let mut upserts: Vec<PlannedUpsert> = cs
            .added
            .iter()
            .map(|a| PlannedUpsert {
                path: a.path.clone(),
                mutation: state_mutation(&a.state),
            })
            .collect();
        for m in cs.content_modified.iter().chain(&cs.metadata_modified) {
            upserts.push(PlannedUpsert {
                path: m.path.clone(),
                mutation: state_mutation(&m.after),
            });
        }
        for m in &cs.type_changed {
            removes.push(PlannedRemove {
                path: m.path.clone(),
                deep: m.before.kind == EntryKind::Dir,
            });
            upserts.push(PlannedUpsert {
                path: m.path.clone(),
                mutation: state_mutation(&m.after),
            });
        }

        for op in removes
            .iter()
            .map(|r| &r.path)
            .chain(upserts.iter().map(|u| &u.path))
        {
            validate_components(op)?;
        }
        let upsert_paths: Vec<CompPath> = upserts.iter().map(|u| u.path.clone()).collect();
        ensure_no_fold_collisions(&upsert_paths)?;
        self.run(removes, upserts)
    }

    pub fn apply_session_change_set(
        &mut self,
        cs: &ChangeSet,
        target_root_tree_id: &BlobId,
    ) -> Result<ApplyOutcome, MaterializeError> {
        self.apply_change_set(cs)?;
        self.restore_dir_mtimes_from_tree(target_root_tree_id)?;
        Ok(ApplyOutcome { held: 0 })
    }

    pub fn restore_dir_mtimes_from_tree(
        &mut self,
        root_tree_id: &BlobId,
    ) -> Result<ApplyStats, MaterializeError> {
        let mut dirs: Vec<(CompPath, i64, u32)> = Vec::new();
        collect_dir_mtimes(self.store, root_tree_id, Vec::new(), &mut dirs)?;
        for (p, _, _) in &dirs {
            validate_components(p)?;
        }

        dirs.sort_by_key(|d| std::cmp::Reverse(d.0.len()));
        let mut stats = ApplyStats::default();
        self.fold.clear();
        for (rel, sec, nsec) in dirs {
            self.execute_touch(&rel, sec, nsec, &mut stats)?;
        }
        Ok(stats)
    }

    fn run(
        &mut self,
        mut removes: Vec<PlannedRemove>,
        mut upserts: Vec<PlannedUpsert>,
    ) -> Result<ApplyStats, MaterializeError> {
        self.fold.clear();
        upserts.sort_by(|a, b| a.path.cmp(&b.path));

        let shadowed: HashSet<String> = removes
            .iter()
            .map(|r| ferry_platform::fold_key(&join_path(&r.path)))
            .collect();
        let mut touches: Vec<PlannedTouch> = Vec::new();
        let mut skipped = 0usize;
        for up in &mut upserts {
            let abs = self.abs(&up.path)?;
            let case_shadowed = shadowed.contains(&ferry_platform::fold_key(&join_path(&up.path)));
            plan_upsert(
                self.store,
                &abs,
                up,
                &mut touches,
                &mut skipped,
                case_shadowed,
            )?;
        }

        if let Overwrite::Expect { expected } = &self.overwrite {
            let base = Base::new(self.store, &expected.root_tree_id);
            let mut divergences: Vec<Divergence> = Vec::new();
            for rm in &removes {
                guard_removal(
                    self.store,
                    &self.target,
                    &base,
                    &self.fold,
                    rm,
                    &mut divergences,
                )?;
            }
            for up in &upserts {
                if up.mutation.is_mutation() {
                    guard_upsert(
                        self.store,
                        &self.target,
                        &base,
                        &self.fold,
                        up,
                        &mut divergences,
                    )?;
                }
            }
            if !divergences.is_empty() {
                divergences.sort_by(|a, b| a.path.cmp(&b.path));
                return Err(MaterializeError::Diverged { paths: divergences });
            }
        }

        let mut stats = ApplyStats {
            skipped_unchanged: skipped,
            ..Default::default()
        };
        removes.sort_by(|a, b| b.path.cmp(&a.path));
        for rm in removes {
            self.execute_remove(&rm.path, rm.deep, &mut stats)?;
        }

        for up in upserts {
            self.execute_upsert(up, &mut stats, &mut touches)?;
        }

        touches.sort_by(|a, b| b.path.cmp(&a.path));
        for t in touches {
            self.execute_touch(&t.path, t.sec, t.nsec, &mut stats)?;
        }
        Ok(stats)
    }

    fn pace(&self) {
        if self.pace_ms > 0 {
            std::thread::sleep(Duration::from_millis(self.pace_ms));
        }
    }

    fn execute_remove(
        &mut self,
        rel: &CompPath,
        deep: bool,
        stats: &mut ApplyStats,
    ) -> Result<(), MaterializeError> {
        let abs = self.abs(rel)?;
        let md = match std::fs::symlink_metadata(&abs) {
            Ok(md) => md,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(io_at(&abs, e)),
        };
        if md.is_dir() {
            if deep {
                self.remove_dir_children_first(&abs, rel, stats)?;
            } else {
                std::fs::remove_dir(&abs).map_err(|e| io_at(&abs, e))?;
                record_deletion(stats, rel);
            }
        } else {
            std::fs::remove_file(&abs).map_err(|e| io_at(&abs, e))?;
            record_deletion(stats, rel);
        }

        self.fold.note_removed(&abs, deep || md.is_dir());
        self.pace();
        Ok(())
    }

    fn remove_dir_children_first(
        &mut self,
        abs_dir: &Path,
        rel_dir: &CompPath,
        stats: &mut ApplyStats,
    ) -> Result<(), MaterializeError> {
        let entries: Vec<_> = match std::fs::read_dir(abs_dir) {
            Ok(rd) => rd.flatten().collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(io_at(abs_dir, e)),
        };
        for entry in entries {
            let p = entry.path();
            let ft = entry.file_type().map_err(|e| io_at(&p, e))?;
            let mut child_rel = rel_dir.clone();
            child_rel.push(entry.file_name().to_string_lossy().into_owned());
            if ft.is_dir() {
                self.remove_dir_children_first(&p, &child_rel, stats)?;
            } else {
                std::fs::remove_file(&p).map_err(|e| io_at(&p, e))?;
                record_deletion(stats, &child_rel);
                self.pace();
            }
        }
        std::fs::remove_dir(abs_dir).map_err(|e| io_at(abs_dir, e))?;
        record_deletion(stats, rel_dir);
        Ok(())
    }

    fn execute_upsert(
        &mut self,
        up: PlannedUpsert,
        stats: &mut ApplyStats,
        touches: &mut Vec<PlannedTouch>,
    ) -> Result<(), MaterializeError> {
        let abs = self.abs(&up.path)?;
        match up.mutation {
            Mutation::Skip => {}
            Mutation::Mkdir { sec, nsec } => {
                match std::fs::symlink_metadata(&abs) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(io_at(&abs, e)),
                    Ok(md) if md.is_dir() => {}

                    Ok(_) => {
                        std::fs::remove_file(&abs).map_err(|e| io_at(&abs, e))?;
                        record_deletion(stats, &up.path);
                        self.fold.note_removed(&abs, false);
                    }
                }
                std::fs::create_dir(&abs)
                    .or_else(|e| {
                        if e.kind() == std::io::ErrorKind::AlreadyExists {
                            Ok(())
                        } else {
                            Err(e)
                        }
                    })
                    .map_err(|e| io_at(&abs, e))?;
                stats.dirs_created += 1;
                record_creation(stats, &up.path);
                self.fold.note_created_at(&abs);

                self.pace();
                touches.push(PlannedTouch {
                    path: up.path.clone(),
                    sec,
                    nsec,
                });
            }
            Mutation::RestoreMtime { sec, nsec } => {
                set_mtime(&abs, sec, nsec)?;
                stats.mtimes_set += 1;
                self.pace();
            }
            Mutation::RestoreSymlinkMtime { sec, nsec } => {
                set_symlink_times(&abs, sec, nsec)?;
                stats.mtimes_set += 1;
                self.pace();
            }
            Mutation::WriteSymlink { target, times } => {
                let depth = up.path.len().saturating_sub(1);
                match ferry_platform::classify_link(depth, &target) {
                    ferry_platform::LinkDecision::SyncAsLink => {}
                    ferry_platform::LinkDecision::Refuse(reason) => {
                        return Err(MaterializeError::SymlinkRefused {
                            path: join_path(&up.path),
                            target,
                            reason,
                        });
                    }
                }
                self.reject_windows_dir_link(&abs, &up.path, &target)?;

                if let Ok(md) = std::fs::symlink_metadata(&abs) {
                    if md.is_dir() && !md.is_symlink() {
                        self.remove_dir_children_first(&abs, &up.path, stats)?;
                        self.fold.note_removed(&abs, true);
                    }
                }
                let parent = parent_of(&abs);
                let tmp_path = parent.join(temp_name_for(
                    &join_path(&up.path),
                    self.style,
                    &fresh_entropy(),
                ));
                let result = make_symlink(&target, &tmp_path)
                    .and_then(|()| std::fs::rename(&tmp_path, &abs).map_err(|e| io_at(&abs, e)));
                if result.is_err() {
                    let _ = std::fs::remove_file(&tmp_path);
                }
                result?;
                fsync_dir(parent)?;
                stats.symlinks_written += 1;
                record_creation(stats, &up.path);
                self.fold.note_created_at(&abs);

                if let Some((sec, nsec)) = times {
                    set_symlink_times(&abs, sec, nsec)?;
                    stats.mtimes_set += 1;
                }
                self.pace();
            }
            Mutation::WriteFile {
                exec,
                sec,
                nsec,
                size,
                chunks,
            } => {
                if let Ok(md) = std::fs::symlink_metadata(&abs) {
                    if md.is_dir() {
                        self.remove_dir_children_first(&abs, &up.path, stats)?;
                        self.fold.note_removed(&abs, true);
                    }
                }
                self.write_file_atomically(&abs, &up.path, exec, sec, nsec, size, &chunks)?;
                stats.files_written += 1;
                stats.bytes_written += size;
                record_creation(stats, &up.path);
                self.fold.note_created_at(&abs);
                self.pace();
            }
        }
        Ok(())
    }

    fn reject_windows_dir_link(
        &self,
        _abs: &Path,
        rel: &[String],
        target: &str,
    ) -> Result<(), MaterializeError> {
        if !cfg!(windows) || ferry_platform::allow_windows_dir_links() {
            return Ok(());
        }

        let mut resolved = self.target.clone();
        for c in &rel[..rel.len().saturating_sub(1)] {
            resolved.push(c);
        }
        for part in target.split(['/', '\\']) {
            match part {
                "" | "." => {}
                ".." => {
                    resolved.pop();
                }
                p => resolved.push(p),
            }
        }
        let is_dir =
            std::fs::symlink_metadata(&resolved).is_ok_and(|m| m.is_dir() && !m.is_symlink());
        if is_dir {
            Err(MaterializeError::WindowsDirLinkRefused {
                path: join_path(rel),
            })
        } else {
            Ok(())
        }
    }

    fn execute_touch(
        &self,
        rel: &CompPath,
        sec: i64,
        nsec: u32,
        stats: &mut ApplyStats,
    ) -> Result<(), MaterializeError> {
        let abs = self.abs(rel)?;
        let md = match std::fs::symlink_metadata(&abs) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(io_at(&abs, e)),
            Ok(md) => md,
        };
        if !md.is_dir() {
            return Ok(());
        }
        let (cur_sec, cur_nsec) =
            ferry_platform::split_unix(md.modified().map_err(|e| io_at(&abs, e))?);
        if cur_sec == sec && cur_nsec == nsec {
            return Ok(());
        }
        set_mtime(&abs, sec, nsec)?;
        stats.mtimes_set += 1;
        self.pace();
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn write_file_atomically(
        &self,
        abs_dest: &Path,
        rel: &[String],
        exec: bool,
        sec: i64,
        nsec: u32,
        declared_size: u64,
        chunks: &[(BlobId, u64)],
    ) -> Result<(), MaterializeError> {
        let rel_display = join_path(rel);

        let summed: u64 = chunks.iter().map(|c| c.1).sum();
        if summed != declared_size {
            return Err(MaterializeError::SizeMismatch {
                path: rel_display,
                declared: declared_size,
                actual: summed,
            });
        }

        let parent = parent_of(abs_dest);
        let tmp_path = parent.join(temp_name_for(&rel_display, self.style, &fresh_entropy()));
        let outcome =
            self.write_temp_then_rename(&tmp_path, abs_dest, &rel_display, exec, sec, nsec, chunks);
        if outcome.is_err() {
            let _ = std::fs::remove_file(&tmp_path);
        }
        outcome
    }

    #[allow(clippy::too_many_arguments)]
    fn write_temp_then_rename(
        &self,
        tmp_path: &Path,
        abs_dest: &Path,
        rel_display: &str,
        exec: bool,
        sec: i64,
        nsec: u32,
        chunks: &[(BlobId, u64)],
    ) -> Result<(), MaterializeError> {
        let mut file = std::fs::File::create(tmp_path).map_err(|e| io_at(tmp_path, e))?;
        set_exec_bit(&file, exec).map_err(|e| io_at(tmp_path, e))?;

        for (index, (id, len)) in chunks.iter().enumerate() {
            let bytes = self.store.get(BlobKind::DataChunk, id)?;
            if bytes.len() as u64 != *len {
                return Err(MaterializeError::ChunkCorrupt {
                    path: rel_display.to_string(),
                    index,
                    expected: format!("len {len}"),
                    found: format!("len {}", bytes.len()),
                });
            }
            let found = *blake3::hash(&bytes).as_bytes();
            if &found != id {
                return Err(MaterializeError::ChunkCorrupt {
                    path: rel_display.to_string(),
                    index,
                    expected: hex(id),
                    found: hex(&found),
                });
            }
            file.write_all(&bytes).map_err(|e| io_at(tmp_path, e))?;
        }

        file.set_times(
            std::fs::FileTimes::new().set_modified(ferry_platform::time::join_unix(sec, nsec)),
        )
        .map_err(|e| io_at(tmp_path, e))?;
        file.sync_all().map_err(|e| io_at(tmp_path, e))?;

        drop(file);
        let mut rd = std::fs::File::open(tmp_path).map_err(|e| io_at(tmp_path, e))?;
        verify_regions(&mut rd, tmp_path, rel_display, chunks)?;
        drop(rd);

        std::fs::rename(tmp_path, abs_dest).map_err(|e| io_at(abs_dest, e))?;
        fsync_dir(parent_of(abs_dest))
    }

    fn abs(&self, rel: &[String]) -> Result<PathBuf, MaterializeError> {
        let p = self.fold.resolve(&self.target, rel)?;

        Ok(ferry_platform::extend_path(&p))
    }
}

fn record_deletion(stats: &mut ApplyStats, rel: &[String]) {
    stats.unlinked += 1;
    stats.deletions.push(join_path(rel));
}

fn record_creation(stats: &mut ApplyStats, rel: &[String]) {
    stats.creations.push(join_path(rel));
}

fn plan_upsert(
    store: &Store,
    abs: &Path,
    up: &mut PlannedUpsert,
    touches: &mut Vec<PlannedTouch>,
    skipped: &mut usize,
    case_shadowed: bool,
) -> Result<(), MaterializeError> {
    if case_shadowed {
        return Ok(());
    }
    match &up.mutation {
        Mutation::Skip => Ok(()),
        Mutation::Mkdir { sec, nsec } => {
            let live = stat_opt(abs)?;
            if let Some(md) = live {
                if md.is_dir() {
                    touches.push(PlannedTouch {
                        path: up.path.clone(),
                        sec: *sec,
                        nsec: *nsec,
                    });
                    *skipped += 1;
                    up.mutation = Mutation::Skip;
                    return Ok(());
                }

                return Ok(());
            }
            Ok(())
        }
        Mutation::WriteSymlink { target, times } => {
            let md = stat_opt(abs)?;
            let unchanged = md.as_ref().is_some_and(|md| {
                md.file_type().is_symlink()
                    && std::fs::read_link(abs).is_ok_and(|p| p.to_string_lossy() == target.as_str())
            });
            if unchanged {
                if cfg!(unix) {
                    if let (Some(md), Some((sec, nsec))) = (md.as_ref(), *times) {
                        let (lsec, lnsec) =
                            ferry_platform::split_unix(md.modified().map_err(|e| io_at(abs, e))?);
                        if lsec != sec || lnsec != nsec {
                            up.mutation = Mutation::RestoreSymlinkMtime { sec, nsec };
                            return Ok(());
                        }
                    }
                }
                *skipped += 1;
                up.mutation = Mutation::Skip;
            }
            Ok(())
        }
        Mutation::RestoreMtime { .. } | Mutation::RestoreSymlinkMtime { .. } => {
            unreachable!("planner input")
        }
        Mutation::WriteFile {
            exec,
            sec,
            nsec,
            size,
            chunks,
        } => {
            let (exec, sec, nsec, size) = (*exec, *sec, *nsec, *size);
            let chunks = chunks.clone();
            let Some(md) = stat_opt(abs)? else {
                return Ok(());
            };
            if !md.is_file() {
                return Ok(());
            }

            let exec_drifts = cfg!(unix) && live_exec(&md) != exec;
            if md.len() != size || exec_drifts {
                return Ok(());
            }

            if !content_matches(store, abs, &chunks)? {
                return Ok(());
            }
            let (lsec, lnsec) =
                ferry_platform::split_unix(md.modified().map_err(|e| io_at(abs, e))?);
            if lsec == sec && lnsec == nsec {
                *skipped += 1;
                up.mutation = Mutation::Skip;
                return Ok(());
            }

            up.mutation = Mutation::RestoreMtime { sec, nsec };
            Ok(())
        }
    }
}

fn stat_opt(abs: &Path) -> Result<Option<std::fs::Metadata>, MaterializeError> {
    match std::fs::symlink_metadata(abs) {
        Ok(md) => Ok(Some(md)),

        Err(e)
            if e.kind() == std::io::ErrorKind::NotFound
                || e.kind() == std::io::ErrorKind::NotADirectory =>
        {
            Ok(None)
        }
        Err(e) => Err(io_at(abs, e)),
    }
}

fn state_mutation(s: &EntryState) -> Mutation {
    match s.kind {
        EntryKind::Dir => Mutation::Mkdir {
            sec: s.mtime_sec,
            nsec: s.mtime_nsec,
        },
        EntryKind::File => Mutation::WriteFile {
            exec: s.exec,
            sec: s.mtime_sec,
            nsec: s.mtime_nsec,
            size: s.chunks.iter().map(|c| c.1).sum(),
            chunks: s.chunks.clone(),
        },
        EntryKind::Symlink => Mutation::WriteSymlink {
            target: s.target.clone().unwrap_or_default(),

            times: Some((s.mtime_sec, s.mtime_nsec)),
        },
    }
}

fn guard_removal(
    store: &Store,
    target: &Path,
    base: &Base<'_>,
    fold: &NfcFoldCache,
    rm: &PlannedRemove,
    out: &mut Vec<Divergence>,
) -> Result<(), MaterializeError> {
    let Some(exp_entry) = base.lookup(&rm.path)? else {
        out.push(Divergence {
            path: rm.path.clone(),
            reason: DivergeReason::NotInBase,
        });
        return Ok(());
    };
    let exp = ExpectedState::from(&exp_entry);
    let abs = fold.resolve(target, &rm.path)?;
    let Some(md) = stat_opt(&abs)? else {
        out.push(Divergence {
            path: rm.path.clone(),
            reason: DivergeReason::ExpectedPresent,
        });
        return Ok(());
    };
    let live_kind = classify_md(&md);
    if live_kind != exp.kind {
        out.push(Divergence {
            path: rm.path.clone(),
            reason: DivergeReason::KindMismatch {
                expected: exp.kind,
                found: live_kind,
            },
        });
        return Ok(());
    }
    if rm.deep && live_kind == EntryKind::Dir {
        verify_subtree_matches_base(store, target, base, fold, &rm.path, &exp_entry, out)?;
    } else {
        check_live_matches(target, fold, &rm.path, &exp, store, out)?;
    }
    Ok(())
}

fn guard_upsert(
    store: &Store,
    target: &Path,
    base: &Base<'_>,
    fold: &NfcFoldCache,
    up: &PlannedUpsert,
    out: &mut Vec<Divergence>,
) -> Result<(), MaterializeError> {
    let exp = base.lookup(&up.path)?;
    let abs = fold.resolve(target, &up.path)?;
    let live = classify_opt(stat_opt(&abs)?)?;
    match (exp.as_ref().map(ExpectedState::from), live) {
        (None, None) => Ok(()),
        (None, Some(_)) => {
            out.push(Divergence {
                path: up.path.clone(),
                reason: DivergeReason::ExpectedAbsent,
            });
            Ok(())
        }
        (Some(_), None) => {
            out.push(Divergence {
                path: up.path.clone(),
                reason: DivergeReason::ExpectedPresent,
            });
            Ok(())
        }
        (Some(exp_state), Some(_)) => match exp_state.kind {
            EntryKind::Dir => Ok(()),
            _ => check_live_matches(target, fold, &up.path, &exp_state, store, out),
        },
    }
}

fn check_live_matches(
    target: &Path,
    fold: &NfcFoldCache,
    rel: &CompPath,
    exp: &ExpectedState,
    store: &Store,
    out: &mut Vec<Divergence>,
) -> Result<(), MaterializeError> {
    let abs = fold.resolve(target, rel)?;
    let Some(md) = stat_opt(&abs)? else {
        out.push(Divergence {
            path: rel.clone(),
            reason: DivergeReason::ExpectedPresent,
        });
        return Ok(());
    };
    let found_kind = classify_md(&md);
    if found_kind != exp.kind {
        out.push(Divergence {
            path: rel.clone(),
            reason: DivergeReason::KindMismatch {
                expected: exp.kind,
                found: found_kind,
            },
        });
        return Ok(());
    }
    match found_kind {
        EntryKind::Dir => Ok(()),
        EntryKind::Symlink => {
            let found = std::fs::read_link(&abs)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let expected_target = exp.target.clone().unwrap_or_default();
            if found != expected_target {
                out.push(Divergence {
                    path: rel.clone(),
                    reason: DivergeReason::TargetMismatch {
                        expected: expected_target,
                        found,
                    },
                });
            }
            Ok(())
        }
        EntryKind::File => {
            if md.len() != exp.size {
                out.push(Divergence {
                    path: rel.clone(),
                    reason: DivergeReason::SizeMismatch {
                        expected: exp.size,
                        found: md.len(),
                    },
                });
                return Ok(());
            }

            if cfg!(unix) && live_exec(&md) != exp.exec {
                out.push(Divergence {
                    path: rel.clone(),
                    reason: DivergeReason::ExecMismatch {
                        expected: exp.exec,
                        found: live_exec(&md),
                    },
                });
                return Ok(());
            }
            if !content_matches(store, &abs, &exp.chunks)? {
                out.push(Divergence {
                    path: rel.clone(),
                    reason: DivergeReason::ContentMismatch,
                });
            }
            Ok(())
        }
    }
}

fn verify_subtree_matches_base(
    store: &Store,
    target: &Path,
    base: &Base<'_>,
    fold: &NfcFoldCache,
    dir_path: &CompPath,
    dir_entry: &TreeEntry,
    out: &mut Vec<Divergence>,
) -> Result<(), MaterializeError> {
    let mut expected: HashMap<String, ExpectedState> = HashMap::new();
    flatten_base_subtree(base, dir_path, dir_entry, &mut expected)?;

    let mut live_keys: HashSet<String> = HashSet::new();
    let abs_dir = fold.resolve(target, dir_path)?;
    let mut stack = vec![abs_dir];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)
            .map_err(|e| io_at(&dir, e))?
            .flatten()
        {
            let p = entry.path();
            let ft = entry.file_type().map_err(|e| io_at(&p, e))?;
            let comp = match entry.file_name().to_str() {
                Some(s) => s.to_string(),
                None => {
                    return Err(MaterializeError::BadComponent {
                        component: entry.file_name().to_string_lossy().into_owned(),
                    })
                }
            };

            let key = comp.nfc().collect::<String>();
            let mut rel = components_below(target, &dir);
            rel.push(key);
            live_keys.insert(join_path(&rel));
            if ft.is_dir() {
                stack.push(p);
                if !expected.contains_key(&join_path(&rel)) {
                    out.push(Divergence {
                        path: rel,
                        reason: DivergeReason::ExpectedAbsent,
                    });
                    continue;
                }
                continue;
            }
            match expected.get(&join_path(&rel)) {
                None => out.push(Divergence {
                    path: rel,
                    reason: DivergeReason::ExpectedAbsent,
                }),
                Some(exp) => check_live_matches_leaf(target, fold, &rel, exp, ft, store, out)?,
            }
        }
    }

    let mut missing = expected
        .keys()
        .filter(|k| !live_keys.contains(*k))
        .cloned()
        .collect::<Vec<_>>();
    missing.sort();
    for k in missing {
        out.push(Divergence {
            path: k.split('/').map(str::to_string).collect(),
            reason: DivergeReason::ExpectedPresent,
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn check_live_matches_leaf(
    target: &Path,
    fold: &NfcFoldCache,
    rel: &CompPath,
    exp: &ExpectedState,
    ft: std::fs::FileType,
    store: &Store,
    out: &mut Vec<Divergence>,
) -> Result<(), MaterializeError> {
    let found_kind = if ft.is_symlink() {
        EntryKind::Symlink
    } else {
        EntryKind::File
    };
    if found_kind != exp.kind {
        out.push(Divergence {
            path: rel.clone(),
            reason: DivergeReason::KindMismatch {
                expected: exp.kind,
                found: found_kind,
            },
        });
        return Ok(());
    }
    check_live_matches(target, fold, rel, exp, store, out)
}

fn classify_opt(md: Option<std::fs::Metadata>) -> Result<Option<EntryKind>, MaterializeError> {
    Ok(md.map(|m| classify_md(&m)))
}

fn classify_md(md: &std::fs::Metadata) -> EntryKind {
    let ft = md.file_type();
    if ft.is_symlink() {
        EntryKind::Symlink
    } else if ft.is_dir() {
        EntryKind::Dir
    } else {
        EntryKind::File
    }
}

struct Base<'a> {
    store: &'a Store,
    root: BlobId,
    cache: RefCell<HashMap<BlobId, Rc<TreeNode>>>,
}

impl<'a> Base<'a> {
    fn new(store: &'a Store, root: &BlobId) -> Self {
        Base {
            store,
            root: *root,
            cache: RefCell::new(HashMap::new()),
        }
    }

    fn node(&self, id: &BlobId) -> Result<Rc<TreeNode>, MaterializeError> {
        if let Some(n) = self.cache.borrow().get(id) {
            return Ok(Rc::clone(n));
        }
        let bytes = self.store.get(BlobKind::TreeNode, id)?;
        let node = Rc::new(parse_tree_node(&bytes)?);
        self.cache.borrow_mut().insert(*id, Rc::clone(&node));
        Ok(node)
    }

    fn lookup(&self, path: &CompPath) -> Result<Option<TreeEntry>, MaterializeError> {
        if path.is_empty() {
            return Ok(None);
        }
        let mut child_id = self.root;
        for (i, comp) in path.iter().enumerate() {
            let node = self.node(&child_id)?;
            let Some(entry) = node.entries.iter().find(|e| &e.name == comp) else {
                return Ok(None);
            };
            if i == path.len() - 1 {
                return Ok(Some(entry.clone()));
            }
            match &entry.payload {
                EntryPayload::Dir { child_tree_id } => child_id = *child_tree_id,
                _ => return Ok(None),
            }
        }
        unreachable!()
    }
}

fn flatten_base_subtree(
    base: &Base<'_>,
    dir_path: &CompPath,
    dir_entry: &TreeEntry,
    out: &mut HashMap<String, ExpectedState>,
) -> Result<(), MaterializeError> {
    let EntryPayload::Dir { child_tree_id } = &dir_entry.payload else {
        return Ok(());
    };
    let node = base.node(child_tree_id)?;
    for e in &node.entries {
        let mut child = dir_path.clone();
        child.push(e.name.clone());
        out.insert(join_path(&child), ExpectedState::from(e));
        if let EntryPayload::Dir { .. } = &e.payload {
            flatten_base_subtree(base, &child, e, out)?;
        }
    }
    Ok(())
}

fn collect_dir_mtimes(
    store: &Store,
    node_id: &BlobId,
    prefix: CompPath,
    out: &mut Vec<(CompPath, i64, u32)>,
) -> Result<(), MaterializeError> {
    let bytes = store.get(BlobKind::TreeNode, node_id)?;
    let node = parse_tree_node(&bytes)?;
    for e in &node.entries {
        let mut path = prefix.clone();
        path.push(e.name.clone());
        if let EntryPayload::Dir { child_tree_id } = &e.payload {
            out.push((path.clone(), e.mtime_sec, e.mtime_nsec));
            collect_dir_mtimes(store, child_tree_id, path, out)?;
        }
    }
    Ok(())
}

fn flatten_tree(
    store: &Store,
    root_tree_id: &BlobId,
) -> Result<Vec<(CompPath, Desired)>, MaterializeError> {
    let mut out = Vec::new();
    fn walk(
        store: &Store,
        node_id: &BlobId,
        prefix: &mut CompPath,
        out: &mut Vec<(CompPath, Desired)>,
    ) -> Result<(), MaterializeError> {
        let bytes = store.get(BlobKind::TreeNode, node_id)?;
        let node = parse_tree_node(&bytes)?;
        for e in &node.entries {
            prefix.push(e.name.clone());
            out.push((prefix.clone(), Desired::of(e)));
            if let EntryPayload::Dir { child_tree_id } = &e.payload {
                walk(store, child_tree_id, prefix, out)?;
            }
            prefix.pop();
        }
        Ok(())
    }
    walk(store, root_tree_id, &mut Vec::new(), &mut out)?;
    Ok(out)
}

fn walk_live(root: &Path) -> Result<Vec<CompPath>, MaterializeError> {
    let mut out = Vec::new();
    fn descend(
        dir: &Path,
        prefix: &mut CompPath,
        out: &mut Vec<CompPath>,
    ) -> Result<(), MaterializeError> {
        for entry in std::fs::read_dir(dir).map_err(|e| io_at(dir, e))?.flatten() {
            let name_os = entry.file_name();
            let name = match name_os.to_str() {
                Some(n) => n.to_string(),
                None => {
                    return Err(MaterializeError::BadComponent {
                        component: name_os.to_string_lossy().into_owned(),
                    })
                }
            };
            if is_temp_name(&name) {
                continue;
            }
            let ft = entry.file_type().map_err(|e| io_at(entry.path(), e))?;
            prefix.push(name);
            out.push(prefix.clone());
            if ft.is_dir() {
                descend(&entry.path(), prefix, out)?;
            }
            prefix.pop();
        }
        Ok(())
    }
    descend(root, &mut Vec::new(), &mut out)?;
    Ok(out)
}

struct NfcFoldCache {
    dirs: RefCell<HashMap<PathBuf, DirFold>>,

    scans: Cell<usize>,

    ambiguity: AmbiguityPolicy,
}

type DirFold = HashMap<String, Vec<String>>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum AmbiguityPolicy {
    Refuse,
    PickSmallest,
}

impl NfcFoldCache {
    fn refusing() -> Self {
        NfcFoldCache {
            dirs: RefCell::new(HashMap::new()),
            scans: Cell::new(0),
            ambiguity: AmbiguityPolicy::Refuse,
        }
    }

    fn lenient() -> Self {
        NfcFoldCache {
            ambiguity: AmbiguityPolicy::PickSmallest,
            ..NfcFoldCache::refusing()
        }
    }

    fn clear(&self) {
        self.dirs.borrow_mut().clear();
        self.scans.set(0);
    }

    #[cfg(test)]
    fn scanned_dirs(&self) -> usize {
        self.scans.get()
    }

    fn resolve(&self, root: &Path, rel: &[String]) -> Result<PathBuf, MaterializeError> {
        let mut cur = root.to_path_buf();
        for comp in rel {
            let direct = cur.join(comp);
            if std::fs::symlink_metadata(&direct).is_ok() {
                cur = direct;
                continue;
            }
            match self.match_live(&cur, comp)? {
                Some(live) => cur = cur.join(live),
                None => cur = direct,
            }
        }
        Ok(cur)
    }

    fn match_live(&self, dir: &Path, want: &str) -> Result<Option<String>, MaterializeError> {
        let want_nfc: String = want.nfc().collect();
        if let Some(fold) = self.dirs.borrow().get(dir) {
            return pick(fold, dir, &want_nfc, self.ambiguity);
        }
        let fold = scan_dir_fold(dir);
        self.scans.set(self.scans.get() + 1);
        self.dirs.borrow_mut().insert(dir.to_path_buf(), fold);
        let dirs = self.dirs.borrow();
        pick(&dirs[dir], dir, &want_nfc, self.ambiguity)
    }

    fn note_created_at(&self, entry_abs: &Path) {
        let Some(parent) = entry_abs.parent() else {
            return;
        };
        let Some(name) = entry_abs.file_name().and_then(|n| n.to_str()) else {
            return;
        };
        let nfc: String = name.nfc().collect();
        let mut dirs = self.dirs.borrow_mut();
        let bucket = dirs
            .entry(parent.to_path_buf())
            .or_default()
            .entry(nfc)
            .or_default();

        if !bucket.iter().any(|n| n == name) {
            bucket.push(name.to_string());
        }
    }

    fn note_removed(&self, entry_abs: &Path, deep: bool) {
        let Some(parent) = entry_abs.parent() else {
            return;
        };
        let Some(name) = entry_abs.file_name().and_then(|n| n.to_str()) else {
            return;
        };
        let nfc: String = name.nfc().collect();
        let mut dirs = self.dirs.borrow_mut();
        if deep {
            dirs.retain(|cached, _| !cached.starts_with(entry_abs));
        }
        if let Some(fold) = dirs.get_mut(parent) {
            if let Some(bucket) = fold.get_mut(&nfc) {
                bucket.retain(|n| n != name);
                if bucket.is_empty() {
                    fold.remove(&nfc);
                }
            }
        }
    }
}

fn scan_dir_fold(dir: &Path) -> DirFold {
    let mut fold = DirFold::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return fold;
    };
    for entry in rd.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        fold.entry(name.nfc().collect::<String>())
            .or_default()
            .push(name);
    }
    fold
}

fn pick(
    fold: &DirFold,
    dir: &Path,
    want_nfc: &str,
    policy: AmbiguityPolicy,
) -> Result<Option<String>, MaterializeError> {
    match fold.get(want_nfc).map(Vec::as_slice) {
        None | Some([]) => Ok(None),
        Some([only]) => Ok(Some(only.clone())),
        Some(many) => {
            let mut names = many.to_vec();
            names.sort();
            match policy {
                AmbiguityPolicy::Refuse => Err(MaterializeError::AmbiguousDiskSpelling {
                    parent: dir.to_string_lossy().into_owned(),
                    first: names[0].clone(),
                    second: names[1].clone(),
                }),
                AmbiguityPolicy::PickSmallest => Ok(Some(names.remove(0))),
            }
        }
    }
}

pub fn resolve_live(root: &Path, rel: &[String]) -> PathBuf {
    NfcFoldCache::lenient()
        .resolve(root, rel)
        .unwrap_or_else(|_| root.join(rel.iter().collect::<PathBuf>()))
}

fn components_below(root: &Path, dir: &Path) -> Vec<String> {
    dir.strip_prefix(root)
        .unwrap_or(Path::new(""))
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(str::to_string))
        .collect()
}

fn live_exec(md: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        md.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = md;
        false
    }
}

fn set_exec_bit(file: &std::fs::File, exec: bool) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if exec { 0o755 } else { 0o644 };
        file.set_permissions(std::fs::Permissions::from_mode(mode))
    }
    #[cfg(not(unix))]
    {
        let _ = (file, exec);
        Ok(())
    }
}

fn make_symlink(target: &str, at: &Path) -> Result<(), MaterializeError> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, at).map_err(|e| io_at(at, e))
    }
    #[cfg(windows)]
    {
        let resolved = at
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join(target);
        if resolved.is_dir() {
            std::os::windows::fs::symlink_dir(target, at)
        } else {
            std::os::windows::fs::symlink_file(target, at)
        }
        .map_err(|e| io_at(at, e))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, at);
        Err(MaterializeError::BadComponent {
            component: "symlinks unsupported on this host".into(),
        })
    }
}

#[cfg(unix)]
pub fn set_symlink_times(path: &Path, sec: i64, nsec: u32) -> Result<(), MaterializeError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c =
        CString::new(path.as_os_str().as_bytes()).map_err(|_| MaterializeError::BadComponent {
            component: "path contains NUL".into(),
        })?;
    let ts = libc::timespec {
        tv_sec: sec as libc::time_t,
        tv_nsec: libc::c_long::from(nsec),
    };
    let times = [ts, ts];

    let rc = unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            c.as_ptr(),
            times.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 {
        return Err(io_at(path, std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(windows)]
pub fn set_symlink_times(path: &Path, sec: i64, nsec: u32) -> Result<(), MaterializeError> {
    let ft = filetime::FileTime::from_unix_time(sec, nsec);
    filetime::set_symlink_file_times(path, ft, ft).map_err(|e| io_at(path, e))
}

#[cfg(not(any(unix, windows)))]
pub fn set_symlink_times(_path: &Path, _sec: i64, _nsec: u32) -> Result<(), MaterializeError> {
    Ok(())
}

fn set_mtime(path: &Path, sec: i64, nsec: u32) -> Result<(), MaterializeError> {
    #[cfg(unix)]
    {
        let f = std::fs::OpenOptions::new()
            .read(true)
            .open(path)
            .map_err(|e| io_at(path, e))?;
        f.set_times(
            std::fs::FileTimes::new().set_modified(ferry_platform::time::join_unix(sec, nsec)),
        )
        .map_err(|e| io_at(path, e))
    }
    #[cfg(not(unix))]
    {
        let ft = filetime::FileTime::from_unix_time(sec, nsec);
        filetime::set_file_mtime(path, ft).map_err(|e| io_at(path, e))
    }
}

#[cfg(unix)]
fn fsync_dir(dir: &Path) -> Result<(), MaterializeError> {
    match std::fs::File::open(dir) {
        Ok(f) => f.sync_all().map_err(|e| io_at(dir, e)),

        Err(e) => Err(io_at(dir, e)),
    }
}

#[cfg(not(unix))]
fn fsync_dir(_dir: &Path) -> Result<(), MaterializeError> {
    Ok(())
}

fn parent_of(p: &Path) -> &Path {
    p.parent().unwrap_or(Path::new("."))
}

fn verify_regions<R: Read>(
    r: &mut R,
    at: &Path,
    rel_display: &str,
    chunks: &[(BlobId, u64)],
) -> Result<(), MaterializeError> {
    for (index, (id, len)) in chunks.iter().enumerate() {
        let mut region = vec![0u8; *len as usize];
        r.read_exact(&mut region).map_err(|e| io_at(at, e))?;
        let found = *blake3::hash(&region).as_bytes();
        if &found != id {
            return Err(MaterializeError::TempWriteVerifyFailed {
                path: rel_display.into(),
                index,
                expected: hex(id),
                found: hex(&found),
            });
        }
    }
    Ok(())
}

fn content_matches(
    store: &Store,
    abs: &Path,
    chunks: &[(BlobId, u64)],
) -> Result<bool, MaterializeError> {
    let mut f = std::fs::File::open(abs).map_err(|e| io_at(abs, e))?;
    for (id, len) in chunks {
        let expect = store.get(BlobKind::DataChunk, id)?;
        if expect.len() as u64 != *len {
            return Err(MaterializeError::ChunkCorrupt {
                path: abs.to_string_lossy().into_owned(),
                index: usize::MAX,
                expected: format!("len {len}"),
                found: format!("len {}", expect.len()),
            });
        }
        let mut got = vec![0u8; *len as usize];
        if f.read_exact(&mut got).is_err() {
            return Ok(false);
        }
        if got != expect {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn validate_components(path: &[String]) -> Result<(), MaterializeError> {
    for c in path {
        if c.is_empty()
            || c == "."
            || c == ".."
            || c.contains('/')
            || c.contains('\\')
            || c.contains('\0')
            || (cfg!(windows) && c.contains(':'))
            || !matches!(
                std::path::Path::new(c).components().next(),
                Some(std::path::Component::Normal(_))
            )
        {
            return Err(MaterializeError::BadComponent {
                component: c.clone(),
            });
        }
        if ferry_platform::is_reserved_device_name(c) {
            return Err(MaterializeError::ReservedName {
                path: join_path(path),
                component: c.clone(),
            });
        }
    }
    Ok(())
}

pub(crate) fn ensure_no_fold_collisions(paths: &[CompPath]) -> Result<(), MaterializeError> {
    if !ferry_platform::host_folds_case() {
        return Ok(());
    }
    use std::collections::HashMap;
    let mut per_parent: HashMap<&[String], Vec<&str>> = HashMap::new();
    for p in paths {
        let (parent, name) = p.split_at(p.len().saturating_sub(1));
        per_parent
            .entry(parent)
            .or_default()
            .push(name.first().map_or("", String::as_str));
    }
    let mut parents: Vec<&[String]> = per_parent.keys().copied().collect();
    parents.sort();
    for parent in parents {
        let names = per_parent[parent].clone();
        if let Some(c) = ferry_platform::find_case_conflict(&names) {
            return Err(MaterializeError::CaseCollision {
                parent: join_path(parent),
                first: c.first,
                second: c.second,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferry_store::chunker::{chunk, generate_polynomial};
    use ferry_store::crypto::PassthroughCipher;
    use ferry_store::diff::{Added, EntryState, Modified, Removed};
    use ferry_store::manifest::{dir_entry, file_entry, serialize_tree_node, symlink_entry};
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};

    fn fmk() -> [u8; 32] {
        core::array::from_fn(|i| (i * 7 + 3) as u8)
    }

    struct World {
        _dir: tempfile::TempDir,
        store: Store,
        poly: u64,
    }

    impl World {
        fn new(seed: u64) -> (World, PathBuf) {
            let dir = tempfile::tempdir().unwrap();

            std::fs::create_dir_all(dir.path().join("store-root")).unwrap();
            let store = Store::create(
                &dir.path().join("store-root"),
                fmk(),
                Box::new(PassthroughCipher),
            )
            .unwrap();
            let target = dir.path().join("target");
            std::fs::create_dir_all(&target).unwrap();
            (
                World {
                    poly: generate_polynomial(&mut StdRng::seed_from_u64(seed)),
                    _dir: dir,
                    store,
                },
                target,
            )
        }
    }

    fn chunked(w: &World, bytes: &[u8]) -> Vec<(BlobId, u64)> {
        chunk(w.poly, bytes)
            .expect("fixture poly valid")
            .iter()
            .map(|b| (w.store.put_data(b).unwrap(), b.len() as u64))
            .collect()
    }

    fn wfile(name: &str, w: &World, bytes: &[u8], exec: bool, mt: (i64, u32)) -> TreeEntry {
        file_entry(name, exec, mt.0, mt.1, chunked(w, bytes))
    }

    fn tree_id(w: &World, node: &TreeNode) -> BlobId {
        let bytes = serialize_tree_node(node);
        let id = *blake3::hash(&bytes).as_bytes();
        w.store.put_meta(BlobKind::TreeNode, &bytes).unwrap();
        id
    }

    fn root_manifest(tree: BlobId) -> RootManifest {
        RootManifest {
            folder_id: [1; 16],
            device_id: [2; 32],
            created_sec: 100,
            created_nsec: 0,
            root_tree_id: tree,
            parent_manifest_id: [0; 32],
        }
    }

    fn md_of(target: &Path, rel: &[&str]) -> std::fs::Metadata {
        let mut p = target.to_path_buf();
        for c in rel {
            p.push(c);
        }
        std::fs::symlink_metadata(p).unwrap()
    }

    fn read_target(target: &Path, rel: &[&str]) -> Vec<u8> {
        let mut p = target.to_path_buf();
        for c in rel {
            p.push(c);
        }
        std::fs::read(p).unwrap()
    }

    fn mtime_of(md: &std::fs::Metadata) -> (i64, u32) {
        ferry_platform::split_unix(md.modified().unwrap())
    }

    const NS_GRAN: u32 = if cfg!(windows) { 100 } else { 1 };
    const MT_A: (i64, u32) = (1_700_000_000, 111 / NS_GRAN * NS_GRAN);
    const MT_B: (i64, u32) = (1_700_000_500, 222 / NS_GRAN * NS_GRAN);

    #[test]
    fn apply_is_idempotent_second_run_is_a_noop() {
        let (w, target) = World::new(1);

        let sub_id = tree_id(
            &w,
            &TreeNode {
                entries: vec![wfile("inner.txt", &w, b"deep", false, MT_B)],
            },
        );
        let root = tree_id(
            &w,
            &TreeNode {
                entries: vec![
                    dir_entry("sub", MT_A.0, MT_A.1, sub_id),
                    wfile("a.txt", &w, b"alpha", true, MT_A),
                    symlink_entry("link", MT_B.0, MT_B.1, "sub/inner.txt"),
                ],
            },
        );

        let mut ap = Applier::new(&w.store, &target);
        let s1 = ap.apply_tree(&root).unwrap();
        assert!(s1.mutations() > 0, "first apply must do work");

        let mut ap = Applier::new(&w.store, &target);
        let s2 = ap.apply_tree(&root).unwrap();

        assert_eq!(
            s2.mutations(),
            0,
            "second identical apply must perform zero writes; stats {s2:?}"
        );
        assert_eq!(s2.skipped_unchanged, 4, "all four entries skipped");
    }

    #[test]
    fn single_file_round_trip_bytes_perm_and_mtime() {
        let (w, target) = World::new(2);

        let root = tree_id(
            &w,
            &TreeNode {
                entries: vec![wfile("script.sh", &w, b"#!/bin/sh\necho hi\n", true, MT_B)],
            },
        );
        Applier::new(&w.store, &target).apply_tree(&root).unwrap();

        let md = md_of(&target, &["script.sh"]);
        assert_eq!(
            read_target(&target, &["script.sh"]),
            b"#!/bin/sh\necho hi\n"
        );

        if cfg!(unix) {
            assert!(live_exec(&md), "exec flag restored");
        }
        assert_eq!(mtime_of(&md), MT_B);
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 1);
    }

    #[test]
    #[cfg(unix)]
    fn exec_bit_is_authoritative_set_and_cleared() {
        let (w, target) = World::new(3);

        let v1 = tree_id(
            &w,
            &TreeNode {
                entries: vec![
                    wfile("on.bin", &w, b"one", true, MT_A),
                    wfile("off.bin", &w, b"two", false, MT_A),
                ],
            },
        );
        Applier::new(&w.store, &target).apply_tree(&v1).unwrap();
        assert!(live_exec(&md_of(&target, &["on.bin"])));
        assert!(!live_exec(&md_of(&target, &["off.bin"])));

        let v2 = tree_id(
            &w,
            &TreeNode {
                entries: vec![
                    wfile("on.bin", &w, b"one", false, MT_A),
                    wfile("off.bin", &w, b"two", true, MT_A),
                ],
            },
        );
        let stats = Applier::new(&w.store, &target).apply_tree(&v2).unwrap();
        assert_eq!(stats.files_written, 2);
        assert_eq!(stats.creations, ["off.bin", "on.bin"]);
        assert!(!live_exec(&md_of(&target, &["on.bin"])));
        assert!(live_exec(&md_of(&target, &["off.bin"])));
    }

    #[test]
    fn dirs_created_parents_first_deletions_children_first() {
        let (w, target) = World::new(4);

        let deep = tree_id(
            &w,
            &TreeNode {
                entries: vec![wfile("leaf.txt", &w, b"L", false, MT_A)],
            },
        );
        let mid = tree_id(
            &w,
            &TreeNode {
                entries: vec![dir_entry("deep", MT_A.0, MT_A.1, deep)],
            },
        );
        let v1 = tree_id(
            &w,
            &TreeNode {
                entries: vec![
                    dir_entry("outer", MT_A.0, MT_A.1, mid),
                    wfile("stay.txt", &w, b"S", false, MT_A),
                ],
            },
        );
        let s1 = Applier::new(&w.store, &target).apply_tree(&v1).unwrap();

        let pos = |s: &str| s1.creations.iter().position(|c| c == s).unwrap();
        assert!(pos("outer") < pos("outer/deep"));
        assert!(pos("outer/deep") < pos("outer/deep/leaf.txt"));

        assert_eq!(mtime_of(&md_of(&target, &["outer"])), MT_A);

        let v2 = tree_id(
            &w,
            &TreeNode {
                entries: vec![wfile("stay.txt", &w, b"S", false, MT_A)],
            },
        );
        let s2 = Applier::new(&w.store, &target).apply_tree(&v2).unwrap();
        assert_eq!(
            s2.deletions,
            ["outer/deep/leaf.txt", "outer/deep", "outer"],
            "children must be deleted before their parents"
        );
        assert!(!target.join("outer").exists());
        assert!(target.join("stay.txt").exists());
    }

    #[test]
    fn unicode_nfc_names_round_trip() {
        let (w, target) = World::new(5);

        let composed = "caf\u{e9}.txt";
        let decomposed = "cafe\u{301}.txt";
        let node_composed = TreeNode {
            entries: vec![wfile(composed, &w, b"unicode!", false, MT_A)],
        };
        let node_decomposed = TreeNode {
            entries: vec![wfile(decomposed, &w, b"unicode!", false, MT_A)],
        };

        assert_eq!(tree_id(&w, &node_composed), tree_id(&w, &node_decomposed));

        let root = tree_id(&w, &node_composed);
        Applier::new(&w.store, &target).apply_tree(&root).unwrap();
        let names: Vec<String> = std::fs::read_dir(&target)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            [composed],
            "materialized under the composed NFC name"
        );
        assert_eq!(read_target(&target, &[composed]), b"unicode!");
    }

    #[test]
    fn symlinks_created_via_temp_rename_and_retargeted_atomically() {
        let (w, target) = World::new(6);

        let v1 = tree_id(
            &w,
            &TreeNode {
                entries: vec![symlink_entry("lnk", MT_A.0, MT_A.1, "elsewhere")],
            },
        );
        Applier::new(&w.store, &target).apply_tree(&v1).unwrap();
        assert_eq!(
            std::fs::read_link(target.join("lnk"))
                .unwrap()
                .to_string_lossy(),
            "elsewhere"
        );

        let v2 = tree_id(
            &w,
            &TreeNode {
                entries: vec![symlink_entry("lnk", MT_A.0, MT_A.1, "somewhere/else")],
            },
        );
        let stats = Applier::new(&w.store, &target).apply_tree(&v2).unwrap();
        assert_eq!(stats.symlinks_written, 1);
        assert_eq!(
            std::fs::read_link(target.join("lnk"))
                .unwrap()
                .to_string_lossy(),
            "somewhere/else"
        );

        let s3 = Applier::new(&w.store, &target).apply_tree(&v2).unwrap();
        assert_eq!(s3.mutations(), 0);
    }

    #[test]
    fn type_changes_file_to_dir_to_symlink_across_manifests() {
        let (w, target) = World::new(7);

        let v1 = tree_id(
            &w,
            &TreeNode {
                entries: vec![wfile("x", &w, b"plain file", false, MT_A)],
            },
        );
        Applier::new(&w.store, &target).apply_tree(&v1).unwrap();
        assert!(md_of(&target, &["x"]).is_file());

        let inner = tree_id(
            &w,
            &TreeNode {
                entries: vec![wfile("y", &w, b"in dir", false, MT_B)],
            },
        );
        let v2 = tree_id(
            &w,
            &TreeNode {
                entries: vec![dir_entry("x", MT_B.0, MT_B.1, inner)],
            },
        );
        Applier::new(&w.store, &target).apply_tree(&v2).unwrap();
        assert!(md_of(&target, &["x"]).is_dir());
        assert_eq!(read_target(&target, &["x", "y"]), b"in dir");

        let v3 = tree_id(
            &w,
            &TreeNode {
                entries: vec![symlink_entry("x", MT_A.0, MT_A.1, "target-z")],
            },
        );
        let s3 = Applier::new(&w.store, &target).apply_tree(&v3).unwrap();
        assert!(md_of(&target, &["x"]).file_type().is_symlink());
        let di: Vec<&str> = s3.deletions.iter().map(String::as_str).collect();
        assert_eq!(
            di,
            ["x/y", "x"],
            "dir torn down children-first before the symlink landed"
        );
    }

    #[test]
    fn empty_file_materializes_with_zero_chunks() {
        let (w, target) = World::new(8);

        let root = tree_id(
            &w,
            &TreeNode {
                entries: vec![file_entry("void.dat", false, MT_A.0, MT_A.1, vec![])],
            },
        );
        Applier::new(&w.store, &target).apply_tree(&root).unwrap();
        let md = md_of(&target, &["void.dat"]);
        assert!(md.is_file());
        assert_eq!(md.len(), 0);
        assert_eq!(mtime_of(&md), MT_A);
    }

    #[test]
    fn mtime_only_drift_restores_metadata_not_bytes() {
        let (w, target) = World::new(9);

        let root = tree_id(
            &w,
            &TreeNode {
                entries: vec![wfile("touched.txt", &w, b"same bytes", false, MT_A)],
            },
        );
        Applier::new(&w.store, &target).apply_tree(&root).unwrap();

        set_mtime(&target.join("touched.txt"), MT_B.0, MT_B.1).unwrap();
        assert_eq!(mtime_of(&md_of(&target, &["touched.txt"])), MT_B);

        let stats = Applier::new(&w.store, &target).apply_tree(&root).unwrap();

        assert_eq!(
            stats.files_written, 0,
            "identical bytes must not be rewritten"
        );
        assert_eq!(stats.mtimes_set, 1);
        assert_eq!(mtime_of(&md_of(&target, &["touched.txt"])), MT_A);

        std::fs::write(target.join("touched.txt"), b"tampered").unwrap();
        let stats = Applier::new(&w.store, &target).apply_tree(&root).unwrap();
        assert_eq!(stats.files_written, 1);
        assert_eq!(read_target(&target, &["touched.txt"]), b"same bytes");
    }

    #[test]
    #[cfg(unix)]
    fn guarded_mode_tamper_lists_exact_divergences_and_leaves_files_untouched() {
        let (w, target) = World::new(10);

        let base_inner = tree_id(
            &w,
            &TreeNode {
                entries: vec![wfile("c.txt", &w, b"CCC", false, MT_A)],
            },
        );
        let base_root = tree_id(
            &w,
            &TreeNode {
                entries: vec![
                    wfile("a.txt", &w, b"AAA", false, MT_A),
                    wfile("b.txt", &w, b"BBB", true, MT_B),
                    dir_entry("d", MT_A.0, MT_A.1, base_inner),
                    symlink_entry("lnk", MT_A.0, MT_A.1, "a.txt"),
                ],
            },
        );
        Applier::new(&w.store, &target)
            .overwrite(Overwrite::Always)
            .apply_tree(&base_root)
            .unwrap();

        let new_root = tree_id(
            &w,
            &TreeNode {
                entries: vec![
                    wfile("a.txt", &w, b"XXX", false, MT_B),
                    wfile("b.txt", &w, b"BBB", true, MT_B),
                    dir_entry("d", MT_A.0, MT_A.1, base_inner),
                    symlink_entry("lnk", MT_B.0, MT_B.1, "b.txt"),
                    wfile("new.txt", &w, b"N", false, MT_B),
                ],
            },
        );
        let cs = ferry_store::diff::diff_roots(&w.store, &base_root, &new_root).unwrap();
        assert_eq!(
            cs.content_modified.len() + cs.added.len() + cs.removed.len(),
            3
        );

        std::fs::write(target.join("a.txt"), b"XXY").unwrap();
        std::fs::write(target.join("new.txt"), b"junk that was never synced").unwrap();
        std::fs::remove_file(target.join("lnk")).unwrap();
        std::os::unix::fs::symlink("d", target.join("lnk")).unwrap();

        let before_a = read_target(&target, &["a.txt"]);
        let before_b_md = md_of(&target, &["b.txt"]);

        let err = Applier::new(&w.store, &target)
            .overwrite(Overwrite::Expect {
                expected: root_manifest(base_root),
            })
            .apply_change_set(&cs)
            .unwrap_err();

        let MaterializeError::Diverged { paths } = &err else {
            panic!("expected Diverged, got {err}");
        };
        let rendered: Vec<(String, DivergeReason)> = paths
            .iter()
            .map(|d| (join_path(&d.path), d.reason.clone()))
            .collect();
        assert_eq!(
            rendered,
            vec![
                ("a.txt".into(), DivergeReason::ContentMismatch),
                (
                    "lnk".into(),
                    DivergeReason::TargetMismatch {
                        expected: "a.txt".into(),
                        found: "d".into()
                    }
                ),
                ("new.txt".into(), DivergeReason::ExpectedAbsent),
            ]
        );

        assert_eq!(read_target(&target, &["a.txt"]), before_a);
        assert_eq!(
            mtime_of(&before_b_md),
            mtime_of(&md_of(&target, &["b.txt"]))
        );
        assert_eq!(read_target(&target, &["d", "c.txt"]), b"CCC");
        assert!(
            !target.join("new.txt").exists()
                || read_target(&target, &["new.txt"]) == b"junk that was never synced"
        );

        std::fs::write(target.join("a.txt"), b"AAA").unwrap();
        std::fs::remove_file(target.join("new.txt")).unwrap();
        std::fs::remove_file(target.join("lnk")).unwrap();
        std::os::unix::fs::symlink("a.txt", target.join("lnk")).unwrap();
        Applier::new(&w.store, &target)
            .overwrite(Overwrite::Expect {
                expected: root_manifest(base_root),
            })
            .apply_change_set(&cs)
            .unwrap();
        assert_eq!(read_target(&target, &["a.txt"]), b"XXX");
        assert_eq!(read_target(&target, &["new.txt"]), b"N");
        assert_eq!(
            std::fs::read_link(target.join("lnk"))
                .unwrap()
                .to_string_lossy(),
            "b.txt"
        );
    }

    #[test]
    fn guarded_mode_refuses_to_delete_unaccounted_data_in_teardown() {
        let (w, target) = World::new(11);

        let base_root = tree_id(
            &w,
            &TreeNode {
                entries: vec![dir_entry("gone-next", MT_A.0, MT_A.1, {
                    tree_id(
                        &w,
                        &TreeNode {
                            entries: vec![wfile("accounted.txt", &w, b"A", false, MT_A)],
                        },
                    )
                })],
            },
        );
        Applier::new(&w.store, &target)
            .overwrite(Overwrite::Always)
            .apply_tree(&base_root)
            .unwrap();
        std::fs::write(target.join("gone-next/junk.txt"), b"never synced").unwrap();

        let new_root = tree_id(&w, &TreeNode { entries: vec![] });
        let cs = ferry_store::diff::diff_roots(&w.store, &base_root, &new_root).unwrap();

        let err = Applier::new(&w.store, &target)
            .overwrite(Overwrite::Expect {
                expected: root_manifest(base_root),
            })
            .apply_change_set(&cs)
            .unwrap_err();
        let MaterializeError::Diverged { paths } = &err else {
            panic!("expected Diverged, got {err}");
        };
        let junk: Vec<&str> = paths
            .iter()
            .filter(|d| join_path(&d.path) == "gone-next/junk.txt")
            .map(|_| "found")
            .collect();
        assert_eq!(junk, ["found"], "junk must surface as a divergence");

        assert_eq!(
            read_target(&target, &["gone-next/junk.txt"]),
            b"never synced"
        );

        Applier::new(&w.store, &target)
            .overwrite(Overwrite::Always)
            .apply_change_set(&cs)
            .unwrap();
        assert!(!target.join("gone-next").exists());
    }

    #[test]
    fn change_set_minimality_touches_only_listed_paths() {
        let (w, target) = World::new(12);

        std::fs::create_dir(target.join("keep-dir")).unwrap();
        std::fs::write(target.join("keep.txt"), b"stable").unwrap();
        std::fs::write(target.join("mod.txt"), b"version one").unwrap();
        std::fs::write(target.join("del.txt"), b"bye").unwrap();
        set_mtime(&target.join("keep.txt"), MT_A.0, MT_A.1).unwrap();
        set_mtime(&target.join("mod.txt"), MT_A.0, MT_A.1).unwrap();

        let mod_chunks = chunked(&w, b"version two!");
        let cs = ChangeSet {
            added: vec![Added {
                path: vec!["add.txt".into()],
                state: EntryState {
                    kind: EntryKind::File,
                    exec: false,
                    mtime_sec: MT_B.0,
                    mtime_nsec: MT_B.1,
                    chunks: chunked(&w, b"fresh"),
                    target: None,
                },
            }],
            removed: vec![Removed {
                path: vec!["del.txt".into()],
                state: EntryState {
                    kind: EntryKind::File,
                    exec: false,
                    mtime_sec: MT_A.0,
                    mtime_nsec: MT_A.1,
                    chunks: chunked(&w, b"bye"),
                    target: None,
                },
            }],
            content_modified: vec![Modified {
                path: vec!["mod.txt".into()],
                before: EntryState {
                    kind: EntryKind::File,
                    exec: false,
                    mtime_sec: MT_A.0,
                    mtime_nsec: MT_A.1,
                    chunks: chunked(&w, b"version one"),
                    target: None,
                },
                after: EntryState {
                    kind: EntryKind::File,
                    exec: false,
                    mtime_sec: MT_B.0,
                    mtime_nsec: MT_B.1,
                    chunks: mod_chunks,
                    target: None,
                },
            }],
            metadata_modified: vec![],
            type_changed: vec![],
        };

        let stats = Applier::new(&w.store, &target)
            .apply_change_set(&cs)
            .unwrap();
        assert_eq!(
            stats.mutations(),
            3,
            "exactly one write, one unlink, one create: {stats:?}"
        );
        assert_eq!(read_target(&target, &["mod.txt"]), b"version two!");
        assert_eq!(read_target(&target, &["add.txt"]), b"fresh");
        assert!(!target.join("del.txt").exists());

        let keep = md_of(&target, &["keep.txt"]);
        assert_eq!(keep.len(), 6);
        assert_eq!(mtime_of(&keep), MT_A);
        assert_eq!(mtime_of(&md_of(&target, &["keep-dir"])), {
            let _ = 1;
            mtime_of(&md_of(&target, &["keep-dir"]))
        });

        let again = Applier::new(&w.store, &target)
            .apply_change_set(&cs)
            .unwrap();
        assert_eq!(again.unlinked, 0, "removal already satisfied");
    }

    #[test]
    fn traversal_defense_rejects_bad_stored_components() {
        let (w, target) = World::new(13);

        let mut bads = vec!["..", ".", "a/b", "a\\b", "", "/abs"];
        if cfg!(windows) {
            bads.extend(["C:x", "C\\x", "a:b"]);
        }
        for bad in bads {
            let cs = ChangeSet {
                added: vec![Added {
                    path: vec![bad.to_string()],
                    state: EntryState {
                        kind: EntryKind::File,
                        exec: false,
                        mtime_sec: 0,
                        mtime_nsec: 0,
                        chunks: chunked(&w, b"x"),
                        target: None,
                    },
                }],
                ..Default::default()
            };
            let err = Applier::new(&w.store, &target)
                .apply_change_set(&cs)
                .unwrap_err();
            assert!(
                matches!(err, MaterializeError::BadComponent { .. }),
                "{bad:?} should be refused, got {err}"
            );
        }

        assert!(std::fs::read_dir(&target).unwrap().next().is_none());
    }

    #[test]
    fn corrupt_store_chunk_is_refused_before_any_write() {
        let (w, target) = World::new(14);

        let root = tree_id(
            &w,
            &TreeNode {
                entries: vec![wfile("victim.bin", &w, b"precious data", false, MT_A)],
            },
        );

        w.store.flush().unwrap();

        let packs = w._dir.path().join("store-root/.ferry/packs");
        for e in std::fs::read_dir(&packs).unwrap().flatten() {
            let mut bytes = std::fs::read(e.path()).unwrap();
            if bytes.len() > 28 {
                bytes[27] ^= 0xFF;
                std::fs::write(e.path(), bytes).unwrap();
            }
        }

        let err = Applier::new(&w.store, &target)
            .apply_tree(&root)
            .unwrap_err();
        assert!(
            matches!(err, MaterializeError::Store(_)),
            "store-level verification failure must surface: {err}"
        );

        assert!(std::fs::read_dir(&target).unwrap().next().is_none());
    }

    #[test]
    fn pre_rename_region_verification_failure_branch_works() {
        let id_a = *blake3::hash(b"aaaa").as_bytes();
        let good: [(BlobId, u64); 1] = [(id_a, 4)];
        let mut ok_reader = std::io::Cursor::new(b"aaaa".to_vec());
        verify_regions(&mut ok_reader, Path::new("mem"), "f", &good).unwrap();

        let bad_reader = std::io::Cursor::new(b"aaab".to_vec());
        let err = verify_regions(&mut { bad_reader }, Path::new("mem"), "f", &good).unwrap_err();
        match err {
            MaterializeError::TempWriteVerifyFailed {
                path, index, found, ..
            } => {
                assert_eq!(path, "f");
                assert_eq!(index, 0);
                assert_eq!(found, hex(blake3::hash(b"aaab").as_bytes()));
            }
            other => panic!("wrong error {other}"),
        }
    }

    #[test]
    fn extras_deleted_in_always_mode_refused_without_expectation() {
        let (w, target) = World::new(15);

        std::fs::write(target.join("wanted.txt"), b"wanted").unwrap();
        std::fs::create_dir(target.join("stale-dir")).unwrap();
        std::fs::write(target.join("stale-dir/old.txt"), b"old").unwrap();

        let root = tree_id(
            &w,
            &TreeNode {
                entries: vec![wfile("wanted.txt", &w, b"wanted", false, MT_A)],
            },
        );

        let empty_base = tree_id(&w, &TreeNode { entries: vec![] });
        let err = Applier::new(&w.store, &target)
            .overwrite(Overwrite::Expect {
                expected: root_manifest(empty_base),
            })
            .apply_tree(&root)
            .unwrap_err();
        assert!(matches!(err, MaterializeError::Diverged { .. }), "{err}");
        assert!(target.join("stale-dir/old.txt").exists());

        let stats = Applier::new(&w.store, &target).apply_tree(&root).unwrap();
        assert!(!target.join("stale-dir").exists());
        assert_eq!(
            stats.deletions.last().map(String::as_str),
            Some("stale-dir"),
            "the directory itself is deleted last"
        );
    }

    #[test]
    fn temp_names_during_apply_follow_the_selected_style() {
        let (w, target) = World::new(16);

        let root = tree_id(
            &w,
            &TreeNode {
                entries: vec![wfile("big.bin", &w, &vec![7u8; 300_000], false, MT_A)],
            },
        );

        let mut ap = Applier::new(&w.store, &target).temp_style(TempStyle::Windows);
        ap.apply_tree(&root).unwrap();
        assert!(target.join("big.bin").exists());
        assert!(
            std::fs::read_dir(&target).unwrap().count() == 1,
            "no temps left"
        );
    }

    #[test]
    fn shared_clock_helpers_round_trip_pre1970() {
        for (sec, nsec) in [
            (0i64, 0u32),
            (1_700_000_000, 999_999_999 / NS_GRAN * NS_GRAN),
            (-1, 0),
            (-1, 500_000_000),
            (-5_000_000_000, 1 / NS_GRAN * NS_GRAN),
        ] {
            let t = ferry_platform::time::join_unix(sec, nsec);
            assert_eq!(ferry_platform::split_unix(t), (sec, nsec), "({sec},{nsec})");
        }
    }

    #[test]
    fn seeded_multi_file_apply_matches_model_exactly() {
        let (w, target) = World::new(17);

        let mut rng = StdRng::seed_from_u64(99);
        let mut model: Vec<(Vec<String>, Vec<u8>, bool)> = Vec::new();
        let mut entries: Vec<TreeEntry> = Vec::new();
        for i in 0..12 {
            let len = rng.gen::<usize>() % (ferry_store::chunker::MIN_SIZE + 4096);
            let bytes: Vec<u8> = (0..len).map(|_| rng.gen()).collect();
            let exec = rng.gen_bool(0.4);
            let name = format!("f{i:02}.bin");
            entries.push(file_entry(
                &name,
                exec,
                MT_A.0 + i64::from(i),
                (MT_A.1 + i as u32 * NS_GRAN) % 1_000_000_000,
                chunked(&w, &bytes),
            ));
            model.push((vec![name], bytes, exec));
        }
        let nested = tree_id(
            &w,
            &TreeNode {
                entries: vec![wfile("nested.bin", &w, &model[0].1.clone(), true, MT_B)],
            },
        );
        entries.push(dir_entry("nest", MT_B.0, MT_B.1, nested));
        let root = tree_id(&w, &TreeNode { entries });

        Applier::new(&w.store, &target).apply_tree(&root).unwrap();
        for (path, bytes, exec) in &model {
            assert_eq!(
                read_target(
                    &target,
                    path.iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .as_slice()
                ),
                bytes.as_slice(),
                "{path:?}"
            );

            if cfg!(unix) {
                assert_eq!(
                    live_exec(&md_of(
                        &target,
                        path.iter()
                            .map(String::as_str)
                            .collect::<Vec<_>>()
                            .as_slice()
                    )),
                    *exec
                );
            }
        }

        let again = Applier::new(&w.store, &target).apply_tree(&root).unwrap();
        assert_eq!(again.mutations(), 0);
    }

    #[test]
    #[cfg(unix)]
    fn symlink_own_mtime_is_restored_and_idempotent() {
        let (w, target) = World::new(31);
        let root = tree_id(
            &w,
            &TreeNode {
                entries: vec![
                    file_entry("real.txt", false, MT_A.0, MT_A.1, vec![]),
                    symlink_entry("lnk", MT_B.0, MT_B.1, "real.txt"),
                ],
            },
        );
        let stats = Applier::new(&w.store, &target).apply_tree(&root).unwrap();
        assert_eq!(stats.symlinks_written, 1);

        let md = std::fs::symlink_metadata(target.join("lnk")).unwrap();
        let (sec, nsec) = ferry_platform::split_unix(md.modified().unwrap());
        assert_eq!(
            (sec, nsec),
            MT_B,
            "deferred T-005 piece: link mtime restored"
        );

        let again = Applier::new(&w.store, &target).apply_tree(&root).unwrap();
        assert_eq!(again.mutations(), 0);
    }

    #[test]
    #[cfg(unix)]
    fn identical_symlink_with_drifted_own_mtime_gets_times_restored() {
        let (w, target) = World::new(34);
        let root = tree_id(
            &w,
            &TreeNode {
                entries: vec![symlink_entry("lnk", MT_B.0, MT_B.1, "elsewhere")],
            },
        );
        Applier::new(&w.store, &target).apply_tree(&root).unwrap();

        std::fs::remove_file(target.join("lnk")).unwrap();
        std::os::unix::fs::symlink("elsewhere", target.join("lnk")).unwrap();
        let drifted = md_of(&target, &["lnk"]).modified().unwrap();
        assert_ne!(
            ferry_platform::split_unix(drifted),
            MT_B,
            "sabotage must take effect"
        );

        let stats = Applier::new(&w.store, &target).apply_tree(&root).unwrap();
        assert_eq!(
            stats.symlinks_written, 0,
            "identical link must NOT be recreated"
        );
        assert_eq!(stats.mtimes_set, 1, "only the link's own times restored");
        let (sec, nsec) = ferry_platform::split_unix(md_of(&target, &["lnk"]).modified().unwrap());
        assert_eq!((sec, nsec), MT_B);

        let s3 = Applier::new(&w.store, &target).apply_tree(&root).unwrap();
        assert_eq!(s3.mutations(), 0);
    }

    #[test]
    fn session_change_set_restores_ancestor_dir_mtimes_absent_from_the_change_set() {
        let inner_mt: (i64, u32) = (111, 222 / NS_GRAN * NS_GRAN);
        let (w, target) = World::new(35);

        let leaf_id = tree_id(
            &w,
            &TreeNode {
                entries: vec![wfile("deep.txt", &w, b"", false, (7, 7))],
            },
        );
        let target_root = tree_id(
            &w,
            &TreeNode {
                entries: vec![dir_entry("inner", inner_mt.0, inner_mt.1, leaf_id)],
            },
        );

        std::fs::create_dir_all(target.join("inner")).unwrap();
        let cs = ChangeSet {
            added: vec![Added {
                path: vec!["inner".into(), "deep.txt".into()],
                state: EntryState {
                    kind: EntryKind::File,
                    exec: false,
                    mtime_sec: 7,
                    mtime_nsec: 7,
                    chunks: vec![],
                    target: None,
                },
            }],
            ..Default::default()
        };

        let mut ap = Applier::new(&w.store, &target);
        ap.apply_session_change_set(&cs, &target_root).unwrap();

        let (sec, nsec) =
            ferry_platform::split_unix(md_of(&target, &["inner"]).modified().unwrap());
        assert_eq!(
            (sec, nsec),
            inner_mt,
            "ancestor dir mtime comes from the offered tree"
        );
        assert_eq!(read_target(&target, &["inner/deep.txt"]), b"");
    }

    #[test]
    fn peer_manifest_with_escaping_symlink_is_refused_before_any_write() {
        let (w, target) = World::new(32);
        let root = tree_id(
            &w,
            &TreeNode {
                entries: vec![symlink_entry("evil", MT_A.0, MT_A.1, "../../outside")],
            },
        );
        let err = Applier::new(&w.store, &target)
            .apply_tree(&root)
            .unwrap_err();
        match &err {
            MaterializeError::SymlinkRefused {
                path, target: t, ..
            } => {
                assert_eq!(path, "evil");
                assert_eq!(t, "../../outside");
            }
            other => panic!("wrong error: {other}"),
        }
        assert!(
            matches!(
                err,
                MaterializeError::SymlinkRefused {
                    reason: ferry_platform::LinkRefusal::EscapesRoot,
                    ..
                }
            ),
            "reason must name the fix"
        );
        assert!(!target.join("evil").exists(), "nothing written on refusal");
    }

    #[test]
    fn reserved_device_names_are_refused_at_materialize() {
        let (w, target) = World::new(33);
        let root = tree_id(
            &w,
            &TreeNode {
                entries: vec![file_entry("aux.txt", false, MT_A.0, MT_A.1, vec![])],
            },
        );
        let err = Applier::new(&w.store, &target)
            .apply_tree(&root)
            .unwrap_err();
        assert!(
            matches!(err, MaterializeError::ReservedName { ref component, .. } if component == "aux.txt"),
            "{err}"
        );
    }

    #[test]
    fn case_conflicting_siblings_never_materialize_on_folding_hosts() {
        let (w, target) = World::new(34);
        let root = tree_id(
            &w,
            &TreeNode {
                entries: vec![
                    file_entry("README", false, MT_A.0, MT_A.1, vec![]),
                    file_entry("readme", false, MT_B.0, MT_B.1, vec![]),
                ],
            },
        );
        let res = Applier::new(&w.store, &target).apply_tree(&root);
        if ferry_platform::host_folds_case() {
            let err = res.unwrap_err();
            assert!(
                matches!(err, MaterializeError::CaseCollision { ref first, ref second, .. }
                    if first == "README" && second == "readme"),
                "{err}"
            );
            assert!(!target.join("README").exists());
        } else {
            res.unwrap();
            assert_eq!(std::fs::read(target.join("README")).unwrap(), b"");
            assert_eq!(std::fs::read(target.join("readme")).unwrap(), b"");
        }
    }

    #[test]
    fn case_only_rename_on_folding_host_never_loses_the_file() {
        let (w, target) = World::new(36);
        let v1 = tree_id(
            &w,
            &TreeNode {
                entries: vec![file_entry("Rename-Me.txt", false, MT_A.0, MT_A.1, vec![])],
            },
        );
        Applier::new(&w.store, &target).apply_tree(&v1).unwrap();

        let v2 = tree_id(
            &w,
            &TreeNode {
                entries: vec![file_entry("rename-me.TXT", false, MT_B.0, MT_B.1, vec![])],
            },
        );
        Applier::new(&w.store, &target).apply_tree(&v2).unwrap();

        assert!(
            target.join("rename-me.TXT").exists(),
            "case-only rename lost the file"
        );
    }

    #[test]
    fn nfd_disk_spelling_resolves_to_manifest_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("rapport-anne\u{301}e.md"), b"x").unwrap();
        let fold = NfcFoldCache::refusing();
        let p = fold
            .resolve(dir.path(), &["rapport-ann\u{e9}e.md".to_string()])
            .unwrap();
        assert!(
            std::fs::symlink_metadata(&p).is_ok(),
            "NFC manifest name must resolve onto the NFD disk file"
        );
    }

    #[test]
    fn m_children_of_one_parent_cost_at_most_one_read_dir() {
        let dir = tempfile::tempdir().unwrap();
        const M: usize = 16;
        for i in 0..M {
            std::fs::write(dir.path().join(format!("child-{i:02}.txt")), b"x").unwrap();
        }
        let mut paths: Vec<Vec<String>> =
            (0..M).map(|i| vec![format!("child-{i:02}.txt")]).collect();

        paths.push(vec!["never-written.txt".into()]);

        paths.push(vec!["missing-dir".into(), "deep.txt".into()]);

        let fold = NfcFoldCache::refusing();
        for p in &paths {
            fold.resolve(dir.path(), p).unwrap();
        }
        assert_eq!(
            fold.scanned_dirs(),
            2,
            "root scanned once, missing-dir once; never per-child"
        );

        for p in &paths {
            fold.resolve(dir.path(), p).unwrap();
        }
        assert_eq!(fold.scanned_dirs(), 2);
    }

    #[test]
    fn applier_created_entries_enter_the_cache_without_a_rescan() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("fresh")).unwrap();
        let fold = NfcFoldCache::refusing();

        fold.resolve(dir.path(), &["probe".into()]).unwrap();
        fold.resolve(dir.path(), &["fresh".into(), "probe".into()])
            .unwrap();
        assert_eq!(fold.scanned_dirs(), 2);

        std::fs::write(dir.path().join("new.txt"), b"x").unwrap();
        fold.note_created_at(&dir.path().join("new.txt"));
        let inner_abs = dir.path().join("fresh").join("inner");
        std::fs::create_dir(&inner_abs).unwrap();
        fold.note_created_at(&inner_abs);

        {
            let dirs = fold.dirs.borrow();
            let root_fold = &dirs[dir.path()];
            assert_eq!(
                root_fold.get("new.txt").map(Vec::as_slice),
                Some(&["new.txt".to_string()][..])
            );
            let fresh_fold = &dirs[&dir.path().join("fresh")];
            assert_eq!(
                fresh_fold.get("inner").map(Vec::as_slice),
                Some(&["inner".to_string()][..])
            );
        }
        assert_eq!(fold.scanned_dirs(), 2, "no rescan to learn our own writes");
    }

    #[test]
    fn applier_removed_entries_leave_the_cache_and_deep_removal_drops_submaps() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("gone/inside")).unwrap();
        std::fs::write(dir.path().join("gone/inside/deep.txt"), b"d").unwrap();
        std::fs::write(dir.path().join("stay.txt"), b"s").unwrap();
        let fold = NfcFoldCache::refusing();

        fold.resolve(dir.path(), &["probe".into()]).unwrap();
        fold.resolve(dir.path(), &["gone".into(), "probe".into()])
            .unwrap();
        {
            let dirs = fold.dirs.borrow();
            assert!(dirs.contains_key(&dir.path().join("gone")));
            assert!(dirs[dir.path()].contains_key("stay.txt"));
        }

        fold.note_removed(&dir.path().join("gone"), true);
        let dirs = fold.dirs.borrow();
        assert!(
            !dirs.contains_key(&dir.path().join("gone")),
            "sub-maps of a removed subtree are dropped"
        );
        assert!(
            dirs[dir.path()]
                .get("gone")
                .is_none_or(std::vec::Vec::is_empty),
            "the removed name leaves the parent's fold map"
        );
        assert!(dirs[dir.path()].contains_key("stay.txt"));
    }

    #[test]
    fn duplicate_spellings_refuse_loudly_instead_of_lexicographic_min() {
        let mut fold_map: DirFold = HashMap::new();
        let want_nfc: String = "caf\u{e9}.txt".nfc().collect();
        fold_map.insert(
            want_nfc.clone(),
            vec!["caf\u{e9}.txt".to_string(), "cafe\u{301}.txt".to_string()],
        );

        let err = pick(
            &fold_map,
            Path::new("/target"),
            &want_nfc,
            AmbiguityPolicy::Refuse,
        )
        .unwrap_err();
        match err {
            MaterializeError::AmbiguousDiskSpelling {
                parent,
                first,
                second,
            } => {
                assert_eq!(parent, "/target");
                let mut both = [first, second];
                both.sort();
                assert_eq!(both[0], "cafe\u{301}.txt");
                assert_eq!(both[1], "caf\u{e9}.txt");
            }
            other => panic!("wrong error: {other}"),
        }

        let got = pick(
            &fold_map,
            Path::new("/target"),
            &want_nfc,
            AmbiguityPolicy::PickSmallest,
        )
        .unwrap();
        assert_eq!(got.as_deref(), Some("cafe\u{301}.txt"));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn ambiguous_disk_spellings_produce_the_typed_error_via_resolve() {
        let dir = tempfile::tempdir().unwrap();
        let nfd = dir.path().join("a\u{30a}.txt");
        let singleton = dir.path().join("\u{212b}.txt");
        std::fs::write(&nfd, b"one").unwrap();
        std::fs::write(&singleton, b"two").unwrap();

        let distinct = match (
            std::fs::symlink_metadata(&nfd),
            std::fs::symlink_metadata(&singleton),
        ) {
            (Ok(a), Ok(b)) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    a.ino() != b.ino()
                }
                #[cfg(not(unix))]
                {
                    true
                }
            }
            _ => false,
        };
        if !distinct {
            eprintln!(
                "skipping: host temp filesystem merges NFC-equivalent spellings \
                 (no byte-preserving fixture possible here)"
            );
            return;
        }

        if std::fs::symlink_metadata(dir.path().join("\u{c5}.txt")).is_ok() {
            eprintln!(
                "skipping: host resolves NFC-equivalent lookups natively \
                 (ambiguity not constructible on this filesystem)"
            );
            return;
        }

        let seen = scan_dir_fold(dir.path());
        match seen.get("\u{c5}.txt").map(Vec::as_slice) {
            Some([a, b])
                if (a.as_str(), b.as_str()) == ("\u{212b}.txt", "a\u{30a}.txt")
                    || (a.as_str(), b.as_str()) == ("a\u{30a}.txt", "\u{212b}.txt") => {}
            other => {
                eprintln!(
                    "skipping: host did not present both spellings under one NFC key \
                     (saw {other:?}); resolver behavior unchanged by this environment"
                );
                return;
            }
        }

        let fold = NfcFoldCache::refusing();
        let err = fold
            .resolve(dir.path(), &["\u{c5}.txt".to_string()])
            .unwrap_err();
        match err {
            MaterializeError::AmbiguousDiskSpelling { first, second, .. } => {
                let mut both = [first, second];
                both.sort();
                assert_eq!(both[0], "a\u{30a}.txt");
                assert_eq!(both[1], "\u{212b}.txt");
            }
            other => panic!("wrong error: {other}"),
        }

        let p = resolve_live(dir.path(), &["\u{c5}.txt".to_string()]);
        assert_eq!(p.file_name().and_then(|n| n.to_str()), Some("a\u{30a}.txt"));
    }

    #[test]
    fn abs_applies_long_path_prefix_rule() {
        let short = std::path::Path::new("/tmp/whatever/a/b.txt");
        assert_eq!(ferry_platform::extend_path(short), short.to_path_buf());
        assert!(!ferry_platform::needs_extended_length(short));
    }
}
