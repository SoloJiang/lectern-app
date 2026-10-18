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
use lectern_services::project::{BlobId, FileKind, ProjectKey, ProjectSnapshot, RelPath, Scan};

use super::state::{Project, State};
use super::{Inner, bump, lock};
use crate::extract::BACKGROUND_MAX_LEN;
use crate::git_access::{Tree, hash_blob, hash_kind};
use crate::map::{
    BaseMap, HeadDiff, IndexedHints, LangLookup, LiveMap, Overlay, Provenance, WorktreeState,
    find_path,
};
use crate::util::log;

/// A reconcile stops after this many passes even with input left (it never
/// should: `reconcile_settles_*` bound the passes per input change).
const MAX_PASSES: usize = 16;

/// Whether a step of the reconcile has input left to consume, with the
/// language table's generation `grammar`.
fn pending(st: &State, grammar: u64) -> bool {
    let files = st.seen.files.is_some();
    let base = st.base_for.is_some();
    let git = st.tree.is_some();
    (git && files && st.root.is_none() && !st.root_tried)
        || (git && (st.check_head || st.check_index))
        || (files && st.base_for != Some((st.gens.files, grammar)))
        || (git
            && base
            && st.root.is_some()
            && st.compare_for != Some(st.gens.compare_deps(grammar)))
        || (base
            && st.root.is_some()
            && !(st.dirty.is_empty() && st.dirty_dirs.is_empty() && st.probe.is_empty()))
}

