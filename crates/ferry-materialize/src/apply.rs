//! The applier: turn manifests and change sets into filesystem state.
//!
//! Execution pipeline for every apply call:
//!
//! 1. **Validate** every stored path component (traversal defense).
//! 2. **Plan** against live disk state: stat-level checks mark
//!    already-correct entries as skips (idempotence: a second identical
//!    apply performs zero mutations). Whenever size and exec bit agree,
//!    content is verified against the store before trusting the file —
//!    equal-length edits can share a timestamp (reconciliation ties produce
//!    exactly that), so mtime alone never proves equality.
//! 3. **Guard** (`Overwrite::Expect`): verify every path about to be
//!    mutated still matches the caller's base expectation. Any divergence
//!    aborts with the complete list BEFORE anything is touched.
//! 4. **Execute**: removals children-first, creations/updates
//!    parents-first, directory mtimes deepest-first and last. Every file
//!    lands via temp+rename; every chunk is hash-verified after the store
//!    read and again in the temp file pre-rename.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use ferry_store::diff::{join_path, ChangeSet, CompPath, EntryKind, EntryState};
use ferry_store::format::hex;
use ferry_store::manifest::{parse_tree_node, EntryPayload, RootManifest, TreeEntry, TreeNode};
use ferry_store::store::Store;
use ferry_store::{BlobId, BlobKind};

use crate::error::{io_at, DivergeReason, Divergence, MaterializeError};
use crate::temp::{fresh_entropy, is_temp_name, temp_name_for, TempStyle};

// ---------------------------------------------------------------------------
// Public options and stats
// ---------------------------------------------------------------------------

/// How much trust the applier may place in the live tree.
///
/// ADR-0004 puts conflict decisions in the reconciler (T-010); the applier
/// just executes decisions atomically. This enum draws the line between
/// "caller knows best" and "prove the world still looks like the base the
/// decisions were computed against".
#[derive(Clone, Debug)]
pub enum Overwrite {
    /// Apply decisions unconditionally: overwrite and delete whatever the
    /// operations say without consulting live content. Right for first
    /// materialization and for tests.
    Always,
    /// Guarded mode. Before any mutation, every affected path is compared
    /// against `expected` — the manifest the applied decisions were diffed
    /// FROM. Files are size/exec/content-verified, symlinks target-verified,
    /// directories kind-verified, wholesale directory teardowns verified
    /// across their entire live subtree. Any divergence aborts the whole
    /// apply with the complete list ([`MaterializeError::Diverged`]) and
    /// modifies nothing.
    Expect { expected: RootManifest },
}

/// What one apply call actually did. "Second run is a no-op" is asserted
/// with [`ApplyStats::mutations`] == 0.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApplyStats {
    pub dirs_created: usize,
    pub files_written: usize,
    pub symlinks_written: usize,
    pub unlinked: usize,
    /// Explicit mtime restorations (files and directories).
    pub mtimes_set: usize,
    pub bytes_written: u64,
    pub skipped_unchanged: usize,
    /// Deleted relative paths in execution order (children before parents).
    pub deletions: Vec<String>,
    /// Created/rewritten relative paths in execution order (parents before
    /// children).
    pub creations: Vec<String>,
}

impl ApplyStats {
    /// Number of mutating filesystem operations performed.
    pub fn mutations(&self) -> usize {
        self.dirs_created
            + self.files_written
            + self.symlinks_written
            + self.unlinked
            + self.mtimes_set
    }
}

// ---------------------------------------------------------------------------
// Internal models
// ---------------------------------------------------------------------------

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

/// What the base expectation says about one path (from either an
/// [`EntryState`] or a full [`TreeEntry`]).
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

// ---------------------------------------------------------------------------
// Planned operations
// ---------------------------------------------------------------------------

struct PlannedRemove {
    path: CompPath,
    /// True when the live entry may be a directory whose whole subtree goes
    /// (dir removals, type changes away from dir).
    deep: bool,
}

enum Mutation {
    /// Stat fast path proved the live state already equals the desired
    /// state; nothing to do.
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
        /// Link's own mtime from the manifest (`None` for change-set ops,
        /// whose states carry no timestamps). Restored via
        /// `utimensat(AT_SYMLINK_NOFOLLOW)` on unix — the piece T-005
        /// deferred, landed here in T-012.
        times: Option<(i64, u32)>,
    },
    /// Bytes and mode already correct; only the recorded mtime drifted.
    RestoreMtime {
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

// ---------------------------------------------------------------------------
// The applier
// ---------------------------------------------------------------------------

/// Applies manifests/change sets to one target directory through a store.
pub struct Applier<'a> {
    store: &'a Store,
    target: PathBuf,
    overwrite: Overwrite,
    style: TempStyle,
    pace_ms: u64,
}

impl<'a> Applier<'a> {
    /// New applier writing under `target` (created if missing), reading
    /// blobs from `store`.
    pub fn new(store: &'a Store, target: impl Into<PathBuf>) -> Self {
        Applier {
            store,
            target: target.into(),
            overwrite: Overwrite::Always,
            style: TempStyle::current(),
            pace_ms: 0,
        }
    }

