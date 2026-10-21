//! The identity task (「身份与包的路径表」): when a project opens, a manifest
//! or lockfile changes, or tier 3's sub-project changes, one `Background`
//! task (holding a `BusyGuard`, one per project at a time, run again when
//! asked meanwhile) finds the current sub-project's direct dependencies,
//! their package directories and identities, and compares them with the
//! path tables: an unchanged package is used as stored (its files' units
//! checked by OID, the missing ones queued), any other is queued for the
//! dependency tier with its newest table as the stat table.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use lectern_executor::Priority;
use lectern_git::HashKind;
use lectern_platform::{FileKind, OpenFile, Opened, Read, RelPath as SafeRel, SafeRoot};
use lectern_services::code_intel::{DepOrigin, DepRoot, Ecosystem};
use lectern_services::project::{BlobId, RelPath};

use super::json::Json;
use super::lockfile::{Locked, read_locked};
use super::npm::direct_dependencies;
use super::table::{self, PathTable};
use super::{MANIFEST_MAX, PkgDir, PkgWork, Roots, Stat, WalkFile, dep_lang, vouches};
use crate::extract::{ExtractStatus, LanguageTable};
use crate::git_access::hash_blob;
use crate::index::batch::subproject;
use crate::index::state::{Project, Scope, State};
use crate::index::{Inner, lock};
use crate::store::{EXISTS_BATCH, Store, Version};
use crate::util::log;

/// The lockfiles npm reads, nearest first at each level.
const LOCKFILES: [&str; 2] = ["npm-shrinkwrap.json", "package-lock.json"];

/// The manifests and lockfiles whose change runs the identity task again
/// (「增量更新」).
pub(crate) const WATCHED: [&str; 5] = [
    "package.json",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "pnpm-lock.yaml",
    "yarn.lock",
];

/// The sub-project directory the dependency tier works for: tier 3's
/// sub-project, the worktree root without one.
pub(crate) fn dep_dir(st: &State) -> RelPath {
    match subproject(st) {
        Scope::Dir(d) => d,
        Scope::Whole | Scope::Empty => Arc::from(""),
    }
}

/// What one run of the identity task works with.
struct IdCtx {
    session: u64,
    root: Arc<SafeRoot>,
    roots: Arc<Roots>,
    dir: RelPath,
    hash: HashKind,
    /// The session's worktree (its root), the path tables' first key part.
    worktree: Arc<str>,
}

/// The worktree part of the path tables' keys: the current worktree's root
/// (a folder project has none: "").
pub(crate) fn worktree_of(st: &State) -> Arc<str> {
    match &st.tree {
        Some(t) => Arc::from(t.root.to_string_lossy().as_ref()),
        None => Arc::from(""),
    }
}

/// What became of one package.
pub(crate) enum Found {
    /// Its path table is current: used as stored. `missing`: some of its
    /// files' units are not stored (evicted): the walk repairs them.
    Ready {
        table: Arc<PathTable>,
        missing: bool,
    },
    /// To be walked.
    Walk,
}

impl Inner {
    /// Runs the identity task when this session has not yet for its
    /// current sub-project, or a manifest or lockfile changed (the end of
    /// a reconcile, a focus).
    pub(crate) fn deps_check(self: &Arc<Self>, project: &Arc<Project>) {
        let want = {
            let st = lock(&project.state);
            st.base_for.is_some()
                && st.root.is_some()
                && !st.cancel.load(std::sync::atomic::Ordering::Acquire)
                && (st.deps.dirty || st.deps.ran_for.as_ref() != Some(&dep_dir(&st)))
        };
        if want {
            self.request_identity(project);
        }
    }

    /// Starts the identity task, or has the running one go around again.
    pub(crate) fn request_identity(self: &Arc<Self>, project: &Arc<Project>) {
        let session = {
            let mut st = lock(&project.state);
            if st.cancel.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            if st.deps.identity.running {
                st.deps.identity.again = true;
                return;
            }
            st.deps.identity.running = true;
            st.session
        };
        let me = self.clone();
        let p = project.clone();
        let spawned = self
            .host
            .spawn_busy("deps identity", Priority::Background, move || {
                me.identity_task(&p, session);
            });
        if !spawned {
            let mut st = lock(&project.state);
            if st.session == session {
                st.deps.identity.running = false;
            }
        }
    }

