//! The reconcile task (「输入代数与派生状态」「执行」): one per project, a
//! `Background` task holding a `BusyGuard`, at most one at a time and run
//! again when an input changed while it ran. It reads the current
//! generations, recomputes what is out of date in dependency order (base
//! map → worktree comparison → overlay; the cursors follow in the batch
//! tasks) and installs each result only if the generations it was computed
//! from are still current.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use lectern_executor::Priority;
use lectern_git::HashKind;
use lectern_platform::{FileKind as SafeKind, Read, RelPath as SafeRel, SafeError, SafeRoot};
use lectern_services::project::{BlobId, ProjectKey, RelPath, Scan};

use super::state::{Project, State};
use super::{Inner, bump, lock};
use crate::extract::BACKGROUND_MAX_LEN;
use crate::git_access::{Tree, hash_blob, hash_kind};
use crate::map::{BaseMap, HeadDiff, IndexedHints, LangLookup, LiveMap, Overlay, WorktreeState, find_path};
use crate::util::log;

/// Whether `path`'s content equals HEAD, by the rule of「依赖 git、fs_watch 与
/// cache 的接口」.
pub(crate) fn head_clean(st: &State, path: &str) -> bool {
    let diff = &st.head_diff;
    if diff.head != st.gens.head || diff.index != st.gens.index || diff.files != st.gens.files {
        return false;
    }
    if st.compare_for.is_none() {
        return false;
    }
    let files = st.map.base.files();
    let effective = match st.map.overlay.get(path) {
        Some(WorktreeState::Content(oid)) => Some(oid),
        Some(_) => None,
        None => find_path(files, path).and_then(|i| files.oid(i)),
    };
    let Some(effective) = effective else {
        return false;
    };
    match diff.head_oid(path) {
        Some(head) => head == Some(effective),
        // Not listed: HEAD has the index entry's OID, and the worktree has no
        // overlay entry for it.
        None => st.map.overlay.get(path).is_none(),
    }
}

impl Inner {
    /// Requests a reconcile: starts the task, or has the running one go
    /// around again.
    pub(crate) fn request_reconcile(self: &Arc<Self>, project: &Arc<Project>) {
        {
            let mut st = lock(&project.state);
            if st.run.running {
                st.run.again = true;
                return;
            }
            st.run.running = true;
        }
        let me = self.clone();
        let p = project.clone();
        if !self
            .host
            .spawn_busy("reconcile", Priority::Background, move || me.reconcile(p))
        {
            lock(&project.state).run.running = false;
        }
    }

    fn reconcile(self: Arc<Self>, project: Arc<Project>) {
        bump(&self.stats.reconciles, 1);
        let mut held = Vec::new();
        loop {
            held.extend(std::mem::take(&mut lock(&project.state).held));
            let mut changed = true;
            while changed {
                changed = self.reconcile_once(&project);
            }
            let mut st = lock(&project.state);
            if st.run.again {
                st.run.again = false;
                continue;
            }
            st.run.running = false;
            break;
        }
        // The batches fs_watch delivered are handled: the overlay is up to
        // date with them.
        drop(held);
        self.enqueue_focus(&project);
        self.kick_batch();
    }

    /// One pass over the steps. True when something was installed, so that
    /// a later step may now be out of date.
    fn reconcile_once(self: &Arc<Self>, project: &Arc<Project>) -> bool {
        let mut changed = false;
        let Some(session) = self.sync_snapshot(project) else {
            return false;
        };
        changed |= self.open_root(project, session);
        changed |= self.check_head_index(project, session);
        changed |= self.rebuild_base(project, session);
        changed |= self.compare(project, session);
        changed |= self.reread(project, session);
        changed
    }