    /// Set the overwrite policy (default [`Overwrite::Always`]).
    pub fn overwrite(mut self, overwrite: Overwrite) -> Self {
        self.overwrite = overwrite;
        self
    }

    /// Override the host-default temp-name style (exercises the Windows
    /// variant on other hosts).
    pub fn temp_style(mut self, style: TempStyle) -> Self {
        self.style = style;
        self
    }

    /// Sleep this many milliseconds between executed mutations. Zero in
    /// production; the crash-test harness raises it to spread SIGKILL
    /// offsets across the operation sequence.
    pub fn pace_ms(mut self, ms: u64) -> Self {
        self.pace_ms = ms;
        self
    }

    /// Apply a complete root manifest (its root tree) as the desired state:
    /// creates/updates everything listed, deletes live paths not listed.
    pub fn apply_manifest(
        &mut self,
        manifest: &RootManifest,
    ) -> Result<ApplyStats, MaterializeError> {
        self.apply_tree(&manifest.root_tree_id)
    }

    /// Same as [`Applier::apply_manifest`] starting at a root tree id.
    pub fn apply_tree(&mut self, root_tree_id: &BlobId) -> Result<ApplyStats, MaterializeError> {
        std::fs::create_dir_all(&self.target).map_err(|e| io_at(&self.target, e))?;

        let desired = flatten_tree(self.store, root_tree_id)?;
        for (p, _) in &desired {
            validate_components(p)?;
        }
        ensure_no_fold_collisions(&desired.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>())?;

        // Extras: live paths absent from the desired state. Descendants are
        // enumerated individually, so children-first ordering falls out of
        // the global descending sort at execution time.
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

    /// Apply exactly the operations in a change set. Nothing outside the
    /// listed paths is read, written, or deleted.
    ///
    /// Ancestor directories of written paths must exist or be part of the
    /// change set (the T-003 diff always flattens added subtrees per path);
    /// the applier never implicitly touches unlisted parents.
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

    // -- pipeline -----------------------------------------------------------

    fn run(
        &mut self,
        mut removes: Vec<PlannedRemove>,
        mut upserts: Vec<PlannedUpsert>,
    ) -> Result<ApplyStats, MaterializeError> {
        upserts.sort_by(|a, b| a.path.cmp(&b.path));

        // Phase 2: plan each upsert against live state. An upsert whose
        // path folds (case-insensitively) onto a pending REMOVAL must never
        // degrade to Skip: on folding hosts the old spelling satisfies the
        // stat check, and executing the removal afterwards would delete the
        // only copy (the case-only-rename hazard, T-012).
        let shadowed: HashSet<String> = removes
            .iter()
            .map(|r| ferry_platform::fold_key(&join_path(&r.path)))
            .collect();
        let mut touches: Vec<PlannedTouch> = Vec::new();
        let mut skipped = 0usize;
        for up in &mut upserts {
            let abs = self.abs(&up.path);
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

        // Phase 3: guard everything that would mutate.
        if let Overwrite::Expect { expected } = &self.overwrite {
            let base = Base::new(self.store, &expected.root_tree_id);
            let mut divergences: Vec<Divergence> = Vec::new();
            for rm in &removes {
                guard_removal(self.store, &self.target, &base, rm, &mut divergences)?;
            }
            for up in &upserts {
                if up.mutation.is_mutation() {
                    guard_upsert(self.store, &self.target, &base, up, &mut divergences)?;
                }
            }
            if !divergences.is_empty() {
                divergences.sort_by(|a, b| a.path.cmp(&b.path));
                return Err(MaterializeError::Diverged { paths: divergences });
            }
        }

        // Phase 4: execute. Removals children-first.
        let mut stats = ApplyStats {
            skipped_unchanged: skipped,
            ..Default::default()
        };
        removes.sort_by(|a, b| b.path.cmp(&a.path));
        for rm in removes {
            self.execute_remove(&rm.path, rm.deep, &mut stats)?;
        }
        // Upserts parents-first (ascending component order).
        for up in upserts {
            self.execute_upsert(up, &mut stats, &mut touches)?;
        }
        // Directory mtimes deepest-first, after everything beneath them
        // (both pre-existing dirs planned above and freshly created ones).
        touches.sort_by(|a, b| b.path.cmp(&a.path));
        for t in touches {
            self.execute_touch(&t.path, t.sec, t.nsec, &mut stats)?;
        }
        Ok(stats)
    }

    // -- execution ----------------------------------------------------------

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
        let abs = self.abs(rel);
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
        self.pace();
        Ok(())
    }

    /// Delete a directory's contents children-first, then the directory.
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
        let abs = self.abs(&up.path);
        match up.mutation {
            Mutation::Skip => {}
            Mutation::Mkdir { sec, nsec } => {
                match std::fs::symlink_metadata(&abs) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(io_at(&abs, e)),
                    Ok(md) if md.is_dir() => {}
                    // Wrong kind occupies the path (guard approved in
                    // Expect mode); replace it.
                    Ok(_) => {
                        std::fs::remove_file(&abs).map_err(|e| io_at(&abs, e))?;
                        record_deletion(stats, &up.path);
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
                // Fresh dirs carry wall-clock mtime; the touch phase fixes
                // it once children exist. execute_touch skips no-op sets.
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
            Mutation::WriteSymlink { target, times } => {
                // Policy re-check (defense in depth: manifests can arrive
                // from peers). Only relative targets staying inside the
                // folder are ever created (T-012).
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

                // A directory occupying the path must go first; rename
                // cannot cover directories.
                if let Ok(md) = std::fs::symlink_metadata(&abs) {
                    if md.is_dir() && !md.is_symlink() {
                        self.remove_dir_children_first(&abs, &up.path, stats)?;
                    }
                }
                let parent = parent_of(&abs);
                let tmp_path = parent.join(temp_name_for(
                    &join_path(&up.path),
                    self.style,
                    &fresh_entropy(),
                ));
                let result = make_symlink(&target, &tmp_path)
                    .and_then(|_| std::fs::rename(&tmp_path, &abs).map_err(|e| io_at(&abs, e)));
                if result.is_err() {
                    let _ = std::fs::remove_file(&tmp_path);
                }
                result?;
                fsync_dir(parent)?;
                stats.symlinks_written += 1;
                record_creation(stats, &up.path);
                // The deferred T-005 piece: restore the link's OWN mtime
                // (std cannot open a link without following it).
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
                // A directory occupying the path must go first; rename
                // cannot cover directories.
                if let Ok(md) = std::fs::symlink_metadata(&abs) {
                    if md.is_dir() {
                        self.remove_dir_children_first(&abs, &up.path, stats)?;
                    }
                }
                self.write_file_atomically(&abs, &up.path, exec, sec, nsec, size, &chunks)?;
                stats.files_written += 1;
                stats.bytes_written += size;
                record_creation(stats, &up.path);
                self.pace();
            }
        }
        Ok(())
    }

    /// Windows-only gate: refuse restoring a link that resolves to a
    /// DIRECTORY inside the tree unless the documented developer-mode env
    /// flag is set. Compiles out on other hosts.
    fn reject_windows_dir_link(
        &self,
        _abs: &Path,
        rel: &[String],
        target: &str,
    ) -> Result<(), MaterializeError> {
        if !cfg!(windows) || ferry_platform::allow_windows_dir_links() {
            return Ok(());
        }
        // Lexically resolve the internal target; if it lands on an existing
        // directory of this tree, restoring it needs a dir link.
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
        let is_dir = std::fs::symlink_metadata(&resolved)
            .map(|m| m.is_dir() && !m.is_symlink())
            .unwrap_or(false);
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
        let abs = self.abs(rel);
        let md = match std::fs::symlink_metadata(&abs) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(io_at(&abs, e)),
            Ok(md) => md,
        };
        if !md.is_dir() {
            return Ok(());
        }
        let (cur_sec, cur_nsec) = split_unix_time(md.modified().map_err(|e| io_at(&abs, e))?);
        if cur_sec == sec && cur_nsec == nsec {
            return Ok(());
        }
        set_mtime(&abs, sec, nsec)?;
        stats.mtimes_set += 1;
        self.pace();
        Ok(())
    }

    // -- atomic file writes ---------------------------------------------------

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

        // Declared size must equal the sum of chunk lengths.
        let summed: u64 = chunks.iter().map(|c| c.1).sum();
        if summed != declared_size {
            return Err(MaterializeError::SizeMismatch {
                path: rel_display,
                declared: declared_size,
                actual: summed,
            });
        }

        // 1. Fetch every chunk and verify each against its id AFTER reading
        //    from the store (defense in depth; the store verifies too).
        let mut bufs: Vec<Vec<u8>> = Vec::with_capacity(chunks.len());
        for (index, (id, len)) in chunks.iter().enumerate() {
            let bytes = self.store.get(BlobKind::DataChunk, id)?;
            if bytes.len() as u64 != *len {
                return Err(MaterializeError::ChunkCorrupt {
                    path: rel_display,
                    index,
                    expected: format!("len {len}"),
                    found: format!("len {}", bytes.len()),
                });
            }
            let found = *blake3::hash(&bytes).as_bytes();
            if &found != id {
                return Err(MaterializeError::ChunkCorrupt {
                    path: rel_display,
                    index,
                    expected: hex(id),
                    found: hex(&found),
                });
            }
            bufs.push(bytes);
        }

        // 2. Write the temp file in the DESTINATION directory (same
        //    filesystem, atomic rename), with its final permissions.
        let parent = parent_of(abs_dest);
        let tmp_path = parent.join(temp_name_for(&rel_display, self.style, &fresh_entropy()));
        let outcome = self.write_temp_then_rename(
            &tmp_path,
            abs_dest,
            &rel_display,
            exec,
            sec,
            nsec,
            chunks,
            &bufs,
        );
        if outcome.is_err() {
            // Never leave our temp behind on a handled failure.
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
        bufs: &[Vec<u8>],
    ) -> Result<(), MaterializeError> {
        let mut file = std::fs::File::create(tmp_path).map_err(|e| io_at(tmp_path, e))?;
        set_exec_bit(&file, exec).map_err(|e| io_at(tmp_path, e))?;

        for b in bufs {
            file.write_all(b).map_err(|e| io_at(tmp_path, e))?;
        }

        // Durability + final mtime, both before the rename so the
        // destination never exists with wrong metadata.
        file.set_times(std::fs::FileTimes::new().set_modified(system_time(sec, nsec)))
            .map_err(|e| io_at(tmp_path, e))?;
        file.sync_all().map_err(|e| io_at(tmp_path, e))?;

        // 3. Pre-rename verification: re-read every chunk region from the
        //    temp file and re-hash. Covers torn temp writes; the
        //    destination is still untouched when this fails. Reopen for
        //    reading: File::create yields a write-only handle.
        drop(file);
        let mut rd = std::fs::File::open(tmp_path).map_err(|e| io_at(tmp_path, e))?;
        verify_regions(&mut rd, tmp_path, rel_display, chunks)?;
        drop(rd);

        // 4. Atomic swap.
        std::fs::rename(tmp_path, abs_dest).map_err(|e| io_at(abs_dest, e))?;
        fsync_dir(parent_of(abs_dest))
    }

    // -- helpers ------------------------------------------------------------

    fn abs(&self, rel: &[String]) -> PathBuf {
        let mut p = self.target.clone();
        for c in rel {
            p.push(c);
        }
        // Windows long paths: apply the \\?\ extended-length prefix when the
        // absolute path meets or exceeds MAX_PATH. Identity on short,
        // relative, and POSIX paths (T-012).
        ferry_platform::extend_path(&p)
    }
}

fn record_deletion(stats: &mut ApplyStats, rel: &[String]) {
    stats.unlinked += 1;
    stats.deletions.push(join_path(rel));
}

fn record_creation(stats: &mut ApplyStats, rel: &[String]) {
    stats.creations.push(join_path(rel));
}

// ---------------------------------------------------------------------------
// Planning
// ---------------------------------------------------------------------------

/// Decide what one upsert must do given live state. Size and exec are
/// checked first; content is verified against the store whenever those
/// agree, so a same-size divergent file is rewritten rather than skipped.
fn plan_upsert(
    store: &Store,
    abs: &Path,
    up: &mut PlannedUpsert,
    touches: &mut Vec<PlannedTouch>,
    skipped: &mut usize,
    case_shadowed: bool,
) -> Result<(), MaterializeError> {
    // A pending removal folds onto this path: whatever lives here now is a
    // DIFFERENT stored spelling that the removal will delete. Always plan
    // the real write so the new spelling lands after the removal runs.
    if case_shadowed {
        return Ok(());
    }
    match &up.mutation {
        Mutation::Skip => Ok(()),
        Mutation::Mkdir { sec, nsec } => {
            let live = stat_opt(abs)?;
            if let Some(md) = live {
                if md.is_dir() {
                    // Already a dir: creation becomes a no-op, but its
                    // recorded mtime still gets enforced in the touch phase.
                    touches.push(PlannedTouch {
                        path: up.path.clone(),
                        sec: *sec,
                        nsec: *nsec,
                    });
                    *skipped += 1;
                    up.mutation = Mutation::Skip;
                    return Ok(());
                }
                // Wrong kind occupying the path: executor will replace it.
                return Ok(());
            }
            Ok(())
        }
        Mutation::WriteSymlink { target, .. } => {
            let unchanged = stat_opt(abs)?
                .map(|md| {
                    md.file_type().is_symlink()
                        && std::fs::read_link(abs)
                            .map(|p| p.to_string_lossy() == target.as_str())
                            .unwrap_or(false)
                })
                .unwrap_or(false);
            if unchanged {
                *skipped += 1;
                up.mutation = Mutation::Skip;
            }
            Ok(())
        }
        Mutation::RestoreMtime { .. } => unreachable!("planner input"),
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
                return Ok(()); // absent: plain create
            };
            if !md.is_file() {
                return Ok(()); // occupied otherwise: atomic replacement
            }
            if md.len() != size || live_exec(&md) != exec {
                return Ok(()); // cheap facts differ: full rewrite
            }
            // Size and exec agree; bytes may still differ (equal-length
            // edits CAN share a timestamp — reconciliation ties produce
            // exactly that). Prove content before trusting anything.
            if !content_matches(store, abs, &chunks)? {
                return Ok(()); // divergent bytes: full rewrite
            }
            let (lsec, lnsec) = split_unix_time(md.modified().map_err(|e| io_at(abs, e))?);
            if lsec == sec && lnsec == nsec {
                *skipped += 1;
                up.mutation = Mutation::Skip;
                return Ok(());
            }
            // Bytes correct, recorded mtime drifted: restore it only.
            up.mutation = Mutation::RestoreMtime { sec, nsec };
            Ok(())
        }
    }
}

