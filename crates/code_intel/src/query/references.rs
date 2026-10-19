//! Find references and callers (「查找引用与调用方」): the definition's own
//! file, the indexed files whose reference names include the name (`ci_ref`),
//! then the time-boxed search; each file parsed, each reference of the name
//! bound by the locals and the resolution extension point. A reference that
//! resolves back to the definition is a caller; one that resolves elsewhere
//! is dropped; the rest are same-name references. The completeness is the
//! accounting's (`account`): every file of the language group, indexed,
//! searched, or not.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lectern_cache::Unit;
use lectern_services::code_intel::{
    Completeness, DefinitionRef, Location, QueryError, QueryState, RefSyntax, Reference, Symbol,
    TextOrigin, TextRef,
};
use lectern_services::project::{BlobId, RelPath, WorktreeKey};

use super::account::{Account, Reason};
use super::definition::{LocKey, loc_key};
use super::rank::Ranker;
use super::search::{FileDeadline, Searched};
use super::{Run, SEARCH_FOR, Snap, Step, ViewCheck};
use crate::extract::{ExtractStatus, Extraction, INTERACTIVE_MAX_LEN, Lang, compatible};
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
    /// Those of them whose parse failed for good (their own deadline, their
    /// grammar), and why: not searched however often they come up.
    failed: HashMap<(RelPath, BlobId), Reason>,
    files: usize,
    since: usize,
    last: Option<Instant>,
    changed: bool,
}

impl Found {
    /// What parsing a file already parsed gave.
    fn outcome(&self, path: &RelPath, oid: BlobId) -> Searched {
        match self.failed.get(&(path.clone(), oid)) {
            Some(reason) => Searched::Unsearched(*reason),
            None => Searched::Done,
        }
    }
}

/// Records what reading one file gave.
fn record(account: &mut Account, path: &RelPath, oid: BlobId, searched: Searched) {
    match searched {
        Searched::Done => account.searched(path, oid),
        Searched::Unsearched(reason) => account.unsearched(path, oid, reason),
    }
}

impl Inner {
    /// Whether the definition `of` is local, by the parse of its own file
    /// `bytes` (tier 1's limits) at its position: the definition there with
    /// its name. `of.local` as given when that parse fails or has no such
    /// definition.
    fn own_locality(
        &self,
        lang: &Lang,
        bytes: &Arc<[u8]>,
        of: &DefinitionRef,
        cancel: &Arc<std::sync::atomic::AtomicBool>,
    ) -> bool {
        let l = &of.location;
        let clock = self.watchdog.clock().clone();
        let deadline = clock.now() + crate::extract::INTERACTIVE_DEADLINE;
        let site = lectern_services::code_intel::Position {
            line: l.line,
            column: l.column,
        };
        let Ok(parsed) = ParsedFile::parse_at(lang, bytes.clone(), &*clock, deadline, cancel, site)
        else {
            return of.local;
        };
        parsed
            .defs_with_local()
            .find(|(d, _)| d.line == l.line && d.column == l.column && *d.name == *of.name)
            .map_or(of.local, |(_, local)| local)
    }