/// Whether `path`'s content equals HEAD, by the rule of「依赖 git、fs_watch 与
/// cache 的接口」, with the language table's generation `grammar`.
/// Conservative: input the reconcile has not consumed yet makes it false —
/// a HEAD or index check, a requested comparison project-wide; a path to
/// read again (or a directory event above it) for that path.
pub(crate) fn head_clean(st: &State, path: &str, grammar: u64) -> bool {
    let diff = &st.head_diff;
    if diff.head != st.gens.head || diff.index != st.gens.index || diff.files != st.gens.files {
        return false;
    }
    if st.compare_for != Some(st.gens.compare_deps(grammar)) || st.check_head || st.check_index {
        return false;
    }
    // The path or a directory above it (a file replaced by a directory).
    let pending = |set: &std::collections::BTreeSet<RelPath>| {
        let mut at = path;
        loop {
            if set.contains(at) {
                return true;
            }
            match at.rfind('/') {
                Some(i) => at = &at[..i],
                None => return set.contains(""),
            }
        }
    };
    if pending(&st.dirty) || pending(&st.dirty_dirs) || pending(&st.probe) {
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

    pub(super) fn reconcile(self: Arc<Self>, project: Arc<Project>) {
        bump(&self.stats.reconciles, 1);
        let mut held = Vec::new();
        loop {
            held.extend(std::mem::take(&mut lock(&project.state).held));
            // Each step consumes its input (a generation it is out of date
            // with, or a set of paths); a pass is repeated only while some
            // input is left and the pass changed something, and no step
            // produces input for itself: a comparison's paths are read,
            // never compared again. Input a pass could not consume (its
            // result was stale) waits for the request of whoever changed
            // it.
            for pass in 1.. {
                bump(&self.stats.reconcile_passes, 1);
                let Some(progress) = self.reconcile_once(&project) else {
                    break;
                };
                let grammar = self.grammar();
                if !progress || !pending(&lock(&project.state), grammar) {
                    break;
                }
                if pass == MAX_PASSES {
                    log(format_args!(
                        "the reconcile of {:?} did not settle in {MAX_PASSES} passes",
                        project.key
                    ));
                    break;
                }
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
        self.names_changed(&project);
        self.warm_site_queries(&project);
    }

    /// Compiles `ParsedFile`'s query for every language some open
    /// project's map has a file in and that is not compiled yet, in one
    /// `Background` task: a jump's step 1, the search and find references
    /// parse files on the `Interactive` thread, and a core language's
    /// extraction (the walker) never compiles it, so the first jump in each
    /// language would pay 30–140 ms. After the first frame like the whole
    /// plugin (`AfterFirstFrame`); it holds the kernel busy.
    ///
    /// Level-triggered: a request only makes sure the task runs; the task
    /// itself looks at every open project, compiles until it finds nothing
    /// left, and looks once more after clearing `warming`, so a request
    /// that found the task running (another project's reconcile, a map
    /// installed meanwhile) is never lost.
    pub(crate) fn warm_site_queries(self: &Arc<Self>, project: &Arc<Project>) {
        // Nothing of this project's is cold: its request asks for nothing.
        if !lock(&project.state)
            .map
            .languages()
            .iter()
            .any(|l| !l.site_query_ready())
        {
            return;
        }
        if self.warming.swap(true, Ordering::AcqRel) {
            return;
        }
        let me = self.clone();
        let spawned = self
            .host
            .spawn_busy("site queries", Priority::Background, move || {
                loop {
                    let cold = me.cold_languages();
                    if cold.is_empty() {
                        me.warming.store(false, Ordering::Release);
                        // A request between the look and the store found
                        // `warming` set and left its languages to this task.
                        if me.cold_languages().is_empty() || me.warming.swap(true, Ordering::AcqRel)
                        {
                            return;
                        }
                        continue;
                    }
                    for lang in &cold {
                        lang.prepare_site();
                    }
                }
            });
        if !spawned {
            self.warming.store(false, Ordering::Release);
        }
    }

    /// The languages of every open project's map whose `ParsedFile` query
    /// is not compiled yet. A language in several maps is compiled once
    /// (`prepare_site` again is a no-op).
    pub(crate) fn cold_languages(&self) -> Vec<crate::extract::Lang> {
        let open: Vec<Arc<Project>> = lock(&self.open).clone();
        let mut cold = Vec::new();
        for project in open {
            let map = lock(&project.state).map.clone();
            cold.extend(
                map.languages()
                    .into_iter()
                    .filter(|l| !l.site_query_ready()),
            );
        }
        cold
    }

    /// One pass over the steps, in dependency order: whether some step
    /// installed or consumed something. None when the project is gone.
    fn reconcile_once(self: &Arc<Self>, project: &Arc<Project>) -> Option<bool> {
        let session = self.sync_snapshot(project)?;
        let mut progress = self.open_root(project, session);
        progress |= self.check_head_index(project, session);
        progress |= self.rebuild_base(project, session);
        progress |= self.compare(project, session);
        progress |= self.reread(project, session);
        Some(progress)
    }

    /// The language table's generation. Shared by every project, so it is
    /// read here, never copied into a project (the copy could lag behind).
    /// Never called with a project's state locked.
    pub(crate) fn grammar(&self) -> u64 {
        lock(&self.langs).generation
    }

    /// Takes the project's snapshot: a new active worktree starts a new
    /// session; a new file list or sub-project list bumps its generation.
    /// None when the project is gone.
    fn sync_snapshot(&self, project: &Arc<Project>) -> Option<u64> {
        let snapshot = self.projects.snapshot(&project.key)?;
        // `open` is never taken while a project's state is locked.
        let is_open = self.is_open(project);
        let mut st = lock(&project.state);
        if st.cancel.load(Ordering::Acquire) && !is_open {
            return None;
        }
        let new_active = snapshot
            .active
            .as_ref()
            .map(|a| (a.worktree.clone(), a.root.clone()));
        let old_active = st
            .snapshot
            .as_ref()
            .and_then(|s| s.active.as_ref())
            .map(|a| (a.worktree.clone(), a.root.clone()));
        let switched = st.snapshot.is_some() && new_active != old_active;
        if switched {
            // Normally ended by the `Changed` event already
            // (`switch_session`); a snapshot the event did not mark.
            drop(st);
            self.switch_session(project, &snapshot);
            st = lock(&project.state);
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
        let session = st.session;
        drop(st);
        Some(session)
    }

    /// Ends the session when `snapshot`'s current worktree is not the
    /// session's (「查询的视图」): a new session starts with no map, the old
    /// one's queries are closed and their semantic requests cancelled, all
    /// under the lock that orders publication, so no query of the old
    /// session publishes after this returns. The queries asked from now on
    /// wait for the new session's map. The old state is dropped in a task
    /// (its watches and map). True when it switched.
    pub(crate) fn switch_session(
        &self,
        project: &Arc<Project>,
        snapshot: &ProjectSnapshot,
    ) -> bool {
        let new_active = snapshot
            .active
            .as_ref()
            .map(|a| (a.worktree.clone(), a.root.clone()));
        let (table, grammar) = {
            let langs = lock(&self.langs);
            (langs.table.clone(), langs.generation)
        };
        let old = {
            let _order = lock(&self.publishing);
            let old = {
                let mut st = lock(&project.state);
                let old_active = st
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.active.as_ref())
                    .map(|a| (a.worktree.clone(), a.root.clone()));
                if st.snapshot.is_none() || new_active == old_active {
                    return false;
                }
                // Every input record starts over; the focus stays under its
                // worktree's name.
                let focus = st.focus.clone();
                let demand = st.demand;
                let run = std::mem::take(&mut st.run);
                st.end();
                let mut old =
                    std::mem::replace(&mut *st, State::new(self.new_session(), &table, grammar));
                st.focus = focus;
                st.demand = demand;
                st.run = run;
                // Queries that waited for the old session's map were asked
                // of the old worktree: closed below with the others.
                old.waiting.clear();
                old
            };
            self.close_queries(Some(&project.key));
            old
        };
        // Without an activation the task is dropped unrun, and `old` with it.
        let _ = self
            .host
            .spawn_busy("old session", Priority::Background, move || drop(old));
        if let Some(me) = self.arc() {
            me.request_reconcile(project);
        }
        true
    }

    /// Opens the worktree root and subscribes to fs_watch, once the file
    /// list is ready (「打开项目与热启动」1).
    fn open_root(self: &Arc<Self>, project: &Arc<Project>, session: u64) -> bool {
        if matches!(project.key, ProjectKey::Folder { .. }) {
            return self.open_folder(project, session);
        }
        let tree = {
            let mut st = lock(&project.state);
            if st.root.is_some() || st.root_tried || st.seen.files.is_none() {
                return false;
            }
            let Some(tree) = st.tree.clone() else {
                return false;
            };
            // Once per subscription: a root that cannot be opened is tried
            // again on the next `ProjectEvent`.
            st.root_tried = true;
            tree
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

    /// A folder project (「blob → 路径映射」, the last point): no index, no
    /// fs_watch, no comparison. Its root is opened once the file list is
    /// ready, and every file of the list is read: its raw bytes' SHA-1 is
    /// its OID, an overlay entry like an untracked file's. A new file list
    /// is read again in full (`rebuild_base`); `project` does not watch
    /// folders, so nothing changes in between.
    fn open_folder(&self, project: &Arc<Project>, session: u64) -> bool {
        let path = {
            let mut st = lock(&project.state);
            if st.root.is_some() || st.root_tried || st.seen.files.is_none() {
                return false;
            }
            let Some(path) = st
                .snapshot
                .as_ref()
                .and_then(|s| s.active.as_ref())
                .map(|a| a.root.clone())
            else {
                return false;
            };
            st.root_tried = true;
            path
        };
        let root = match SafeRoot::open(&path) {
            Ok(root) => Arc::new(root),
            Err(error) => {
                log(format_args!("cannot open {}: {error}", path.display()));
                return false;
            }
        };
        let mut st = lock(&project.state);
        if st.session != session {
            return false;
        }
        st.root = Some(root);
        queue_folder(&mut st);
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
        let current = self.grammar();
        let files = {
            let st = lock(&project.state);
            if st.base_for == Some((st.gens.files, current)) {
                return false;
            }
            let Some(files) = st.seen.files.clone() else {
                return false;
            };
            (st.gens.files, files)
        };
        // The table and its generation together: the map's provenance is
        // the table it was really built with.
        let (table, grammar) = {
            let langs = lock(&self.langs);
            (langs.table.clone(), langs.generation)
        };
        let deps = (files.0, grammar);
        bump(&self.stats.base_builds, 1);
        let base = Arc::new(BaseMap::build(files.1, &table, &*self.packs));
        // A newer table: the round that changed it requested a reconcile.
        let current = self.grammar();
        let mut st = lock(&project.state);
        if st.session != session || (st.gens.files, current) != deps {
            return false;
        }
        let hints = IndexedHints::carried(&st.map.hints, st.map.base.files(), base.files());
        let entries = st.map.overlay.entries().to_vec();
        let overlay = Overlay::build(entries, &base, &*self.packs);
        st.map = LiveMap {
            base,
            overlay: Arc::new(overlay),
            hints: Arc::new(hints),
            provenance: Provenance {
                files: deps.0,
                worktree: st.gens.worktree,
                grammar: deps.1,
            },
        };
        st.base_for = Some(deps);
        if matches!(project.key, ProjectKey::Folder { .. }) {
            // A new list: every file is read again for its OID.
            queue_folder(&mut st);
        }
        // The session has its map: the queries that waited for it start.
        let waiting = std::mem::take(&mut st.waiting);
        drop(st);
        if !waiting.is_empty()
            && let Some(me) = self.arc()
        {
            me.resume_waiting(waiting);
        }
        true
    }

    /// The worktree comparison: `HeadDiff`, and which paths to read again.
    fn compare(&self, project: &Arc<Project>, session: u64) -> bool {
        let grammar = self.grammar();
        let (deps, tree, cancel) = {
            let st = lock(&project.state);
            let deps = st.gens.compare_deps(grammar);
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
        let grammar = self.grammar();
        let mut st = lock(&project.state);
        if st.session != session || st.gens.compare_deps(grammar) != deps {
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
            st.map.provenance.worktree = st.gens.worktree;
        }
        true
    }

    /// Reads the dirty paths and updates the overlay.
    fn reread(&self, project: &Arc<Project>, session: u64) -> bool {
        let (paths, probe, root, tree, base, deps, hash) = {
            let mut st = lock(&project.state);
            if st.base_for.is_none() {
                return false;
            }
            let Some(root) = st.root.clone() else {
                return false;
            };
            if st.dirty.is_empty() && st.dirty_dirs.is_empty() && st.probe.is_empty() {
                return false;
            }
            let probe = std::mem::take(&mut st.probe);
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
                probe,
                root,
                st.tree.clone(),
                st.map.base.clone(),
                // Read against this base map: installed only on it.
                (st.map.provenance.files, st.map.provenance.grammar),
                st.hash,
            )
        };
        // An event path the map does not know may be a new directory (a
        // directory moved in, a clone inside the worktree): it asks for one
        // comparison. Paths a comparison listed never ask for another, so a
        // directory it lists (an untracked nested repository) is read once.
        let saw_dir = probe.iter().any(|path| {
            SafeRel::new(path.as_bytes())
                .is_some_and(|rel| root.stat(rel).is_ok_and(|m| m.kind == SafeKind::Dir))
        });
        let table = base.table().clone();
        let mut lookup = LangLookup::new(&table, &*self.packs);
        let mut updates: HashMap<RelPath, Option<WorktreeState>> = HashMap::new();
        let mut read = 0;
        for path in &paths {
            if lookup.code(path) == 0 {
                continue;
            }
            read += 1;
            let state = self.read_state(&root, tree.as_ref(), &base, path, hash);
            updates.insert(path.clone(), state);
        }
        bump(&self.stats.rereads, read);
        let mut st = lock(&project.state);
        if st.session != session {
            return false;
        }
        if (st.map.provenance.files, st.map.provenance.grammar) != deps {
            // A new base map meanwhile (its rebuild is progress): read again
            // against it.
            st.dirty.extend(paths);
            st.probe.extend(probe);
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
        entries.extend(updates.into_iter().filter_map(|(p, s)| s.map(|s| (p, s))));
        let overlay = Overlay::build(entries, &st.map.base, &*self.packs);
        let differs = overlay.entries() != st.map.overlay.entries();
        if differs {
            st.map.overlay = Arc::new(overlay);
            st.gens.worktree += 1;
            st.map.provenance.worktree = st.gens.worktree;
        }
        differs || saw_dir
    }

    /// What the worktree holds at `path` (「blob → 路径映射」). A directory
    /// where the file list has a file is `Absent`.
    fn read_state(
        &self,
        root: &SafeRoot,
        tree: Option<&Tree>,
        base: &BaseMap,
        path: &str,
        hash: HashKind,
    ) -> Option<WorktreeState> {
        let files = base.files();
        let at = find_path(files, path);
        let index = at.and_then(|i| files.oid(i));
        let Some(rel) = SafeRel::new(path.as_bytes()) else {
            return Some(WorktreeState::Unindexable);
        };
        match root.read(rel, BACKGROUND_MAX_LEN as u64) {
            Ok(Read::File { bytes, .. }) => {
                let raw = hash_blob(hash, &bytes);
                Some(self.content_state(tree, path, &bytes, raw, index))
            }
            Ok(Read::Unsupported { meta }) if meta.kind == SafeKind::Dir => {
                at.map(|_| WorktreeState::Absent)
            }
            Ok(Read::Symlink { .. } | Read::TooLarge { .. } | Read::Unsupported { .. }) => {
                Some(WorktreeState::Unindexable)
            }
            Err(SafeError::NotFound) => Some(WorktreeState::Absent),
            Err(SafeError::PathThroughSymlink | SafeError::Io(_)) => {
                Some(WorktreeState::Unindexable)
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

/// Every file of a folder project's list goes to the paths to read, once
/// its root is open: a folder's OIDs come from its bytes (the base part is
/// empty, every file an overlay entry).
fn queue_folder(st: &mut State) {
    if st.root.is_none() {
        return;
    }
    let Some(files) = st.seen.files.clone() else {
        return;
    };
    for (path, kind) in files.iter() {
        if matches!(kind, FileKind::File | FileKind::Executable) {
            st.dirty.insert(path.into());
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
