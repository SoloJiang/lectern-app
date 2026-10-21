//! The dependency tier in the batch layer (「依赖层」「何时建索引」): after
//! tier 4, before tier 5, under the yielding guard like them. A step starts
//! the next queued package's walk (its entry), visits up to 500 files of
//! the walk's frontier (their stats; a stat table's OID or the bytes'; one
//! existence check: a stored unit's edges come from `cd_spec`, the rest go
//! to the tier's queue to be extracted and written with their edges), or
//! finishes the walk (its path table written, the dependency map
//! replaced). The extraction of the queued files is the batch layer's own
//! (`extract_batch` on queue 3): pause, cancel, the watchdog, the large
//! slot and the yield are the other tiers'.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lectern_cache::{Unit, Written};
use lectern_platform::FileKind;
use lectern_services::code_intel::Ecosystem;
use lectern_services::project::BlobId;

use super::json::Json;
use super::npm::{entry_candidates, is_relative, spec_candidates, types_package};
use super::table::{self, PathTable};
use super::{
    FILE_MAX, MANIFEST_MAX, MapInput, PkgDir, PkgWalk, PkgWork, Roots, Stat, WalkFile, dep_lang,
    install_map, vouches, wall_nanos,
};
use crate::extract::{ExtractStatus, SpecKind, Specifier};
use crate::git_access::hash_blob;
use crate::index::batch::{Ctx, Done};
use crate::index::state::{DepSource, Item, Project, Source, State, Tier};
use crate::index::{Inner, bump, lock};
use crate::store::{EXISTS_BATCH, Found, Version};

/// The dependency tier's queue in `Batch::resume` and `Batch::aside`.
pub(crate) const QUEUE: usize = 3;

/// One visited file before the existence check.
struct Visited {
    rel: String,
    file: super::DepFile,
    stat: Stat,
    oid: BlobId,
}

/// What a step does, taken under the project's lock.
enum Todo {
    Start(u64, PkgWork),
    Visit {
        id: u64,
        rels: Vec<String>,
        work: PkgWork,
        roots: Arc<Roots>,
    },
    Finish(u64),
}

impl Inner {
    /// One step of the dependency tier; false when it has nothing to do.
    /// `slot`: a keepalive slot (「交互让路」), which does one item: at most
    /// one file read and one existence check.
    pub(crate) fn deps_step(
        self: &Arc<Self>,
        project: &Arc<Project>,
        ctx: &Ctx,
        slot: bool,
    ) -> bool {
        let todo = {
            let mut st = lock(&project.state);
            if !st.live(ctx.session) || !st.deps.has_work() {
                return false;
            }
            let Some(roots) = st.deps.roots.clone() else {
                return false;
            };
            let deps = &mut st.deps;
            if deps.walk.is_none() {
                let Some(work) = deps.queue.pop_front() else {
                    return false;
                };
                deps.walks += 1;
                deps.walk = Some(PkgWalk::new(deps.walks, work));
            }
            let walk = deps.walk.as_mut().expect("a walk");
            if !walk.started {
                Todo::Start(walk.id, walk.work.clone())
            } else if !walk.frontier.is_empty() {
                let n = if slot { 1 } else { EXISTS_BATCH };
                let take = n.min(walk.frontier.len());
                Todo::Visit {
                    id: walk.id,
                    rels: walk.frontier.drain(..take).collect(),
                    work: walk.work.clone(),
                    roots,
                }
            } else if walk.done() {
                Todo::Finish(walk.id)
            } else {
                return false;
            }
        };
        match todo {
            Todo::Start(id, work) => self.start_walk(project, ctx, id, &work),
            Todo::Visit {
                id,
                rels,
                work,
                roots,
            } => self.visit(project, ctx, id, &work, &roots, rels, slot),
            Todo::Finish(id) => self.finish_walk(project, ctx, id),
        }
        true
    }