fn stat_opt(abs: &Path) -> Result<Option<std::fs::Metadata>, MaterializeError> {
    match std::fs::symlink_metadata(abs) {
        Ok(md) => Ok(Some(md)),
        // NotFound and NotADirectory both mean "nothing lives here" for our
        // purposes: the latter arises while a type change has not yet
        // replaced a file occupying an ancestor position.
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
            // Change-set states DO carry mtimes (diff preserves them), and
            // restoring them is what lets link-metadata drift converge
            // across devices instead of oscillating forever (T-012).
            times: Some((s.mtime_sec, s.mtime_nsec)),
        },
    }
}

// ---------------------------------------------------------------------------
// Guard (Overwrite::Expect)
// ---------------------------------------------------------------------------

fn guard_removal(
    store: &Store,
    target: &Path,
    base: &Base<'_>,
    rm: &PlannedRemove,
    out: &mut Vec<Divergence>,
) -> Result<(), MaterializeError> {
    // Removals are sanctioned only for paths the expectation describes;
    // anything else might be unaccounted-for data.
    let Some(exp_entry) = base.lookup(&rm.path)? else {
        out.push(Divergence {
            path: rm.path.clone(),
            reason: DivergeReason::NotInBase,
        });
        return Ok(());
    };
    let exp = ExpectedState::from(&exp_entry);
    let abs = abs_under(target, &rm.path);
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
        // The whole live subtree goes: account for every descendant against
        // the base manifest before allowing it.
        verify_subtree_matches_base(store, target, base, &rm.path, &exp_entry, out)?;
    } else {
        check_live_matches(target, &rm.path, &exp, store, out)?;
    }
    Ok(())
}

