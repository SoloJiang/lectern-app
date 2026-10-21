//! Dependencies in queries (「依赖层」「何时建索引」按需, 「查询里的依赖」):
//! the resolution extension point's bare module names resolved into the
//! session's dependency roots and extracted on demand, written to
//! `code_intel.deps` afterwards; a dependency file read with other bytes
//! than its path table recorded updates that table's row.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use lectern_executor::Priority;
use lectern_platform::FileKind;
use lectern_services::code_intel::{DepOrigin, DepRoot, Ecosystem};
use lectern_services::project::BlobId;

use super::Snap;
use crate::deps::json::Json;
use crate::deps::npm::{entry_candidates, spec_candidates, types_package, valid_name};
use crate::deps::{FILE_MAX, MANIFEST_MAX, PkgDir, Roots, Stat, dep_lang};
use crate::extract::{Deadline, ExtractStatus, Extraction, INTERACTIVE_DEADLINE};
use crate::git_access::hash_blob;
use crate::index::batch::Done;
use crate::index::state::{DepSource, Item, Project, Source, Tier};
use crate::index::{Inner, lock};
use crate::resolve::DepTarget;
use crate::store::{UnitMeta, Version};

/// A dependency file a query extracted, to be written to `code_intel.deps`
/// (its edges with it) by a `Background` task.
#[derive(Clone)]
pub(crate) struct DepScanned {
    pub target: DepTarget,
    pub roots: Arc<Roots>,
    /// The OID of the bytes extracted.
    pub oid: BlobId,
    pub extraction: Extraction,
    pub lang: crate::extract::Lang,
}

/// A bare specifier's package name and the subpath after it:
/// `@scope/x/a/b` → (`@scope/x`, `a/b`), `lodash/fp` → (`lodash`, `fp`).
fn split_specifier(spec: &str) -> Option<(&str, &str)> {
    let cut = if spec.starts_with('@') {
        let first = spec.find('/')?;
        spec[first + 1..].find('/').map(|i| first + 1 + i)
    } else {
        spec.find('/')
    };
    let (name, sub) = match cut {
        Some(i) => (&spec[..i], &spec[i + 1..]),
        None => (spec, ""),
    };
    valid_name(name).then_some((name, sub))
}

impl Inner {
    /// `ResolveContext::dependency` for a query of `snap`: the package by
    /// the lookup chain of `subproject`, then its `@types` package when it
    /// has no declaration entry; the entry, or the subpath's file by the
    /// relative rules. Only through the session's roots.
    pub(crate) fn resolve_dependency(
        &self,
        snap: &Snap,
        specifier: &str,
        subproject: &str,
    ) -> Option<DepTarget> {
        let roots = snap.dep_roots.as_ref()?;
        let (name, sub) = split_specifier(specifier)?;
        let chain = roots.chain(subproject);
        let mut names = vec![name.to_owned()];
        names.extend(types_package(name));
        for name in names {
            let Some(pkg) = roots.find_package(&chain, &name) else {
                continue;
            };
            let manifest = roots
                .file(&pkg, "package.json", Ecosystem::Npm)
                .and_then(|f| roots.read(&f, MANIFEST_MAX).ok())
                .and_then(|(bytes, _)| Json::parse(&bytes));
            let candidates = if sub.is_empty() {
                manifest.as_ref().map(entry_candidates).unwrap_or_default()
            } else {
                spec_candidates("", &format!("./{sub}"))
            };
            let file = candidates.iter().find_map(|rel| {
                let file = roots.file(&pkg, rel, Ecosystem::Npm)?;
                roots
                    .stat(&file)
                    .is_ok_and(|m| m.kind == FileKind::File)
                    .then_some(file)
            });
            let Some(file) = file else {
                continue;
            };
            let version = manifest
                .as_ref()
                .and_then(|m| m.get("version").and_then(Json::as_str))
                .map(Arc::from);
            return Some(DepTarget {
                origin: Arc::new(origin(&pkg, &name, version)),
                file,
            });
        }
        None
    }

    /// `ResolveContext::dependency_extraction` for a query of `snap`: a
    /// stored `Done` unit of the bytes' OID, else extracted now (by `until`
    /// at the latest, the background size limit), queued in `written`.
    pub(crate) fn extract_dependency(
        &self,
        snap: &Snap,
        target: &DepTarget,
        until: Instant,
        cancel: &Arc<AtomicBool>,
        written: &mut Vec<DepScanned>,
    ) -> Option<(Arc<Extraction>, BlobId)> {
        let roots = snap.dep_roots.as_ref()?;
        let path = target.file.path();
        let lang = dep_lang(&snap.table, &path)?.clone();
        crate::phases::count("file reads", 1);
        let (bytes, _) = roots.read(&target.file, FILE_MAX).ok()?;
        let oid = hash_blob(snap.hash, &bytes);
        let version = Version::of(&lang);
        let stored = snap
            .project
            .deps
            .exists(&version, &[oid])
            .unwrap_or_default()
            .into_iter()
            .next();
        if let Some(f) = stored {
            if f.status != ExtractStatus::Done {
                return None;
            }
            let extraction = snap.project.deps.load(f.unit)?;
            snap.project.deps.touch(&[f.unit]);
            return Some((Arc::new(extraction), oid));
        }
        let now = self.watchdog.clock().now();
        let at = (now + INTERACTIVE_DEADLINE).min(until);
        let deadline = Deadline::at(&self.watchdog, at, FILE_MAX as usize).with_specifiers();
        let extract = lock(&self.extract).clone();
        let extraction = extract(&lang, &bytes, &deadline, cancel);
        let settles = matches!(
            extraction.status,
            ExtractStatus::Done | ExtractStatus::Binary | ExtractStatus::TooLarge
        );
        if settles {
            written.push(DepScanned {
                target: target.clone(),
                roots: roots.clone(),
                oid,
                extraction: extraction.clone(),
                lang,
            });
        }
        (extraction.status == ExtractStatus::Done).then(|| (Arc::new(extraction), oid))
    }

