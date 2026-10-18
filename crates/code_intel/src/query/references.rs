//! Find references and callers (「查找引用与调用方」): the definition's own
//! file, the indexed files whose reference names include the name (`ci_ref`),
//! then the time-boxed search; each file parsed, each reference of the name
//! bound by the locals and the resolution extension point. A reference that
//! resolves back to the definition is a caller; one that resolves elsewhere
//! is dropped; the rest are same-name references.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lectern_cache::Unit;
use lectern_services::code_intel::{
    DefinitionRef, Location, QueryError, QueryState, RefSyntax, Reference, Symbol, TextOrigin,
    TextRef,
};
use lectern_services::project::{BlobId, RelPath, WorktreeKey};

use super::definition::{LocKey, loc_key};
use super::rank::Ranker;
use super::search::{FileDeadline, Searched};
use super::{Run, SEARCH_FOR, Snap};
use crate::extract::{ExtractStatus, Extraction, Lang, compatible};
use crate::index::Inner;
use crate::resolve::ResolveContext;
use crate::site::{ParseError, ParsedFile};
use crate::store::Version;

/// The first batch goes out after this many files…
const FIRST_FILES: usize = 4;
/// …or this long after the query started.
const FIRST_AFTER: Duration = Duration::from_millis(150);
/// Later batches: every this many files…
const BATCH_FILES: usize = 32;
/// …or this long.
const BATCH_AFTER: Duration = Duration::from_millis(100);

/// One file to read for references.
struct File {
    worktree: Option<WorktreeKey>,
    path: RelPath,
    origin: TextOrigin,
    lang: Lang,
}

/// The query's results so far and its batching.
struct Found {
    callers: Vec<Reference>,
    same_name: Vec<Reference>,
    delivered: HashSet<LocKey>,
    /// (path, OID) of every file parsed: each one once.
    parsed: HashSet<(RelPath, BlobId)>,
    files: usize,
    since: usize,
    last: Option<Instant>,
    changed: bool,
}