    fn identity_task(self: &Arc<Self>, project: &Arc<Project>, session: u64) {
        #[cfg(any(test, feature = "test-support"))]
        let start = std::time::Instant::now();
        loop {
            let ctx = {
                let mut st = lock(&project.state);
                if !st.live(session) {
                    return;
                }
                st.deps.identity.again = false;
                st.deps.dirty = false;
                let dir = dep_dir(&st);
                match st.root.clone() {
                    Some(root) => {
                        let roots = st
                            .deps
                            .roots
                            .get_or_insert_with(|| Arc::new(Roots::new(root.clone())))
                            .clone();
                        Some(IdCtx {
                            session,
                            root,
                            roots,
                            dir,
                            hash: st.hash,
                            worktree: worktree_of(&st),
                        })
                    }
                    None => None,
                }
            };
            if let Some(ctx) = ctx {
                self.identity_once(project, &ctx);
            }
            let mut st = lock(&project.state);
            if !st.live(session) {
                return;
            }
            if st.deps.identity.again || st.deps.dirty {
                continue;
            }
            st.deps.identity.running = false;
            #[cfg(any(test, feature = "test-support"))]
            st.deps.identity.took.push(start.elapsed());
            break;
        }
        // The packages queued are batch work.
        self.kick_batch();
    }

    fn identity_once(&self, project: &Arc<Project>, ctx: &IdCtx) {
        let names = read_direct(&ctx.root, &ctx.dir);
        ctx.roots.revalidate();
        let chain = ctx.roots.chain(&ctx.dir);
        // Each direct dependency's package directory and installed
        // manifest's stats; one lockfile read for all their identities.
        let mut found: Vec<(String, PkgDir, Stat)> = Vec::new();
        for name in &names {
            if let Some((pkg, stat)) = locate(&ctx.roots, &chain, name) {
                found.push((name.clone(), pkg, stat));
            }
        }
        let locked = lockfile_entries(&ctx.root, &ctx.dir, &found);
        let table = lock(&self.langs).table.clone();
        let store = &project.deps;
        let mut ready: Vec<Arc<PathTable>> = Vec::new();
        let mut queue: Vec<PkgWork> = Vec::new();
        let mut touches = Vec::new();
        let mut done: HashSet<String> = HashSet::new();
        let mut todo: Vec<(String, PkgDir, Stat)> = found;
        while let Some((name, pkg, stat)) = todo.pop() {
            if !done.insert(name.clone()) {
                continue;
            }
            let Some(work) = package_work(
                &ctx.roots,
                ctx.hash,
                &name,
                pkg,
                stat,
                &locked,
                &ctx.dir,
                &ctx.worktree,
            ) else {
                continue;
            };
            match look_up(store, &table, &work, &mut touches) {
                Found::Ready { table: t, missing } => {
                    // An `@types` package that gave this one's types is
                    // checked the same way.
                    if let Some(types) = &t.types
                        && !done.contains(types)
                        && let Some((pkg, stat)) = locate(&ctx.roots, &chain, types)
                    {
                        todo.push((types.clone(), pkg, stat));
                    }
                    if missing {
                        queue.push(PkgWork {
                            stat_table: Some(t.clone()),
                            ..work
                        });
                    }
                    ready.push(t);
                }
                Found::Walk => {
                    let stat_table =
                        table::latest(store, &work.worktree, &work.origin).map(Arc::new);
                    queue.push(PkgWork { stat_table, ..work });
                }
            }
        }
        // Direct dependencies first, in the manifest's order.
        let order = |w: &PkgWork| names.iter().position(|n| **n == *w.origin.package);
        queue.sort_by_key(|w| order(w).unwrap_or(usize::MAX));
        if !touches.is_empty() {
            store.touch(&touches);
            crate::index::bump(&self.stats.touched, touches.len());
        }
        let mut st = lock(&project.state);
        if !st.live(ctx.session) {
            return;
        }
        let deps = &mut st.deps;
        deps.ran_for = Some(ctx.dir.clone());
        deps.set_ready(&ctx.dir, ready);
        // The package being walked goes on when it is still wanted as it is.
        let walking = deps.walk.as_ref().map(|w| w.work.clone());
        match walking {
            Some(w) if queue.iter().any(|q| q.same_key(&w)) => {
                queue.retain(|q| !q.same_key(&w));
            }
            Some(_) => deps.walk = None,
            None => {}
        }
        deps.queue = queue.into();
        deps.install_map();
    }
}