    /// A walk's start: the package's declaration entry, the first of its
    /// candidates that is a file there; none, its `@types` package is
    /// queued in front, as a package of its own.
    fn start_walk(self: &Arc<Self>, project: &Arc<Project>, ctx: &Ctx, id: u64, work: &PkgWork) {
        let roots = lock(&project.state).deps.roots.clone();
        let Some(roots) = roots else {
            return;
        };
        let pkg = &work.pkg;
        let manifest = roots
            .file(pkg, "package.json", Ecosystem::Npm)
            .and_then(|f| roots.read(&f, MANIFEST_MAX).ok())
            .and_then(|(bytes, _)| Json::parse(&bytes));
        let entry = manifest.as_ref().and_then(|m| {
            entry_candidates(m).into_iter().find(|rel| {
                roots
                    .file(pkg, rel, Ecosystem::Npm)
                    .and_then(|f| roots.stat(&f).ok())
                    .is_some_and(|m| m.kind == FileKind::File)
            })
        });
        let types = match &entry {
            Some(_) => None,
            None => types_package(&work.origin.package).and_then(|name| {
                let chain = roots.chain(&work.subproject);
                let (pkg, stat) = super::identity::locate(&roots, &chain, &name)?;
                let hash = lock(&project.state).hash;
                let w = super::identity::package_work(
                    &roots,
                    hash,
                    &name,
                    pkg,
                    stat,
                    &None,
                    &work.subproject,
                    &work.worktree,
                )?;
                Some((name, w))
            }),
        };
        let lookup = types.as_ref().map(|(_, w)| {
            let langs = lock(&self.langs).table.clone();
            let mut touches = Vec::new();
            let found = super::identity::look_up(&project.deps, &langs, w, &mut touches);
            project.deps.touch(&touches);
            found
        });
        let mut st = lock(&project.state);
        if !st.live(ctx.session) {
            return;
        }
        let deps = &mut st.deps;
        let Some(walk) = deps.walk.as_mut().filter(|w| w.id == id) else {
            return;
        };
        walk.started = true;
        if let Some(entry) = entry {
            walk.visit(entry);
        }
        if let Some((name, w)) = types {
            walk.types = Some(name.clone());
            // Right after the package it gives declarations to.
            let sub = w.subproject.clone();
            if let Some(r) = deps.rank(&sub, &work.origin.package) {
                deps.set_rank(&sub, &name, r + 1);
            }
            match lookup {
                Some(super::identity::Found::Ready { table, missing }) => {
                    if missing {
                        deps.queue.push_front(PkgWork {
                            stat_table: Some(table.clone()),
                            ..w
                        });
                    }
                    deps.add_ready(&sub, table);
                    let input = MapInput::of(&st);
                    drop(st);
                    install_map(project, input);
                }
                _ => {
                    let stat_table = None;
                    deps.queue.push_front(PkgWork { stat_table, ..w });
                }
            }
        }
    }