impl Inner {
    pub(crate) fn run_references(self: &Arc<Self>, run: &Run, snap: &Snap, of: &DefinitionRef) {
        let Some(lang) = snap
            .table
            .get(&of.lang.0)
            .filter(|l| !snap.disabled.contains(l.name()))
            .cloned()
        else {
            self.publish(run.id, |r| {
                r.state = QueryState::Failed(QueryError::UnsupportedLanguage);
            });
            return;
        };
        let symbol = Symbol {
            name: of.name.clone(),
            lang: of.lang.clone(),
            syntax: RefSyntax::Plain,
            defines: Some(of.clone()),
        };
        let l = &of.location;
        let ranker = self.ranker(snap, Some(&l.path), Some((l.origin, l.oid)), Some(&of.lang));
        let own = TextRef {
            worktree: l.worktree.clone(),
            path: l.path.clone(),
            oid: l.oid,
            origin: l.origin,
        };
        // The definition's own file, read as its location says.
        let bytes = match self.read_text(snap, &own) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.publish(run.id, |r| {
                    r.symbol = Some(symbol);
                    r.state = QueryState::Failed(error);
                });
                return;
            }
        };
        self.publish(run.id, |r| r.symbol = Some(symbol));
        self.escalate_references(run.id, snap, of);
        let mut found = Found {
            callers: Vec::new(),
            same_name: Vec::new(),
            delivered: HashSet::new(),
            parsed: HashSet::new(),
            files: 0,
            since: 0,
            last: None,
            changed: false,
        };
        let file = File {
            worktree: l.worktree.clone(),
            path: l.path.clone(),
            origin: l.origin,
            lang: lang.clone(),
        };
        self.references_in(
            run, snap, of, &ranker, &file, l.oid, bytes, None, &mut found,
        );

        let mut units: Vec<Unit> = Vec::new();
        let mut cut = snap.hinted;
        // 1. Indexed files whose reference names include the name.
        if !of.local
            && !self.indexed_references(run, snap, of, &ranker, &lang, &mut found, &mut units)
        {
            return;
        }
        if run.stopped(snap) {
            return;
        }
        // 3. Unindexed files of the sub-project: the search.
        let completeness = if of.local {
            Some(lectern_services::code_intel::Completeness::Complete)
        } else {
            self.run_after_lookup(snap);
            let skip = found.parsed.clone();
            let scope = snap.search_scope(&l.path, &lang, &skip);
            // Files the scope takes as indexed whose unit was written after
            // the lookup: their references are read now.
            if snap.hinted_since(&mut cut)
                && !self.indexed_references(run, snap, of, &ranker, &lang, &mut found, &mut units)
            {
                return;
            }
            let name = of.name.clone();
            let result = self.search(
                run,
                snap,
                &scope,
                &lang,
                &name,
                run.started + SEARCH_FOR,
                |eff, key, bytes, until| {
                    if found.parsed.contains(&(eff.path.clone(), key)) {
                        return Searched::Done;
                    }
                    let Some(file_lang) = snap.table.get(&eff.lang.name().0).cloned() else {
                        return Searched::Done;
                    };
                    let file = File {
                        worktree: snap.worktree.clone(),
                        path: eff.path.clone(),
                        origin: TextOrigin::Worktree,
                        lang: file_lang,
                    };
                    self.references_in(
                        run,
                        snap,
                        of,
                        &ranker,
                        &file,
                        key,
                        bytes.into(),
                        Some(until),
                        &mut found,
                    )
                },
            );
            // The counts of `Partial` read hints too.
            if result.is_some()
                && snap.hinted_since(&mut cut)
                && !self.indexed_references(run, snap, of, &ranker, &lang, &mut found, &mut units)
            {
                return;
            }
            result
        };
        let Some(completeness) = completeness else {
            return;
        };
        let (callers, same_name) = (found.callers.clone(), found.same_name.clone());
        self.publish(run.id, |r| {
            r.callers = callers.into();
            r.same_name = same_name.into();
            r.state = QueryState::Done(completeness);
        });
        units.sort();
        units.dedup();
        snap.project.store.touch(&units);
    }

    /// The indexed files whose reference names include the name (`ci_ref`),
    /// nearest first, each not parsed yet read and parsed. Run again when
    /// hints were installed after the lookup ([`Snap::hinted_since`]).
    /// False when the query stopped.
    #[allow(
        clippy::too_many_arguments,
        reason = "the query's snapshot, the definition and what it adds to"
    )]
    fn indexed_references(
        &self,
        run: &Run,
        snap: &Snap,
        of: &DefinitionRef,
        ranker: &Ranker<'_>,
        lang: &Lang,
        found: &mut Found,
        units: &mut Vec<Unit>,
    ) -> bool {
        let versions: Vec<Version> = snap
            .table
            .iter()
            .filter(|x| !snap.disabled.contains(x.name()))
            .filter(|x| compatible(&snap.table, x.name(), lang.name()))
            .map(Version::of)
            .collect();
        let stored = snap
            .project
            .store
            .refs_by_name(&of.name, &versions)
            .unwrap_or_default();
        let mut jobs = Vec::new();
        for s in &stored {
            for eff in snap.map.paths(&s.oid, &s.lang.0) {
                jobs.push((eff, s.unit));
            }
        }
        jobs.sort_by(|(a, _), (b, _)| {
            (ranker.file_order(&a.path), &a.path).cmp(&(ranker.file_order(&b.path), &b.path))
        });
        for (eff, unit) in jobs {
            if run.stopped(snap) {
                return false;
            }
            if found.parsed.contains(&(eff.path.clone(), eff.oid)) {
                continue;
            }
            let Some(bytes) = self.read_file(snap, &eff.path) else {
                continue;
            };
            let key = self.key_of(snap, &eff.path, &bytes, eff.oid);
            if key == eff.oid {
                units.push(unit);
            }
            let Some(file_lang) = snap.table.get(&eff.lang.name().0).cloned() else {
                continue;
            };
            let file = File {
                worktree: snap.worktree.clone(),
                path: eff.path.clone(),
                origin: TextOrigin::Worktree,
                lang: file_lang,
            };
            self.references_in(run, snap, of, ranker, &file, key, bytes.into(), None, found);
        }
        true
    }

    /// Parses one file and sorts every reference of the name in it, with
    /// the per-file deadline cut at `until` (the search's end). A parse the
    /// query's time cut short, or a cancelled one, is `Unsearched`.
    #[allow(
        clippy::too_many_arguments,
        reason = "one file's part of a query: its snapshot, the file and what it adds to"
    )]
    fn references_in(
        &self,
        run: &Run,
        snap: &Snap,
        of: &DefinitionRef,
        ranker: &Ranker<'_>,
        file: &File,
        oid: BlobId,
        bytes: Arc<[u8]>,
        until: Option<Instant>,
        found: &mut Found,
    ) -> Searched {
        if found.parsed.contains(&(file.path.clone(), oid)) {
            return Searched::Done;
        }
        let clock = self.watchdog.clock().clone();
        let deadline = FileDeadline::new(clock.now(), until);
        let parsed = match ParsedFile::parse(&file.lang, bytes, &*clock, deadline.at, &run.cancel) {
            Ok(parsed) => parsed,
            Err(error) => {
                let status = match error {
                    ParseError::TimedOut => ExtractStatus::TimedOut,
                    ParseError::Cancelled => ExtractStatus::Cancelled,
                    ParseError::Unsupported => ExtractStatus::Unsupported,
                };
                if !deadline.settles(status) {
                    return Searched::Unsearched;
                }
                found.parsed.insert((file.path.clone(), oid));
                return Searched::Done;
            }
        };
        found.parsed.insert((file.path.clone(), oid));
        let def = &of.location;
        let own = file.path == def.path && file.origin == def.origin && oid == def.oid;
        let rules = self.resolver(file.lang.name());
        let stored = |path: &str| -> Option<Extraction> {
            let eff = snap.map.effective(path)?;
            let lang = snap.table.get(&eff.lang.name().0)?;
            let found = snap
                .project
                .store
                .exists(&Version::of(lang), &[eff.oid])?
                .into_iter()
                .find(|f| f.status == ExtractStatus::Done)?;
            snap.project.store.load(found.unit)
        };
        let ctx = ResolveContext {
            map: &snap.map,
            files: snap.map.base.files(),
            packages: Some(&snap.packages),
            extraction: &stored,
        };
        let name = of.name.as_bytes();
        for r in parsed.references() {
            if parsed.text(r) != name {
                continue;
            }
            let Some(site) = parsed.site_at(r.at) else {
                continue;
            };
            if site.defines.is_some() {
                continue;
            }
            let caller = match &site.binding {
                // The locals resolve it: to this definition (the same file,
                // the same position), or to another one.
                Some(b) => {
                    if own && b.at.line == def.line && b.at.column == def.column {
                        true
                    } else {
                        continue;
                    }
                }
                // A local is visible in its own scope only: what the locals
                // do not bind there is not about it.
                None if of.local => continue,
                None => match rules.bind(&site, &parsed, &ctx) {
                    Some(bound) => {
                        let b = &bound.definition;
                        if b.name == of.name
                            && b.location.path == def.path
                            && b.location.line == def.line
                            && b.location.column == def.column
                        {
                            true
                        } else {
                            continue;
                        }
                    }
                    None => false,
                },
            };
            let location = Location {
                worktree: file.worktree.clone(),
                path: file.path.clone(),
                origin: file.origin,
                oid,
                line: r.at.line,
                column: r.at.column,
                end_line: r.at.line,
            };
            if !found.delivered.insert(loc_key(&location)) {
                continue;
            }
            let reference = Reference {
                location,
                syntax: site.syntax,
                enclosing: parsed.enclosing(r.start).map(|d| Arc::from(&*d.name)),
                in_review: ranker.in_review(&file.path),
            };
            found.changed = true;
            if caller {
                found.callers.push(reference);
            } else {
                found.same_name.push(reference);
            }
        }
        found.files += 1;
        found.since += 1;
        let now = clock.now();
        let due = match found.last {
            None => {
                found.files >= FIRST_FILES
                    || now.saturating_duration_since(run.started) >= FIRST_AFTER
            }
            Some(last) => {
                found.since >= BATCH_FILES || now.saturating_duration_since(last) >= BATCH_AFTER
            }
        };
        if due && found.changed {
            found.since = 0;
            found.last = Some(now);
            found.changed = false;
            let (callers, same_name) = (found.callers.clone(), found.same_name.clone());
            self.publish(run.id, |r| {
                r.callers = callers.into();
                r.same_name = same_name.into();
            });
        }
        Searched::Done
    }
}