/// The direct dependencies of the manifest of `dir` (relative to the
/// worktree root), read through the worktree root; none without one.
fn read_direct(root: &SafeRoot, dir: &str) -> Vec<String> {
    let path = if dir.is_empty() {
        "package.json".to_owned()
    } else {
        format!("{dir}/package.json")
    };
    let Some(rel) = SafeRel::new(path.as_bytes()) else {
        return Vec::new();
    };
    let bytes = match root.read(rel, MANIFEST_MAX) {
        Ok(Read::File { bytes, .. }) => bytes,
        _ => return Vec::new(),
    };
    let Some(manifest) = Json::parse(&bytes) else {
        return Vec::new();
    };
    let (names, more) = direct_dependencies(&manifest);
    if more > 0 {
        log(format_args!(
            "{path}: {more} direct dependencies past the first {} are only resolved on demand",
            names.len()
        ));
    }
    names
}

/// A package's directory and its installed `package.json`'s stats.
pub(crate) fn locate(roots: &Roots, chain: &[RelPath], name: &str) -> Option<(PkgDir, Stat)> {
    let pkg = roots.find_package(chain, name)?;
    let file = roots.file(&pkg, "package.json", Ecosystem::Npm)?;
    let meta = roots.stat(&file).ok()?;
    (meta.kind == FileKind::File).then(|| (pkg, Stat::of(&meta.identity())))
}

/// The nearest npm lockfile of `dir` (relative to the worktree root, up to
/// the root): its directory and the opened file.
fn find_lockfile(root: &SafeRoot, dir: &str) -> Option<(String, OpenFile)> {
    let mut at = Some(dir.to_owned());
    while let Some(d) = at {
        for name in LOCKFILES {
            let path = if d.is_empty() {
                name.to_owned()
            } else {
                format!("{d}/{name}")
            };
            if let Some(rel) = SafeRel::new(path.as_bytes())
                && let Ok(Opened::File(file)) = root.open_file(rel)
            {
                return Some((d, file));
            }
        }
        at = if d.is_empty() {
            None
        } else {
            Some(d.rfind('/').map_or(String::new(), |i| d[..i].to_owned()))
        };
    }
    None
}

/// The lockfile's entries of `found`, keyed by package directory relative
/// to the lockfile (its key in `packages`), with that directory; None
/// without a usable npm lockfile.
pub(crate) type LockEntries = Option<(String, HashMap<String, Locked>)>;

fn lockfile_entries(root: &SafeRoot, dir: &str, found: &[(String, PkgDir, Stat)]) -> LockEntries {
    let (lockdir, file) = find_lockfile(root, dir)?;
    let wanted: HashSet<String> = found
        .iter()
        .filter_map(|(_, pkg, _)| lock_key(&lockdir, &pkg.lookup))
        .collect();
    let entries = read_locked(&file, &wanted)?;
    Some((lockdir, entries))
}

/// A package directory's key in the lockfile of `lockdir`.
fn lock_key(lockdir: &str, lookup: &str) -> Option<String> {
    if lockdir.is_empty() {
        return Some(lookup.to_owned());
    }
    lookup
        .strip_prefix(lockdir)
        .and_then(|r| r.strip_prefix('/'))
        .map(str::to_owned)
}