    /// Writes a query's dependency extractions in a `Background` task, as
    /// the dependency tier would (no walk: their edges are stored, not
    /// followed); a file of a ready path table read with other bytes than
    /// recorded updates its row and the dependency map (「读取核对」).
    pub(crate) fn store_dep_scanned(
        &self,
        project: &Arc<Project>,
        session: u64,
        scanned: Vec<DepScanned>,
    ) {
        if scanned.is_empty() {
            return;
        }
        let Some(me) = self.arc() else {
            return;
        };
        let p = project.clone();
        self.host
            .spawn_busy("store deps", Priority::Background, move || {
                me.commit_dep_scanned(&p, session, scanned);
            });
    }

    fn commit_dep_scanned(
        self: &Arc<Self>,
        project: &Arc<Project>,
        session: u64,
        scanned: Vec<DepScanned>,
    ) {
        let ctx = {
            let st = lock(&project.state);
            if !st.live(session) {
                return;
            }
            crate::index::batch::ctx_of(&st)
        };
        let mut changed = Vec::new();
        let done: Vec<Done> = scanned
            .into_iter()
            .map(|s| {
                let stat = s
                    .roots
                    .stat(&s.target.file)
                    .map(|m| Stat::of(&m.identity()))
                    .unwrap_or(Stat {
                        dev: 0,
                        ino: 0,
                        size: 0,
                        mtime: 0,
                        ctime: 0,
                    });
                changed.push((s.target.file.path(), s.oid, stat));
                Done {
                    item: Item {
                        path: Arc::from(s.target.file.path()),
                        oid: s.oid,
                        lang: s.lang,
                        source: Source::Dep(Arc::new(DepSource {
                            file: s.target.file,
                            roots: s.roots,
                            walk: None,
                            stat,
                            rel: String::new(),
                        })),
                        tier: Tier::Current,
                    },
                    key: s.oid,
                    check: None,
                    extraction: s.extraction,
                    flag: Arc::new(AtomicBool::new(false)),
                    meta: UnitMeta::whole(true),
                }
            })
            .collect();
        self.commit_deps(project, &ctx, done, false);
        self.dep_files_read(project, session, &changed);
    }

    /// Files of ready path tables read with these OIDs (「读取核对」): a row
    /// whose OID differs is updated, its table written again and the
    /// dependency map replaced. The ready tables are headers: a table with
    /// a read file under its directory has its rows read again from the
    /// store. Blocking; a `Background` task.
    fn dep_files_read(
        &self,
        project: &Arc<Project>,
        session: u64,
        read: &[(String, BlobId, Stat)],
    ) {
        let headers = {
            let st = lock(&project.state);
            if !st.live(session) {
                return;
            }
            st.deps.ready.clone()
        };
        let mut updated = Vec::new();
        for (sub, headers) in &headers {
            for h in headers {
                let prefix = format!("{}/", h.dir);
                if !read.iter().any(|(path, _, _)| path.starts_with(&prefix)) {
                    continue;
                }
                let t = if h.loaded {
                    (**h).clone()
                } else {
                    match crate::deps::table::reload(&project.deps, h) {
                        Some(t) => t,
                        None => continue,
                    }
                };
                let mut table: Option<crate::deps::PathTable> = None;
                for (path, oid, stat) in read {
                    let Some(i) = t.files.iter().position(|f| &*f.path == path) else {
                        continue;
                    };
                    if t.files[i].oid == Some(*oid) {
                        continue;
                    }
                    let tbl = table.get_or_insert_with(|| t.clone());
                    tbl.files[i].oid = Some(*oid);
                    tbl.files[i].stat = *stat;
                    tbl.files[i].status = ExtractStatus::Done.code().unwrap_or(0);
                }
                if let Some(tbl) = table {
                    updated.push((sub.clone(), tbl));
                }
            }
        }
        if updated.is_empty() {
            return;
        }
        {
            let _turn = lock(&project.deps_commits);
            let mut write = project.deps.write();
            for (_, t) in &updated {
                crate::deps::table::write(&mut write, t);
            }
            let _ = write.commit();
        }
        let mut st = lock(&project.state);
        if !st.live(session) {
            return;
        }
        for (sub, t) in updated {
            st.deps.add_ready(&sub, Arc::new(t));
        }
        let input = crate::deps::MapInput::of(&st);
        drop(st);
        crate::deps::install_map(project, input);
    }
}

/// A package's origin for a target found on demand.
fn origin(pkg: &PkgDir, name: &str, version: Option<Arc<str>>) -> DepOrigin {
    DepOrigin {
        ecosystem: Ecosystem::Npm,
        package: Arc::from(name),
        version,
        root: DepRoot::Worktree {
            dir: pkg.root.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::split_specifier;

    #[test]
    fn specifiers_split_into_package_and_subpath() {
        assert_eq!(split_specifier("react"), Some(("react", "")));
        assert_eq!(split_specifier("lodash/fp"), Some(("lodash", "fp")));
        assert_eq!(
            split_specifier("@types/node/fs"),
            Some(("@types/node", "fs"))
        );
        assert_eq!(split_specifier("@scope/x"), Some(("@scope/x", "")));
        assert_eq!(split_specifier("@scope"), None);
        assert_eq!(split_specifier("../x"), None);
    }
}