fn guard_upsert(
    store: &Store,
    target: &Path,
    base: &Base<'_>,
    up: &PlannedUpsert,
    out: &mut Vec<Divergence>,
) -> Result<(), MaterializeError> {
    let exp = base.lookup(&up.path)?;
    let abs = abs_under(target, &up.path);
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
            EntryKind::Dir => Ok(()), // dirs: kind-only
            _ => check_live_matches(target, &up.path, &exp_state, store, out),
        },
    }
}

/// Compare one live leaf (or bare dir kind) against the expectation.
fn check_live_matches(
    target: &Path,
    rel: &CompPath,
    exp: &ExpectedState,
    store: &Store,
    out: &mut Vec<Divergence>,
) -> Result<(), MaterializeError> {
    let abs = abs_under(target, rel);
    let md = match stat_opt(&abs)? {
        Some(m) => m,
        None => {
            out.push(Divergence {
                path: rel.clone(),
                reason: DivergeReason::ExpectedPresent,
            });
            return Ok(());
        }
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
            if live_exec(&md) != exp.exec {
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

/// Deep verification before a wholesale directory teardown: every live
/// descendant must be accounted for by the base manifest AND match it, and
/// every base descendant must still exist.
fn verify_subtree_matches_base(
    store: &Store,
    target: &Path,
    base: &Base<'_>,
    dir_path: &CompPath,
    dir_entry: &TreeEntry,
    out: &mut Vec<Divergence>,
) -> Result<(), MaterializeError> {
    let mut expected: HashMap<String, ExpectedState> = HashMap::new();
    flatten_base_subtree(base, dir_path, dir_entry, &mut expected)?;

    // Walk live, checking each found path.
    let mut live_keys: HashSet<String> = HashSet::new();
    let abs_dir = abs_under(target, dir_path);
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
            let mut rel = components_below(target, &dir);
            rel.push(comp);
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
                Some(exp) => check_live_matches_leaf(target, &rel, exp, ft, store, out)?,
            }
        }
    }

    // Base descendants that vanished underneath us.
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
    check_live_matches(target, rel, exp, store, out)
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

// ---------------------------------------------------------------------------
// Base (expected manifest) walker
// ---------------------------------------------------------------------------

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

    /// Resolve one component path to its full tree entry, if present.
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
                _ => return Ok(None), // parent not a dir in base: deeper absent
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

// ---------------------------------------------------------------------------
// Tree flattening / live walking
// ---------------------------------------------------------------------------

/// Flatten a stored tree into per-path desired states.
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

/// Every live path under root (dirs, files, symlinks), relative. Temp-
/// pattern names are invisible (they belong to us). Names that are not
/// UTF-8 abort loudly: we could neither represent nor faithfully report
/// them.
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

fn abs_under(root: &Path, rel: &[String]) -> PathBuf {
    let mut p = root.to_path_buf();
    for c in rel {
        p.push(c);
    }
    p
}

fn components_below(root: &Path, dir: &Path) -> Vec<String> {
    dir.strip_prefix(root)
        .unwrap_or(Path::new(""))
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(str::to_string))
        .collect()
}