    /// Takes the project's snapshot: a new active worktree starts a new
    /// session; a new file list or sub-project list bumps its generation.
    /// None when the project is gone.
    fn sync_snapshot(&self, project: &Arc<Project>) -> Option<u64> {
        let snapshot = self.projects.snapshot(&project.key)?;
        let mut st = lock(&project.state);
        if st.cancel.load(Ordering::Acquire) && !lock(&self.open).iter().any(|p| Arc::ptr_eq(p, project)) {
            return None;
        }
        let new_active = snapshot.active.as_ref().map(|a| (a.worktree.clone(), a.root.clone()));
        let old_active = st
            .snapshot
            .as_ref()
            .and_then(|s| s.active.as_ref())
            .map(|a| (a.worktree.clone(), a.root.clone()));
        if st.snapshot.is_some() && new_active != old_active {
            // A new current worktree: every input record starts over; the
            // focus stays under its worktree's name.
            let (table, grammar) = {
                let langs = lock(&self.langs);
                (langs.table.clone(), langs.generation)
            };
            let focus = st.focus.clone();
            let demand = st.demand;
            let run = std::mem::take(&mut st.run);
            st.end();
            *st = State::new(self.new_session(), &table, grammar);
            st.focus = focus;
            st.demand = demand;
            st.run = run;
        }
        if let Some(active) = &snapshot.active {
            st.tree = match (&project.key, &active.worktree) {
                (ProjectKey::Git { common_dir }, Some(wt)) => Some(Tree {
                    common_dir: common_dir.clone(),
                    git_dir: wt.0.clone(),
                    root: active.root.clone(),
                }),
                _ => None,
            };
            let files = match &active.files {
                Scan::Ready(files) => Some(files.clone()),
                _ => None,
            };
            if !same(&files, &st.seen.files) {
                st.gens.files += 1;
                if let Some(oid) = files.as_ref().and_then(|f| f.oid(0)) {
                    st.hash = hash_kind(&oid);
                }
                st.seen.files = files;
            }
            let packages = match &active.packages {
                Scan::Ready(p) => Some(p.clone()),
                _ => None,
            };
            if !same(&packages, &st.seen.packages) {
                st.gens.packages += 1;
                st.seen.packages = packages;
            }
        }
        st.snapshot = Some(snapshot);
        Some(st.session)
    }

    /// Opens the worktree root and subscribes to fs_watch, once the file
    /// list is ready (「打开项目与热启动」1).
    fn open_root(self: &Arc<Self>, project: &Arc<Project>, session: u64) -> bool {
        let tree = {
            let st = lock(&project.state);
            if st.root.is_some() || st.seen.files.is_none() {
                return false;
            }
            match &st.tree {
                Some(tree) => tree.clone(),
                None => return false,
            }
        };
        let root = match SafeRoot::open(&tree.root) {
            Ok(root) => Arc::new(root),
            Err(error) => {
                log(format_args!("cannot open {}: {error}", tree.root.display()));
                return false;
            }
        };
        let watches = self.watch(project, session, &tree);
        let mut st = lock(&project.state);
        if st.session != session {
            return false;
        }
        st.root = Some(root);
        st.watches = watches;
        // Whatever happened before the subscription: compare once more.
        st.gens.compare += 1;
        true
    }

    fn check_head_index(&self, project: &Arc<Project>, session: u64) -> bool {
        let (tree, head, index) = {
            let mut st = lock(&project.state);
            let Some(tree) = st.tree.clone() else {
                return false;
            };
            let flags = (st.check_head, st.check_index);
            st.check_head = false;
            st.check_index = false;
            (tree, flags.0, flags.1)
        };
        if !head && !index {
            return false;
        }
        let head = head.then(|| self.git.head(&tree));
        let index = index.then(|| self.git.index_checksum(&tree));
        let mut st = lock(&project.state);
        if st.session != session {
            return false;
        }
        let mut changed = false;
        match head {
            Some(Ok(oid)) => {
                if st.seen.head != Some(oid) {
                    st.seen.head = Some(oid);
                    st.gens.head += 1;
                    changed = true;
                }
            }
            Some(Err(error)) => log(format_args!("reading HEAD failed: {error}")),
            None => {}
        }
        match index {
            Some(Ok(sum)) => {
                if st.seen.index != Some(sum) {
                    st.seen.index = Some(sum);
                    st.gens.index += 1;
                    changed = true;
                }
            }
            Some(Err(error)) => log(format_args!("reading the index checksum failed: {error}")),
            None => {}
        }
        changed
    }