    /// Visits `rels` (relative to the package directory) of walk `id`:
    /// stats, OIDs (the stat table's when its five stats vouch, else the
    /// bytes'), one existence check. A stored unit's edges are read from
    /// `cd_spec` and followed; the others are queued for extraction.
    #[allow(clippy::too_many_arguments, reason = "one step of a walk")]
    fn visit(
        self: &Arc<Self>,
        project: &Arc<Project>,
        ctx: &Ctx,
        id: u64,
        work: &PkgWork,
        roots: &Arc<Roots>,
        rels: Vec<String>,
        slot: bool,
    ) {
        let stat_files: HashMap<&str, &WalkFile> = work
            .stat_table
            .as_ref()
            .map(|t| t.files.iter().map(|f| (&*f.path, f)).collect())
            .unwrap_or_default();
        let recorded = work.stat_table.as_ref().map_or(0, |t| t.recorded);
        let hash = lock(&project.state).hash;
        let mut visited: Vec<Visited> = Vec::new();
        let mut unread: Vec<WalkFile> = Vec::new();
        let mut put_back: Vec<String> = Vec::new();
        let (mut read, mut bytes) = (0u64, 0u64);
        let mut reads = 0usize;
        let (mut session_read, walk_bytes) = {
            let st = lock(&project.state);
            let w = st.deps.walk.as_ref().filter(|w| w.id == id);
            (st.deps.read, w.map_or(0, |w| w.bytes))
        };
        let mut over = false;
        let limits = super::limits();
        for rel in rels {
            if over || (slot && reads >= 1) {
                put_back.push(rel);
                continue;
            }
            let Some(file) = roots.file(&work.pkg, &rel, Ecosystem::Npm) else {
                continue;
            };
            let Ok(meta) = roots.stat(&file) else {
                continue;
            };
            if meta.kind != FileKind::File {
                continue;
            }
            let stat = Stat::of(&meta.identity());
            let path = file.path();
            if stat.size > FILE_MAX {
                // Never read: not part of the closure's bytes.
                unread.push(WalkFile {
                    path: Arc::from(path),
                    oid: None,
                    stat,
                    status: ExtractStatus::TooLarge.code().unwrap_or(2),
                });
                continue;
            }
            if walk_bytes + bytes + stat.size > limits.package_bytes {
                // The closure's 16 MiB: what is left is not followed.
                over = true;
                continue;
            }
            bytes += stat.size;
            let shortcut = stat_files
                .get(&*path)
                .filter(|f| vouches(&f.stat, recorded, &stat))
                .and_then(|f| f.oid);
            let oid = match shortcut {
                Some(oid) => oid,
                None => {
                    if session_read + stat.size > limits.session_bytes {
                        // The session's 64 MiB: the tier stops here.
                        over = true;
                        put_back.push(rel);
                        let mut st = lock(&project.state);
                        if st.live(ctx.session) {
                            stop(&mut st);
                        }
                        break;
                    }
                    let Ok((content, _)) = roots.read(&file, FILE_MAX) else {
                        continue;
                    };
                    reads += 1;
                    session_read += content.len() as u64;
                    read += content.len() as u64;
                    hash_blob(hash, &content)
                }
            };
            visited.push(Visited {
                rel,
                file,
                stat,
                oid,
            });
        }
        // One existence check per language of the visited files.
        let langs = lock(&self.langs).table.clone();
        let mut groups: Vec<(Version, Vec<usize>)> = Vec::new();
        for (i, v) in visited.iter().enumerate() {
            let Some(lang) = dep_lang(&langs, &v.rel) else {
                continue;
            };
            let version = Version::of(lang);
            match groups.iter_mut().find(|(g, _)| *g == version) {
                Some((_, members)) => members.push(i),
                None => groups.push((version, vec![i])),
            }
        }
        let mut found: HashMap<usize, Found> = HashMap::new();
        for (version, members) in &groups {
            let oids: Vec<BlobId> = members.iter().map(|&i| visited[i].oid).collect();
            bump(&self.stats.exists_checks, oids.len().div_ceil(EXISTS_BATCH));
            let hits = project.deps.exists(version, &oids).unwrap_or_default();
            for &i in members {
                if let Some(h) = hits.iter().find(|h| h.oid == visited[i].oid) {
                    found.insert(i, *h);
                }
            }
        }
        // A hit's edges are its stored specifiers.
        let done = ExtractStatus::Done.code().unwrap_or(0);
        let hit_units: Vec<(usize, Unit)> = found
            .iter()
            .filter(|(_, f)| f.status == ExtractStatus::Done)
            .map(|(&i, f)| (i, f.unit))
            .collect();
        let specs = project
            .deps
            .specs_of(&hit_units.iter().map(|(_, u)| *u).collect::<Vec<_>>())
            .unwrap_or_default();
        let mut edges: Vec<String> = Vec::new();
        for ((i, _), specs) in hit_units.iter().zip(&specs) {
            edges.extend(resolve(roots, &work.pkg, &visited[*i].rel, specs));
        }
        let touched: Vec<Unit> = found.values().map(|f| f.unit).collect();
        if !touched.is_empty() {
            project.deps.touch(&touched);
            bump(&self.stats.touched, touched.len());
        }
        let mut items = Vec::new();
        let mut recorded_files = unread;
        for (i, v) in visited.into_iter().enumerate() {
            match found.get(&i) {
                Some(h) => recorded_files.push(WalkFile {
                    path: Arc::from(v.file.path()),
                    oid: Some(v.oid),
                    stat: v.stat,
                    status: h.status.code().unwrap_or(done),
                }),
                None => {
                    let Some(lang) = dep_lang(&langs, &v.rel) else {
                        continue;
                    };
                    items.push(Item {
                        path: Arc::from(v.file.path()),
                        oid: v.oid,
                        lang: lang.clone(),
                        source: Source::Dep(Arc::new(DepSource {
                            file: v.file,
                            roots: roots.clone(),
                            walk: Some(id),
                            stat: v.stat,
                            rel: v.rel,
                        })),
                        tier: Tier::Deps,
                    });
                }
            }
        }
        let mut st = lock(&project.state);
        if !st.live(ctx.session) {
            return;
        }
        st.deps.read += read;
        if st.deps.read >= limits.session_bytes {
            stop(&mut st);
        }
        let State { deps, batch, .. } = &mut *st;
        let Some(walk) = deps.walk.as_mut().filter(|w| w.id == id) else {
            return;
        };
        walk.bytes += bytes;
        if over {
            // The closure's limit: nothing more is visited.
            walk.frontier.clear();
        }
        // A slot's files left for the next step (one read a slot).
        if !over {
            for rel in put_back.into_iter().rev() {
                walk.frontier.push_front(rel);
            }
        }
        walk.files.extend(recorded_files);
        for e in edges {
            walk.visit(e);
        }
        walk.pending += items.len();
        batch.resume[QUEUE].extend(items);
    }