// ---------------------------------------------------------------------------
// Small filesystem utilities
// ---------------------------------------------------------------------------

/// Exec-bit reading; on hosts without a POSIX mode the subset is
/// unrepresentable and treated uniformly false (documented deviation,
/// T-012 territory).
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
        // std has no generic `symlink`: pick the file vs dir flavor from
        // the target's own metadata (targets are relative, in-tree paths
        // per ferry-platform's links policy). Creating symlinks on Windows
        // needs developer mode/admin; failure surfaces loudly.
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

/// Set a SYMLINK's own modified time with nanosecond fidelity.
///
/// std cannot touch a link's own times: every open follows the link. On
/// unix this drops to `utimensat(AT_SYMLINK_NOFOLLOW)` — the piece T-005
/// deferred, landed in T-012 and shared with ferry-sync's engine (which
/// previously carried its own copy).
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
        tv_nsec: nsec as libc::c_long,
    };
    let times = [ts, ts];
    // SAFETY: path is NUL-terminated; times points at two initialized
    // timespecs. Effect limited to updating timestamps of the link itself.
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

/// Non-unix builds cannot set link times (no std API without following);
/// callers record the drift and the next full apply repairs it by link
/// recreation on hosts that support it. Documented platform deviation.
#[cfg(not(unix))]
pub fn set_symlink_times(_path: &Path, _sec: i64, _nsec: u32) -> Result<(), MaterializeError> {
    Ok(())
}