    /// The base map against the file list and the language table.
    fn rebuild_base(&self, project: &Arc<Project>, session: u64) -> bool {
        let (deps, files) = {
            let st = lock(&project.state);
            let deps = (st.gens.files, st.gens.grammar);
            if st.base_for == Some(deps) {
                return false;
            }
            let Some(files) = st.seen.files.clone() else {
                return false;
            };
            (deps, files)
        };
        let table = lock(&self.langs).table.clone();
        bump(&self.stats.base_builds, 1);
        let base = Arc::new(BaseMap::build(files, &table, &*self.packs));
        let mut st = lock(&project.state);
        if st.session != session || (st.gens.files, st.gens.grammar) != deps {
            return false;
        }
        let hints = IndexedHints::carried(&st.map.hints, st.map.base.files(), base.files());
        let entries = st.map.overlay.entries().to_vec();
        let overlay = Overlay::build(entries, &base, &*self.packs);
        st.map = LiveMap {
            base,
            overlay: Arc::new(overlay),
            hints: Arc::new(hints),
        };
        st.base_for = Some(deps);
        true
    }

    /// The worktree comparison: `HeadDiff`, and which paths to read again.
    fn compare(&self, project: &Arc<Project>, session: u64) -> bool {
        let (deps, tree, cancel) = {
            let st = lock(&project.state);
            let deps = st.gens.compare_deps();
            if st.compare_for == Some(deps) || st.base_for.is_none() || st.root.is_none() {
                return false;
            }
            let Some(tree) = st.tree.clone() else {
                return false;
            };
            (deps, tree, st.cancel.clone())
        };
        bump(&self.stats.compares, 1);
        let result = self.git.changed_paths(&tree, &cancel);
        let mut st = lock(&project.state);
        if st.session != session || st.gens.compare_deps() != deps {
            return false;
        }
        st.compare_for = Some(deps);
        let changed = match result {
            Ok(changed) => changed,
            Err(error) => {
                // Keep the overlay; the next event tries again.
                log(format_args!("comparing the worktree failed: {error}"));
                return true;
            }
        };
        st.head_diff = Arc::new(HeadDiff::new(
            changed.head,
            st.gens.head,
            st.gens.index,
            st.gens.files,
        ));
        let listed: BTreeSet<RelPath> = changed.paths.into_iter().collect();
        // Every existing entry is read again with what the comparison lists;
        // an untracked one it no longer lists is now ignored or gone.
        let mut kept = Vec::new();
        let mut dropped = false;
        for (i, (path, state)) in st.map.overlay.entries().iter().enumerate() {
            let untracked = st.map.overlay.base_index(i).is_none();
            if untracked && !changed.untracked_capped && !listed.contains(path) {
                dropped = true;
                continue;
            }
            kept.push((path.clone(), *state));
        }
        let paths: Vec<RelPath> = kept.iter().map(|(p, _)| p.clone()).collect();
        st.dirty.extend(paths);
        st.dirty.extend(listed);
        if dropped {
            let overlay = Overlay::build(kept, &st.map.base, &*self.packs);
            st.map.overlay = Arc::new(overlay);
            st.gens.worktree += 1;
        }
        true
    }