    /// Writes the dependency tier's (and tier 1's dependency files')
    /// extractions to `code_intel.deps` in one transaction, with their
    /// specifiers; a key stored meanwhile is not written again (its stored
    /// specifiers are the edges). A walk's file is recorded with its OID,
    /// stats and status, its edges followed. Our own write evicting
    /// dependency units stops the tier for the session (ADR 0033).
    pub(crate) fn commit_deps(
        self: &Arc<Self>,
        project: &Arc<Project>,
        ctx: &Ctx,
        done: Vec<Done>,
        batch: bool,
    ) {
        if done.is_empty() {
            return;
        }
        let table = lock(&self.langs).table.clone();
        let live = |d: &Done| {
            !ctx.cancel.load(Ordering::Acquire)
                && !d.flag.load(Ordering::Acquire)
                && table
                    .get(&d.item.lang.name().0)
                    .is_some_and(|l| Version::of(l) == Version::of(&d.item.lang))
        };
        // Each item's edges: its extraction's, or a stored unit's.
        let mut specs: Vec<Option<Vec<Specifier>>> = vec![None; done.len()];
        let mut statuses: Vec<Option<ExtractStatus>> = vec![None; done.len()];
        let evicted = {
            let _turn = lock(&project.deps_commits);
            let before = project.deps.evictions();
            let mut write = project.deps.write();
            // A `Done` extraction over a stored `TooLarge` or `TimedOut`
            // unit replaces it: our own bump, not the quota's.
            let mut replaces = false;
            let mut stored: Vec<(usize, Unit)> = Vec::new();
            for (i, d) in done.iter().enumerate() {
                if !live(d) {
                    continue;
                }
                let version = Version::of(&d.item.lang);
                let hit = project
                    .deps
                    .exists(&version, &[d.key])
                    .unwrap_or_default()
                    .into_iter()
                    .next();
                match hit {
                    Some(f) if !outranks(d.extraction.status, f.status) => {
                        statuses[i] = Some(f.status);
                        if f.status == ExtractStatus::Done {
                            stored.push((i, f.unit));
                        }
                    }
                    found => {
                        if write.add_unit(d.key, &version, &d.extraction, d.meta) {
                            replaces |= found.is_some();
                            statuses[i] = Some(d.extraction.status);
                            specs[i] = Some(d.extraction.specs.clone());
                        }
                    }
                }
            }
            let units: Vec<Unit> = stored.iter().map(|(_, u)| *u).collect();
            if let Some(found) = project.deps.specs_of(&units) {
                for ((i, _), s) in stored.iter().zip(found) {
                    specs[*i] = Some(s);
                }
            }
            if !units.is_empty() {
                project.deps.touch(&units);
            }
            let committed = !write.is_empty() && matches!(write.commit(), Written::Committed);
            let after = project.deps.evictions();
            committed && quota_evicted(before, after, replaces)
        };
        // The edges, resolved outside the lock (stats).
        let mut edges: Vec<Vec<String>> = vec![Vec::new(); done.len()];
        for (i, d) in done.iter().enumerate() {
            let (Source::Dep(dep), Some(s)) = (&d.item.source, &specs[i]) else {
                continue;
            };
            if dep.walk.is_none() {
                continue;
            }
            let roots = &dep.roots;
            let pkg = lock(&project.state)
                .deps
                .walk
                .as_ref()
                .filter(|w| Some(w.id) == dep.walk)
                .map(|w| w.work.pkg.clone());
            if let Some(pkg) = pkg {
                edges[i] = resolve(roots, &pkg, &dep.rel, s);
            }
        }
        let mut st = lock(&project.state);
        if st.session != ctx.session {
            return;
        }
        if evicted && batch {
            stop(&mut st);
        }
        for (i, d) in done.into_iter().enumerate() {
            let Source::Dep(dep) = &d.item.source else {
                continue;
            };
            if let Some(walk) = st
                .deps
                .walk
                .as_mut()
                .filter(|w| dep.walk.is_some() && Some(w.id) == dep.walk)
            {
                walk.pending = walk.pending.saturating_sub(1);
                if let Some(status) = statuses[i] {
                    walk.files.push(WalkFile {
                        path: d.item.path.clone(),
                        oid: Some(d.key),
                        stat: dep.stat,
                        status: status.code().unwrap_or(0),
                    });
                }
                for e in std::mem::take(&mut edges[i]) {
                    walk.visit(e);
                }
            }
            st.finish(ctx.session, &d.item, &d.flag);
        }
        drop(st);
        if let Some(me) = self.arc() {
            me.kick_batch();
        }
    }