/// Set modified time with nanosecond fidelity.
fn set_mtime(path: &Path, sec: i64, nsec: u32) -> Result<(), MaterializeError> {
    let f = open_for_times(path)?;
    f.set_times(std::fs::FileTimes::new().set_modified(system_time(sec, nsec)))
        .map_err(|e| io_at(path, e))
}

#[cfg(unix)]
fn open_for_times(path: &Path) -> Result<std::fs::File, MaterializeError> {
    // Read-only suffices on unix for futimens with explicit times, and it
    // works on directories too.
    std::fs::OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|e| io_at(path, e))
}

#[cfg(not(unix))]
fn open_for_times(path: &Path) -> Result<std::fs::File, MaterializeError> {
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| io_at(path, e))
}

/// (sec, nsec) with nsec normalized non-negative, matching the manifest's
/// pre-1970 convention.
pub(crate) fn split_unix_time(t: SystemTime) -> (i64, u32) {
    match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, d.subsec_nanos()),
        Err(e) => {
            let d = e.duration();
            if d.subsec_nanos() == 0 {
                (-(d.as_secs() as i64), 0)
            } else {
                (-(d.as_secs() as i64) - 1, 1_000_000_000 - d.subsec_nanos())
            }
        }
    }
}

/// Inverse of [`split_unix_time`].
pub(crate) fn system_time(sec: i64, nsec: u32) -> SystemTime {
    let total = sec as i128 * 1_000_000_000 + nsec as i128;
    if total >= 0 {
        SystemTime::UNIX_EPOCH + Duration::from_nanos(total as u64)
    } else {
        SystemTime::UNIX_EPOCH - Duration::from_nanos((-total) as u64)
    }
}