/// A package's work item: its identity is the lockfile's `integrity` when
/// the lockfile has its directory, else the fallback (ADR 0033): the
/// installed manifest's OID and the package directory. None when neither
/// can be had (the manifest unreadable).
#[allow(clippy::too_many_arguments, reason = "the parts of one work item")]
pub(crate) fn package_work(
    roots: &Roots,
    hash: HashKind,
    name: &str,
    pkg: PkgDir,
    stat: Stat,
    locked: &LockEntries,
    subproject: &RelPath,
    worktree: &Arc<str>,
) -> Option<PkgWork> {
    let entry = locked.as_ref().and_then(|(lockdir, entries)| {
        let key = lock_key(lockdir, &pkg.lookup)?;
        entries.get(&key).filter(|l| l.integrity.is_some())
    });
    let (identity, version): (Vec<u8>, Option<String>) = match entry {
        Some(l) => {
            let mut id = b"npm:".to_vec();
            id.extend_from_slice(l.integrity.as_deref().unwrap_or("").as_bytes());
            (id, l.version.clone())
        }
        None => {
            let file = roots.file(&pkg, "package.json", Ecosystem::Npm)?;
            let (bytes, _) = roots.read(&file, MANIFEST_MAX).ok()?;
            let oid: BlobId = hash_blob(hash, &bytes);
            let version = Json::parse(&bytes)
                .and_then(|m| m.get("version").and_then(Json::as_str).map(str::to_owned));
            let mut id = b"pkg:".to_vec();
            id.extend_from_slice(oid.as_bytes());
            id.push(0);
            id.extend_from_slice(pkg.path().as_bytes());
            (id, version)
        }
    };
    let origin = Arc::new(DepOrigin {
        ecosystem: Ecosystem::Npm,
        package: Arc::from(name),
        version: version.map(Arc::from),
        root: DepRoot::Worktree {
            dir: pkg.root.clone(),
        },
    });
    Some(PkgWork {
        origin,
        pkg,
        identity: identity.into(),
        manifest: stat,
        stat_table: None,
        subproject: subproject.clone(),
        worktree: worktree.clone(),
    })
}

/// Whether `work`'s path table is stored and its identity holds (the same
/// key, the installed manifest's stats as recorded and not racy): then
/// its files' units are checked by OID (500 a statement), the hits and the
/// table queued in `touches`.
pub(crate) fn look_up(
    store: &Store,
    langs: &LanguageTable,
    work: &PkgWork,
    touches: &mut Vec<lectern_cache::Unit>,
) -> Found {
    let Some(t) = table::by_key(
        store,
        &work.worktree,
        &work.origin,
        &work.identity,
        &work.pkg.path(),
    ) else {
        return Found::Walk;
    };
    if !vouches(&t.manifest, t.recorded, &work.manifest) {
        return Found::Walk;
    }
    let (units, missing) = check_units(store, langs, &t.files);
    touches.extend(t.unit);
    touches.extend(units);
    Found::Ready {
        table: Arc::new(t),
        missing,
    }
}

/// The stored units of `files`' `Done` OIDs, by language, and whether some
/// are missing.
fn check_units(
    store: &Store,
    langs: &LanguageTable,
    files: &[WalkFile],
) -> (Vec<lectern_cache::Unit>, bool) {
    let done = ExtractStatus::Done.code().unwrap_or(0);
    let mut groups: Vec<(Version, Vec<BlobId>)> = Vec::new();
    for f in files.iter().filter(|f| f.status == done) {
        let (Some(oid), Some(lang)) = (f.oid, dep_lang(langs, &f.path)) else {
            continue;
        };
        let version = Version::of(lang);
        match groups.iter_mut().find(|(v, _)| *v == version) {
            Some((_, oids)) => oids.push(oid),
            None => groups.push((version, vec![oid])),
        }
    }
    let mut units = Vec::new();
    let mut missing = false;
    for (version, oids) in groups {
        let mut seen: HashSet<BlobId> = HashSet::new();
        for chunk in oids.chunks(EXISTS_BATCH) {
            match store.exists(&version, chunk) {
                Some(hits) => {
                    for h in &hits {
                        seen.insert(h.oid);
                        units.push(h.unit);
                    }
                }
                None => missing = true,
            }
        }
        missing |= oids.iter().any(|o| !seen.contains(o));
    }
    // Two files of one content are one unit.
    units.sort();
    units.dedup();
    (units, missing)
}