    /// Reads the dirty paths and updates the overlay.
    fn reread(&self, project: &Arc<Project>, session: u64) -> bool {
        let (paths, root, tree, base, deps, hash) = {
            let mut st = lock(&project.state);
            if st.base_for.is_none() {
                return false;
            }
            let Some(root) = st.root.clone() else {
                return false;
            };
            if st.dirty.is_empty() && st.dirty_dirs.is_empty() {
                return false;
            }
            let mut paths = std::mem::take(&mut st.dirty);
            let dirs = std::mem::take(&mut st.dirty_dirs);
            for dir in &dirs {
                let files = st.map.base.files().clone();
                for i in files.range_under(dir) {
                    if let Some((p, _)) = files.get(i) {
                        paths.insert(p.into());
                    }
                }
                for (p, _) in st.map.overlay.entries() {
                    if super::state::under(p, dir) {
                        paths.insert(p.clone());
                    }
                }
            }
            (
                paths,
                root,
                st.tree.clone(),
                st.map.base.clone(),
                (st.gens.files, st.gens.grammar),
                st.hash,
            )
        };
        let table = base.table().clone();
        let mut lookup = LangLookup::new(&table, &*self.packs);
        let mut updates: HashMap<RelPath, Option<WorktreeState>> = HashMap::new();
        let mut saw_dir = false;
        let mut read = 0;
        for path in &paths {
            if lookup.code(path) == 0 {
                continue;
            }
            read += 1;
            let (state, dir) = self.read_state(&root, tree.as_ref(), &base, path, hash);
            saw_dir |= dir;
            updates.insert(path.clone(), state);
        }
        bump(&self.stats.rereads, read);
        let mut st = lock(&project.state);
        if st.session != session || (st.gens.files, st.gens.grammar) != deps {
            st.dirty.extend(paths);
            return true;
        }
        if saw_dir {
            st.gens.compare += 1;
        }
        let mut entries: Vec<(RelPath, WorktreeState)> = st
            .map
            .overlay
            .entries()
            .iter()
            .filter(|(p, _)| !updates.contains_key(p))
            .cloned()
            .collect();
        entries.extend(
            updates
                .into_iter()
                .filter_map(|(p, s)| s.map(|s| (p, s))),
        );
        let overlay = Overlay::build(entries, &st.map.base, &*self.packs);
        let differs = overlay.entries() != st.map.overlay.entries();
        if differs {
            st.map.overlay = Arc::new(overlay);
            st.gens.worktree += 1;
        }
        differs || saw_dir
    }

    /// What the worktree holds at `path` (「blob → 路径映射」), and whether
    /// the path is a directory now.
    fn read_state(
        &self,
        root: &SafeRoot,
        tree: Option<&Tree>,
        base: &BaseMap,
        path: &str,
        hash: HashKind,
    ) -> (Option<WorktreeState>, bool) {
        let files = base.files();
        let at = find_path(files, path);
        let index = at.and_then(|i| files.oid(i));
        let Some(rel) = SafeRel::new(path.as_bytes()) else {
            return (Some(WorktreeState::Unindexable), false);
        };
        match root.read(rel, BACKGROUND_MAX_LEN as u64) {
            Ok(Read::File { bytes, .. }) => {
                let raw = hash_blob(hash, &bytes);
                (Some(self.content_state(tree, path, &bytes, raw, index)), false)
            }
            Ok(Read::Unsupported { meta }) if meta.kind == SafeKind::Dir => (
                at.map(|_| WorktreeState::Absent),
                true,
            ),
            Ok(Read::Symlink { .. } | Read::TooLarge { .. } | Read::Unsupported { .. }) => {
                (Some(WorktreeState::Unindexable), false)
            }
            Err(SafeError::NotFound) => (Some(WorktreeState::Absent), false),
            Err(SafeError::PathThroughSymlink | SafeError::Io(_)) => {
                (Some(WorktreeState::Unindexable), false)
            }
        }
    }

    /// The state of a file read in full. Its raw OID is the overlay's OID;
    /// when the index has the path with another OID, the built-in
    /// conversions decide whether the content still equals it (CRLF
    /// checkouts) or a filter's pointer is checked out (`Unindexable`).
    fn content_state(
        &self,
        tree: Option<&Tree>,
        path: &str,
        bytes: &[u8],
        raw: BlobId,
        index: Option<BlobId>,
    ) -> WorktreeState {
        let (Some(index), Some(tree)) = (index, tree) else {
            return WorktreeState::Content(raw);
        };
        if index == raw {
            return WorktreeState::Content(raw);
        }
        match self.git.canonical_oid(tree, path, bytes) {
            Ok(None) => WorktreeState::Unindexable,
            Ok(Some(canonical)) if canonical == index => WorktreeState::Content(index),
            Ok(Some(_)) => WorktreeState::Content(raw),
            Err(error) => {
                log(format_args!("converting {path} failed: {error}"));
                WorktreeState::Content(raw)
            }
        }
    }
}

fn same<T: ?Sized>(a: &Option<Arc<T>>, b: &Option<Arc<T>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}