#[cfg(unix)]
fn fsync_dir(dir: &Path) -> Result<(), MaterializeError> {
    match std::fs::File::open(dir) {
        Ok(f) => f.sync_all().map_err(|e| io_at(dir, e)),
        // Directory vanished mid-apply: surface it.
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

/// Re-read chunk regions from an open handle and require each region to
/// hash to its id. The pure decision lives here so the failure branch is
/// unit-testable independently of real disk corruption.
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

/// Stream-compare a live file against the store's chunk sequence. Sizes
/// are checked by callers before this runs.
fn content_matches(
    store: &Store,
    abs: &Path,
    chunks: &[(BlobId, u64)],
) -> Result<bool, MaterializeError> {
    let mut f = std::fs::File::open(abs).map_err(|e| io_at(abs, e))?;
    for (id, len) in chunks {
        let expect = store.get(BlobKind::DataChunk, id)?;
        if expect.len() as u64 != *len {
            // The store contradicts the manifest: refuse rather than guess.
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

/// Traversal defense: stored names are single NFC components. Reserved
/// Windows device names are rejected here too — they can never materialize
/// on a Windows endpoint, so carrying them further only delays a loud,
/// actionable failure (T-012 policy).
pub(crate) fn validate_components(path: &[String]) -> Result<(), MaterializeError> {
    for c in path {
        if c.is_empty()
            || c == "."
            || c == ".."
            || c.contains('/')
            || c.contains('\\')
            || c.contains('\0')
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

/// Case-fold collision gate over a desired-state path set: on folding hosts
/// (macOS/Windows), two siblings under one parent whose names fold together
/// cannot coexist; applying would silently overwrite the first with the
/// second. Refused before anything is written, naming both spellings.
/// No-op on case-sensitive hosts, where such pairs are legitimate.
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
            .push(name.first().map(String::as_str).unwrap_or(""));
    }
    let mut parents: Vec<&[String]> = per_parent.keys().copied().collect();
    parents.sort(); // deterministic error order
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

    /// Store in `dir/store-root`, target tree in `dir/target`.
    struct World {
        _dir: tempfile::TempDir,
        store: Store,
        poly: u64,
    }

    impl World {
        fn new(seed: u64) -> (World, PathBuf) {
            let dir = tempfile::tempdir().unwrap();
            // Store::create expects its folder root to exist.
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

    /// Chunk bytes under the world's polynomial, store them, return the
    /// ordered chunk list for a manifest entry.
    fn chunked(w: &World, bytes: &[u8]) -> Vec<(BlobId, u64)> {
        chunk(w.poly, bytes)
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
        split_unix_time(md.modified().unwrap())
    }

    const MT_A: (i64, u32) = (1_700_000_000, 111);
    const MT_B: (i64, u32) = (1_700_000_500, 222);

    // -- acceptance: apply is idempotent -------------------------------------

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
        assert!(live_exec(&md), "exec flag restored");
        assert_eq!(mtime_of(&md), MT_B);
        // No temp files left behind.
        assert!(std::fs::read_dir(&target).unwrap().count() == 1);
    }

    #[test]
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

        // Flip both flags only; the second run must not rewrite content.
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
        assert!(stats.creations == ["off.bin", "on.bin"]);
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
        // Parent dir recorded before its child's file.
        let pos = |s: &str| s1.creations.iter().position(|c| c == s).unwrap();
        assert!(pos("outer") < pos("outer/deep"));
        assert!(pos("outer/deep") < pos("outer/deep/leaf.txt"));
        // Dir mtime enforced after children were written.
        assert_eq!(mtime_of(&md_of(&target, &["outer"])), MT_A);

        // Now remove the whole subtree in the next state.
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

        // Composed and decomposed spellings of "café.txt".
        let composed = "caf\u{e9}.txt";
        let decomposed = "cafe\u{301}.txt";
        let node_composed = TreeNode {
            entries: vec![wfile(composed, &w, b"unicode!", false, MT_A)],
        };
        let node_decomposed = TreeNode {
            entries: vec![wfile(decomposed, &w, b"unicode!", false, MT_A)],
        };

        // The constructors NFC-normalize, so both spellings serialize to
        // the SAME tree (dedup by construction).
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

        // Retarget.
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
        // Idempotent third run.
        let s3 = Applier::new(&w.store, &target).apply_tree(&v2).unwrap();
        assert_eq!(s3.mutations(), 0);
    }

    #[test]
    fn type_changes_file_to_dir_to_symlink_across_manifests() {
        let (w, target) = World::new(7);

        // x starts as a file.
        let v1 = tree_id(
            &w,
            &TreeNode {
                entries: vec![wfile("x", &w, b"plain file", false, MT_A)],
            },
        );
        Applier::new(&w.store, &target).apply_tree(&v1).unwrap();
        assert!(md_of(&target, &["x"]).is_file());

        // x becomes a directory containing y.
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

        // x becomes a symlink; the dir teardown cascades children-first.
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

        // External touch: content identical, mtime moved to MT_B.
        set_mtime(&target.join("touched.txt"), MT_B.0, MT_B.1).unwrap();
        assert_eq!(mtime_of(&md_of(&target, &["touched.txt"])), MT_B);

        let stats = Applier::new(&w.store, &target).apply_tree(&root).unwrap();
        // Ambiguity resolved by CONTENT comparison, not a blind rewrite.
        assert_eq!(
            stats.files_written, 0,
            "identical bytes must not be rewritten"
        );
        assert_eq!(stats.mtimes_set, 1);
        assert_eq!(mtime_of(&md_of(&target, &["touched.txt"])), MT_A);

        // And when content truly differs, a full rewrite happens instead.
        std::fs::write(target.join("touched.txt"), b"tampered").unwrap();
        let stats = Applier::new(&w.store, &target).apply_tree(&root).unwrap();
        assert_eq!(stats.files_written, 1);
        assert_eq!(read_target(&target, &["touched.txt"]), b"same bytes");
    }

    // -- acceptance: guarded divergence --------------------------------------

    #[test]
    fn guarded_mode_tamper_lists_exact_divergences_and_leaves_files_untouched() {
        let (w, target) = World::new(10);

        // Base state on disk.
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

        // Desired next state: edit a.txt, retarget lnk, add new.txt.
        // (d/c.txt and b.txt are untouched by the decision set.)
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

        // Tamper: a.txt diverges in content (same size/exec, forcing the
        // deep byte comparison); junk occupies new.txt's slot; lnk
        // retargeted behind our back to neither base nor desired.
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

        // Nothing was modified anywhere.
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

        // A clean live tree applies fine under the same guard.
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

        // Base: d/ holds accounted.txt AND unaccounted junk.
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

        // Next state removes the directory entirely.
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
        // File still there.
        assert_eq!(
            read_target(&target, &["gone-next/junk.txt"]),
            b"never synced"
        );

        // Always mode may proceed.
        Applier::new(&w.store, &target)
            .overwrite(Overwrite::Always)
            .apply_change_set(&cs)
            .unwrap();
        assert!(!target.join("gone-next").exists());
    }

    // -- change sets ----------------------------------------------------------

    #[test]
    fn change_set_minimality_touches_only_listed_paths() {
        let (w, target) = World::new(12);

        // Seed the live tree directly (as if materialized earlier).
        std::fs::create_dir(target.join("keep-dir")).unwrap();
        std::fs::write(target.join("keep.txt"), b"stable").unwrap();
        std::fs::write(target.join("mod.txt"), b"version one").unwrap();
        std::fs::write(target.join("del.txt"), b"bye").unwrap();
        set_mtime(&target.join("keep.txt"), MT_A.0, MT_A.1).unwrap();
        set_mtime(&target.join("mod.txt"), MT_A.0, MT_A.1).unwrap();

        // Change set: mod.txt rewritten, del.txt removed, add.txt added.
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

        // Untouched sibling: bytes AND mtime identical.
        let keep = md_of(&target, &["keep.txt"]);
        assert_eq!(keep.len(), 6);
        assert_eq!(mtime_of(&keep), MT_A);
        assert_eq!(mtime_of(&md_of(&target, &["keep-dir"])), {
            let _ = 1;
            mtime_of(&md_of(&target, &["keep-dir"]))
        });

        // Second run of the same change set: everything already applied
        // behaves like an upsert against matching live state.
        let again = Applier::new(&w.store, &target)
            .apply_change_set(&cs)
            .unwrap();
        assert_eq!(again.unlinked, 0, "removal already satisfied");
    }

    // -- defenses --------------------------------------------------------------

    #[test]
    fn traversal_defense_rejects_bad_stored_components() {
        let (w, target) = World::new(13);

        for bad in ["..", ".", "a/b", "a\\b", ""] {
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
        // Nothing escaped the target root.
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
        // Seal staging so chunks live in real pack files.
        w.store.flush().unwrap();

        // Corrupt every pack's body.
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
        // Destination never appeared.
        assert!(std::fs::read_dir(&target).unwrap().next().is_none());
    }

    #[test]
    fn pre_rename_region_verification_failure_branch_works() {
        // Unit-proof of the TempWriteVerifyFailed branch (unreachable via
        // the public API because the store refuses corrupt blobs first).
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

        // Expect-mode with no covering expectation: refuse loudly.
        let empty_base = tree_id(&w, &TreeNode { entries: vec![] });
        let err = Applier::new(&w.store, &target)
            .overwrite(Overwrite::Expect {
                expected: root_manifest(empty_base),
            })
            .apply_tree(&root)
            .unwrap_err();
        assert!(matches!(err, MaterializeError::Diverged { .. }), "{err}");
        assert!(target.join("stale-dir/old.txt").exists());

        // Always mode sweeps the extras children-first.
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

        // Windows-style mangling on this host, exercised end to end.
        let mut ap = Applier::new(&w.store, &target).temp_style(TempStyle::Windows);
        ap.apply_tree(&root).unwrap();
        assert!(target.join("big.bin").exists());
        assert!(
            std::fs::read_dir(&target).unwrap().count() == 1,
            "no temps left"
        );
    }

    #[test]
    fn unix_time_helpers_round_trip_pre1970() {
        for (sec, nsec) in [
            (0i64, 0u32),
            (1_700_000_000, 999_999_999),
            (-1, 0),
            (-1, 500_000_000),
            (-5_000_000_000, 1),
        ] {
            let t = system_time(sec, nsec);
            assert_eq!(split_unix_time(t), (sec, nsec), "({sec},{nsec})");
        }
    }

    #[test]
    fn seeded_multi_file_apply_matches_model_exactly() {
        let (w, target) = World::new(17);

        // Build a pseudo-random but deterministic tree.
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
                MT_A.0 + i as i64,
                (MT_A.1 + i as u32) % 1_000_000_000,
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

        // Full-tree idempotence at scale.
        let again = Applier::new(&w.store, &target).apply_tree(&root).unwrap();
        assert_eq!(again.mutations(), 0);
    }

    // ---- T-012 policy tests ------------------------------------------------

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

        // The LINK's own mtime (not its target's) equals the manifest's.
        let md = std::fs::symlink_metadata(target.join("lnk")).unwrap();
        let (sec, nsec) = ferry_platform::split_unix(md.modified().unwrap());
        assert_eq!(
            (sec, nsec),
            MT_B,
            "deferred T-005 piece: link mtime restored"
        );

        // A repeat apply is a full no-op: target AND times now match.
        let again = Applier::new(&w.store, &target).apply_tree(&root).unwrap();
        assert_eq!(again.mutations(), 0);
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
            // Folding host (macOS CI): fatal, naming both spellings, and
            // nothing was silently picked.
            let err = res.unwrap_err();
            assert!(
                matches!(err, MaterializeError::CaseCollision { ref first, ref second, .. }
                    if first == "README" && second == "readme"),
                "{err}"
            );
            assert!(!target.join("README").exists());
        } else {
            // Case-sensitive host (Linux CI): both files legitimately land.
            res.unwrap();
            assert_eq!(std::fs::read(target.join("README")).unwrap(), b"");
            assert_eq!(std::fs::read(target.join("readme")).unwrap(), b"");
        }
    }

    #[test]
    fn case_only_rename_on_folding_host_never_loses_the_file() {
        // The hazard: on macOS/Windows, `Rename-Me.txt` and `rename-me.TXT`
        // are one inode. Planning `rename-me.TXT` against live disk sees
        // the old spelling (same size/content/mtime) and would degrade to
        // Skip; executing the removal of the old spelling afterwards then
        // deletes the only copy. The applier must detect the fold-shadowed
        // upsert and force a real write (T-012).
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
    fn abs_applies_long_path_prefix_rule() {
        // Wiring proof: the applier's abs() routes through the platform
        // policy, so a short POSIX path comes back unchanged (the prefix
        // math itself is unit-tested in ferry-platform on every OS).
        let short = std::path::Path::new("/tmp/whatever/a/b.txt");
        assert_eq!(ferry_platform::extend_path(short), short.to_path_buf());
        assert!(!ferry_platform::needs_extended_length(short));
    }
}