    /// An item of the dependency tier that gave nothing to write: its walk
    /// stops waiting for it.
    pub(crate) fn release_dep(&self, st: &mut State, item: &Item) {
        if let Source::Dep(dep) = &item.source
            && let Some(walk) = st
                .deps
                .walk
                .as_mut()
                .filter(|w| dep.walk.is_some() && Some(w.id) == dep.walk)
        {
            walk.pending = walk.pending.saturating_sub(1);
        }
    }

    /// The walk's end: its path table written (with the walk's files, the
    /// `@types` package it used), ready, the dependency map replaced.
    fn finish_walk(self: &Arc<Self>, project: &Arc<Project>, ctx: &Ctx, id: u64) {
        let (table, subproject) = {
            let mut st = lock(&project.state);
            if !st.live(ctx.session) {
                return;
            }
            let Some(mut walk) = st.deps.walk.take_if(|w| w.id == id) else {
                return;
            };
            walk.sort_files();
            let table = PathTable {
                unit: None,
                origin: walk.work.origin.clone(),
                identity: walk.work.identity.clone(),
                manifest: walk.work.manifest,
                dir: Arc::from(walk.work.pkg.path()),
                recorded: wall_nanos(self.wall_now()),
                types: walk.types.clone(),
                worktree: walk.work.worktree.clone(),
                loaded: true,
                files: walk.files,
            };
            (table, walk.work.subproject)
        };
        let evicted = {
            let _turn = lock(&project.deps_commits);
            // A table of this key already stored (a repair, a re-walk after
            // the manifest's stats or a racy record): the write replaces
            // it, which the cache counts as one eviction of the namespace.
            let replaces = table::stored(&project.deps, &table);
            let before = project.deps.evictions();
            let mut write = project.deps.write();
            table::write(&mut write, &table);
            let committed = matches!(write.commit(), Written::Committed);
            committed && quota_evicted(before, project.deps.evictions(), replaces)
        };
        let mut st = lock(&project.state);
        if !st.live(ctx.session) {
            return;
        }
        if evicted {
            stop(&mut st);
        }
        st.deps.add_ready(&subproject, Arc::new(table));
        let input = MapInput::of(&st);
        drop(st);
        install_map(project, input);
    }
}

/// Whether a committed write of `code_intel.deps` evicted for the quota:
/// the namespace's eviction generation moved from `before` to `after`, and
/// not only by the write's own replace of a unit of the same key (one bump
/// a transaction, as tags counts its replaces, 「存储」). Should the same
/// transaction also evict for the quota, that is not seen; the next write
/// that evicts is.
pub(crate) fn quota_evicted(before: u64, after: u64, replaces: bool) -> bool {
    after > before + u64::from(replaces)
}

/// Whether an extraction replaces a stored unit (as `batch::outranks`).
fn outranks(new: ExtractStatus, stored: ExtractStatus) -> bool {
    new == ExtractStatus::Done
        && matches!(stored, ExtractStatus::TimedOut | ExtractStatus::TooLarge)
}

/// Stops the dependency tier for the session: what it queued goes.
pub(crate) fn stop(st: &mut State) {
    st.deps.stopped = true;
    st.deps.queue.clear();
    st.deps.walk = None;
    st.batch.resume[QUEUE].clear();
    st.batch.aside[QUEUE].clear();
}

/// The files the relative specifiers of `from` (relative to `pkg`'s
/// directory) name: the first candidate of each that is a file there.
/// Bare module names and `reference types` are not followed.
pub(crate) fn resolve(roots: &Roots, pkg: &PkgDir, from: &str, specs: &[Specifier]) -> Vec<String> {
    let mut out = Vec::new();
    for s in specs {
        let spec = match s.kind {
            SpecKind::Module if is_relative(&s.text) => s.text.to_string(),
            SpecKind::Path if is_relative(&s.text) => s.text.to_string(),
            SpecKind::Path => format!("./{}", s.text),
            SpecKind::Module | SpecKind::Types => continue,
        };
        for candidate in spec_candidates(from, &spec) {
            let Some(file) = roots.file(pkg, &candidate, Ecosystem::Npm) else {
                continue;
            };
            if roots.stat(&file).is_ok_and(|m| m.kind == FileKind::File) {
                if !out.contains(&candidate) {
                    out.push(candidate);
                }
                break;
            }
        }
    }
    out
}

/// A new in-flight flag for a dependency item, registered by path.
pub(crate) fn register(st: &mut State, item: &Item, cancel: bool) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(cancel));
    st.deps.in_flight.insert(item.path.clone(), flag.clone());
    flag
}