    pub(crate) fn run_references(self: &Arc<Self>, run: &Run, snap: &Snap, of: &DefinitionRef) {
        let Some(lang) = snap
            .table
            .get(&of.lang.0)
            .filter(|l| !snap.disabled.contains(l.name()))
            .cloned()
        else {
            self.publish_in(snap, run.id, |r| {
                r.state = QueryState::Failed(QueryError::UnsupportedLanguage);
            });
            return;
        };
        let l = &of.location;
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
                let symbol = Symbol {
                    name: of.name.clone(),
                    lang: of.lang.clone(),
                    syntax: RefSyntax::Plain,
                    defines: Some(of.clone()),
                };
                self.publish_in(snap, run.id, |r| {
                    r.symbol = Some(symbol);
                    r.state = QueryState::Failed(error);
                });
                return;
            }
        };
        // Whether the definition is local, from its own file's parse,
        // whoever made `of` (a `Named` result, a provider, a jump): what to
        // scan depends on it (「查找引用与调用方」1).
        let of = &DefinitionRef {
            local: self.own_locality(&lang, &bytes, of, &run.cancel),
            ..of.clone()
        };
        let l = &of.location;
        let symbol = Symbol {
            name: of.name.clone(),
            lang: of.lang.clone(),
            syntax: RefSyntax::Plain,
            defines: Some(of.clone()),
        };
        let ranker = self.ranker(snap, Some(&l.path), Some((l.origin, l.oid)), Some(&of.lang));
        self.publish_in(snap, run.id, |r| r.symbol = Some(symbol));
        self.escalate_references(run.id, snap, of);
        let mut found = Found {
            callers: Vec::new(),
            same_name: Vec::new(),
            delivered: HashSet::new(),
            parsed: HashSet::new(),
            failed: HashMap::new(),
            files: 0,
            since: 0,
            last: None,
            changed: false,
        };
        let mut account = Account::default();
        let file = File {
            worktree: l.worktree.clone(),
            path: l.path.clone(),
            origin: l.origin,
            lang: lang.clone(),
        };
        let own = self.references_in(
            run, snap, of, &ranker, &file, l.oid, bytes, None, &mut found,
        );
        // Read by OID as an object, it is the map's file only when the map
        // has that content at the path (`Snap::finish` checks).
        record(&mut account, &l.path, l.oid, own);

        let mut units: Vec<Unit> = Vec::new();
        // The view the lookups are made under (「查询的视图」).
        let mut view = snap.view;
        // 1. Indexed files whose reference names include the name.
        if !of.local
            && !self.indexed_references(
                run,
                snap,
                of,
                &ranker,
                &lang,
                &mut found,
                &mut account,
                &mut units,
            )
        {
            return;
        }
        if run.stopped(snap) {
            return;
        }
        self.step_hook(Step::Lookup, snap);
        // References go out from the first files on: a view that moved
        // ends the query `Stale` (no hint is read yet, so growth waits for
        // the scope).
        if matches!(
            self.check_view(snap, &view),
            ViewCheck::Evicted | ViewCheck::Ended
        ) {
            return self.end_stale(snap, run.id);
        }
        // 3. Unindexed files of the sub-project: the search.
        // A local is visible in its own scope only: its file is all there is
        // (「查找引用与调用方」1), complete once that file was searched.
        let completeness = if of.local {
            Some(match own {
                Searched::Done => Completeness::Complete,
                Searched::Unsearched(_) => Completeness::Partial {
                    unsearched: 1,
                    outside: 0,
                },
            })
        } else {
            let skip = found.parsed.clone();
            let scope = snap.search_scope(&l.path, &lang, &skip, &mut account);
            self.step_hook(Step::Hints, snap);
            // Files the scope takes as indexed whose unit was written after
            // the lookup: their references are read now.
            match self.check_view(snap, &view) {
                ViewCheck::Same => {}
                ViewCheck::Grew(hinted) => {
                    view.hinted = hinted;
                    if !self.indexed_references(
                        run,
                        snap,
                        of,
                        &ranker,
                        &lang,
                        &mut found,
                        &mut account,
                        &mut units,
                    ) {
                        return;
                    }
                }
                ViewCheck::Evicted | ViewCheck::Ended => return self.end_stale(snap, run.id),
            }
            let name = of.name.clone();
            let searched = self.search(
                run,
                snap,
                &scope,
                &name,
                run.started + SEARCH_FOR,
                &mut account,
                |eff, key, bytes, until| {
                    if found.parsed.contains(&(eff.path.clone(), key)) {
                        return found.outcome(&eff.path, key);
                    }
                    let Some(file_lang) = snap.table.get(&eff.lang.name().0).cloned() else {
                        return Searched::Unsearched(Reason::Unreadable);
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
            // Every file of the language group, classified once; the walk
            // reads hints, so the view is checked after it.
            let part = |l: &Lang| snap.in_group(l, &lang);
            let walked = if searched {
                self.walk(
                    run,
                    snap,
                    &mut account,
                    &scope.dir,
                    &part,
                    Some(lang.name()),
                )
            } else {
                None
            };
            if let Some(walked) = walked {
                match self.check_view(snap, &view) {
                    ViewCheck::Same => {}
                    ViewCheck::Grew(_) => {
                        if !self.indexed_references(
                            run,
                            snap,
                            of,
                            &ranker,
                            &lang,
                            &mut found,
                            &mut account,
                            &mut units,
                        ) {
                            return;
                        }
                    }
                    ViewCheck::Evicted | ViewCheck::Ended => {
                        return self.end_stale(snap, run.id);
                    }
                }
                Some(snap.finish(&account, walked))
            } else {
                None
            }
        };
        let Some(completeness) = completeness else {
            return;
        };
        let (callers, same_name) = (found.callers.clone(), found.same_name.clone());
        self.publish_in(snap, run.id, |r| {
            r.callers = callers.into();
            r.same_name = same_name.into();
            r.state = QueryState::Done(completeness);
        });
        units.sort();
        units.dedup();
        snap.project.store.touch(&units);
    }

    /// The indexed files whose reference names include the name (`ci_ref`),
    /// nearest first, each not read yet read and parsed, what it learned in
    /// `account`. Run again when hints were installed after the lookup
    /// (`ViewCheck::Grew`). False when the query stopped.
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
        account: &mut Account,
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
        // `file_order` scans the review set, the sub-projects and their
        // dependencies: once per file, not per comparison.
        jobs.sort_by_cached_key(|(eff, _)| (ranker.file_order(&eff.path), eff.path.clone()));
        for (eff, unit) in jobs {
            if run.stopped(snap) {
                return false;
            }
            if found.parsed.contains(&(eff.path.clone(), eff.oid))
                || account.outcome(&eff.path, eff.oid).is_some()
            {
                continue;
            }
            // Its unit was extracted from these bytes, under at most the
            // interactive limit (tier 1): read under it. Changed since the
            // snapshot (its unit is of other content), too large or
            // unreadable: not searched.
            let bytes = match self.read_content(snap, &eff, INTERACTIVE_MAX_LEN) {
                Ok(bytes) => bytes,
                Err(reason) => {
                    account.unsearched(&eff.path, eff.oid, reason);
                    continue;
                }
            };
            units.push(unit);
            let Some(file_lang) = snap.table.get(&eff.lang.name().0).cloned() else {
                account.unsearched(&eff.path, eff.oid, Reason::Unreadable);
                continue;
            };
            let file = File {
                worktree: snap.worktree.clone(),
                path: eff.path.clone(),
                origin: TextOrigin::Worktree,
                lang: file_lang,
            };
            let searched = self.references_in(
                run,
                snap,
                of,
                ranker,
                &file,
                eff.oid,
                bytes.into(),
                None,
                found,
            );
            record(account, &eff.path, eff.oid, searched);
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
            return found.outcome(&file.path, oid);
        }
        let clock = self.watchdog.clock().clone();
        let def = &of.location;
        let own = file.path == def.path && file.origin == def.origin && oid == def.oid;
        // The definition's own file is the text the query is about: tier 1's
        // limit, as a jump's current text. The others, the background's.
        let deadline = if own {
            FileDeadline::interactive(clock.now(), until)
        } else {
            FileDeadline::new(clock.now(), until)
        };
        let parsed = ParsedFile::parse(&file.lang, bytes, &*clock, deadline.at, &run.cancel);
        #[cfg(test)]
        let parsed = match &*crate::index::lock(&self.parse_timeout) {
            Some(path) if **path == *file.path => Err(ParseError::TimedOut),
            _ => parsed,
        };
        let parsed = match parsed {
            Ok(parsed) => parsed,
            Err(error) => {
                let status = match error {
                    ParseError::TimedOut => ExtractStatus::TimedOut,
                    ParseError::Cancelled => ExtractStatus::Cancelled,
                    ParseError::Unsupported => ExtractStatus::Unsupported,
                };
                // Parsed once: a failure that settles the file (its own
                // deadline, its grammar) is not tried again in this query.
                let reason = deadline.reason(status).unwrap_or(Reason::Unreadable);
                if deadline.settles(status) {
                    found.parsed.insert((file.path.clone(), oid));
                    found.failed.insert((file.path.clone(), oid), reason);
                }
                return Searched::Unsearched(reason);
            }
        };
        found.parsed.insert((file.path.clone(), oid));
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
            self.publish_in(snap, run.id, |r| {
                r.callers = callers.into();
                r.same_name = same_name.into();
            });
        }
        Searched::Done
    }
}
