//! Jump to definition and `Named` (「跳转到定义」). Steps, each run only
//! while the steps before found no candidate with binding evidence:
//!
//! 1. the current text, verified against its OID and parsed: a local
//!    definition the binding rules prove visible is `Bound(Local)`;
//! 2. the imports the resolution extension point gives (none in #17);
//! 3. the index: definitions by name, filtered by language group,
//!    visibility and syntax kind, only OIDs the map has;
//! 4. the time-boxed search of the current sub-project's unindexed files.
//!
//! Steps 1–3 are the first batch; the search appends. Results only grow.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use lectern_cache::Unit;
use lectern_services::code_intel::{
    Binding, Candidate, CandidateSource, CandidateTier, Completeness, DefinitionRef, Evidence,
    Location, Position, QueryError, QueryId, QueryState, Symbol, TextOrigin, TextRef,
};
use lectern_services::project::{BlobId, RelPath};

use super::rank::{Ranker, visible};
use super::search::{FileDeadline, Searched};
use super::text::FileRead;
use super::{Run, SEARCH_FOR, Snap, Step, View, ViewCheck};
use crate::extract::{
    BACKGROUND_MAX_LEN, Deadline, Def, ExtractStatus, Extraction, INTERACTIVE_DEADLINE, Lang,
    Visibility, compatible,
};
use crate::index::{Inner, Scanned};
use crate::map::Effective;
use crate::resolve::{CandidateDraft, ResolveContext};
use crate::site::{LocalBinding, ParseError, ParsedFile, Site, accepts};
use crate::store::Version;
use crate::{DefKind, LangName};

/// A delivered location: (origin, path, OID, line, column).
pub(crate) type LocKey = (TextOrigin, RelPath, BlobId, u32, u32);

pub(crate) fn loc_key(l: &Location) -> LocKey {
    (l.origin, l.path.clone(), l.oid, l.line, l.column)
}

/// What step 2's extractions add: units used, extractions to write, and
/// each file's extraction by (path, OID of the bytes read).
type Pending = (
    Vec<Unit>,
    Vec<Scanned>,
    HashMap<(RelPath, BlobId), Option<Extraction>>,
);

/// One file's extraction for a query ([`Inner::file_extraction`]).
pub(crate) enum FileExtraction {
    /// `Done`: its definitions.
    Defs(Extraction),
    /// Known to give none: stored or extracted with another status that
    /// settles the file (`FileDeadline::settles`), or a language not in
    /// the current table.
    Nothing,
    /// The query's time ran out or it was cancelled: nothing is known, not
    /// stored; the file counts as unsearched.
    Unsearched,
}

impl FileExtraction {
    fn defs(self) -> Option<Extraction> {
        match self {
            FileExtraction::Defs(e) => Some(e),
            FileExtraction::Nothing | FileExtraction::Unsearched => None,
        }
    }
}

/// Steps 1–3 of a jump: what the first batch shows, and what the search
/// goes on from. A prefetch keeps one.
#[derive(Clone)]
pub(crate) struct Prepared {
    pub symbol: Symbol,
    pub lang: Lang,
    pub site: Site,
    pub candidates: Vec<Candidate>,
    pub delivered: HashSet<LocKey>,
    /// A step found candidates with binding evidence: no step after it runs.
    pub settled: bool,
    /// The current text: step 1 listed its definitions, the search skips it.
    pub skip: HashSet<(RelPath, BlobId)>,
    /// Step 2's extractions by (path, OID): the search uses them instead of
    /// extracting those files again, and dedups what they give.
    pub extracted: HashMap<(RelPath, BlobId), Option<Extraction>>,
    /// Stored units the candidates came from, marked as used afterwards.
    pub units: Vec<Unit>,
    pub scanned: Vec<Scanned>,
    /// The view step 3's lookup was made under (the snapshot's, a
    /// prefetch's own): the query's results are consistent with it
    /// (「查询的视图」, `Inner::check_view`).
    pub view: View,
}

impl Prepared {
    fn push(&mut self, candidate: Candidate) -> bool {
        if self
            .delivered
            .insert(loc_key(&candidate.definition.location))
        {
            self.candidates.push(candidate);
            true
        } else {
            false
        }
    }

    fn bound(&self) -> bool {
        self.candidates
            .iter()
            .any(|c| matches!(c.evidence, Evidence::Bound(_)))
    }
}

/// `jump` and `preselect` (「跳转到定义」): the only candidate with evidence,
/// unless the current file has same-named declarations that might be the
/// target; else the first candidate.
pub(crate) fn jump_and_preselect(
    candidates: &[Candidate],
    site: Option<&Site>,
) -> (Option<u32>, Option<u32>) {
    let bound: Vec<usize> = candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| matches!(c.evidence, Evidence::Bound(_)))
        .map(|(i, _)| i)
        .collect();
    let safe = site.is_none_or(|s| s.possible.is_empty());
    let jump = (bound.len() == 1 && safe).then(|| bound[0] as u32);
    let preselect = jump.or_else(|| (!candidates.is_empty()).then_some(0));
    (jump, preselect)
}

pub(crate) fn def_ref(
    def: &Def,
    lang: &LangName,
    local: bool,
    location: Location,
) -> DefinitionRef {
    DefinitionRef {
        location,
        name: Arc::from(&*def.name),
        kind: def.kind,
        lang: lang.clone(),
        scope: def.scope.as_deref().map(Arc::from),
        local,
    }
}

/// A location in the text `at`.
pub(crate) fn here(at: &TextRef, line: u32, column: u32, end_line: u32) -> Location {
    Location {
        worktree: at.worktree.clone(),
        path: at.path.clone(),
        origin: at.origin,
        oid: at.oid,
        line,
        column,
        end_line,
    }
}

fn binding_ref(b: &LocalBinding, lang: &LangName, at: &TextRef) -> DefinitionRef {
    match &b.def {
        Some(def) => def_ref(
            def,
            lang,
            b.local,
            here(at, def.line, def.column, def.end_line),
        ),
        None => DefinitionRef {
            location: here(at, b.at.line, b.at.column, b.at.line),
            name: Arc::from(&*b.name),
            kind: DefKind::Variable,
            lang: lang.clone(),
            scope: None,
            local: b.local,
        },
    }
}

impl Inner {
    /// The ranking of a query's candidates from `from`.
    pub(crate) fn ranker<'a>(
        &self,
        snap: &'a Snap,
        from: Option<&'a str>,
        current: Option<(TextOrigin, BlobId)>,
        lang: Option<&LangName>,
    ) -> Ranker<'a> {
        let rules = lang.map(|l| self.resolver(l));
        let none = |_: &str| None;
        let ctx = ResolveContext {
            map: &snap.map,
            files: snap.map.base.files(),
            packages: Some(&snap.packages),
            extraction: &none,
        };
        let package = from.and_then(|f| {
            rules
                .as_ref()
                .and_then(|r| r.package_of(f, &ctx))
                .or_else(|| Some(crate::resolve::default_package(f)))
        });
        let dependencies = match (from.and_then(|f| snap.subproject_of(f)), &rules) {
            (Some(sub), Some(r)) => r.dependencies(sub, &ctx),
            _ => Vec::new(),
        };
        Ranker {
            from,
            current,
            packages: &snap.packages,
            package,
            dependencies,
            review: snap.review.iter().map(|p| &**p).collect(),
        }
    }

    /// A file's extraction for a query (「输入代数与派生状态」, the last
    /// point): its stored unit when it is `Done`, else extracted now with
    /// the background limits cut at `until` ([`FileDeadline`]) and, when
    /// the result settles the file, queued to be written.
    #[allow(
        clippy::too_many_arguments,
        reason = "one file's part of a query: its snapshot, the file and what it adds to"
    )]
    pub(crate) fn file_extraction(
        &self,
        snap: &Snap,
        eff: &Effective,
        key: BlobId,
        bytes: &[u8],
        until: Instant,
        cancel: &Arc<AtomicBool>,
        units: &mut Vec<Unit>,
        scanned: &mut Vec<Scanned>,
    ) -> FileExtraction {
        let Some(lang) = snap.table.get(&eff.lang.name().0).cloned() else {
            return FileExtraction::Nothing;
        };
        let version = Version::of(&lang);
        if let Some(found) = snap
            .project
            .store
            .exists(&version, &[key])
            .unwrap_or_default()
            .into_iter()
            .next()
        {
            if found.status != ExtractStatus::Done {
                return FileExtraction::Nothing;
            }
            if let Some(extraction) = snap.project.store.load(found.unit) {
                units.push(found.unit);
                return FileExtraction::Defs(extraction);
            }
        }
        let file = FileDeadline::new(self.watchdog.clock().now(), Some(until));
        let deadline = Deadline::at(&self.watchdog, file.at, BACKGROUND_MAX_LEN);
        let extract = crate::index::lock(&self.extract).clone();
        let extraction = extract(&lang, bytes, &deadline, cancel);
        if !file.settles(extraction.status) {
            return FileExtraction::Unsearched;
        }
        let status = extraction.status;
        // `Unsupported` is this run's grammar failing, not the file's: not
        // stored, as the batch tiers do.
        if status != ExtractStatus::Unsupported {
            scanned.push(Scanned {
                path: eff.path.clone(),
                oid: key,
                lang,
                extraction: extraction.clone(),
            });
        }
        match status {
            ExtractStatus::Done => FileExtraction::Defs(extraction),
            _ => FileExtraction::Nothing,
        }
    }

    /// Steps 1–3.
    pub(crate) fn prepare_definition(
        &self,
        snap: &Snap,
        at: &TextRef,
        position: Position,
        cancel: &Arc<AtomicBool>,
    ) -> Result<Prepared, QueryError> {
        crate::index::bump(&self.stats.prepares, 1);
        let _all = crate::phases::span("prepare (steps 1-3)");
        let lang = snap
            .table
            .for_path(&at.path, &*self.packs)
            .filter(|l| !snap.disabled.contains(l.name()))
            .cloned()
            .ok_or(QueryError::UnsupportedLanguage)?;
        let bytes = {
            let _t = crate::phases::span("1 read + verify current text");
            self.read_text(snap, at)?
        };
        let clock = self.watchdog.clock().clone();
        let deadline = clock.now() + INTERACTIVE_DEADLINE;
        let parsed = {
            let _t = crate::phases::span("1 parse + locals + site query");
            ParsedFile::parse_at(&lang, bytes, &*clock, deadline, cancel, position).map_err(
                |e| match e {
                    ParseError::Unsupported => QueryError::UnsupportedLanguage,
                    ParseError::TimedOut => QueryError::Unreadable("parsing timed out".into()),
                    ParseError::Cancelled => QueryError::Unreadable("cancelled".into()),
                },
            )?
        };
        let site = {
            let _t = crate::phases::span("1 site_at");
            parsed.site_at(position).ok_or(QueryError::NoSymbol)?
        };
        let name = lang.name().clone();
        let defines = site.defines.as_ref().map(|d| {
            let location = here(at, d.def.line, d.def.column, d.def.end_line);
            def_ref(&d.def, &name, d.local, location)
        });
        let symbol = Symbol {
            name: Arc::from(&*site.name),
            lang: name.clone(),
            syntax: site.syntax,
            defines: defines.clone(),
        };
        let ranker = {
            let _t = crate::phases::span("ranker");
            self.ranker(snap, Some(&at.path), Some((at.origin, at.oid)), Some(&name))
        };
        let mut prepared = Prepared {
            symbol,
            lang: lang.clone(),
            site: site.clone(),
            candidates: Vec::new(),
            delivered: HashSet::new(),
            settled: false,
            skip: HashSet::from([(at.path.clone(), at.oid)]),
            extracted: HashMap::new(),
            units: Vec::new(),
            scanned: Vec::new(),
            view: snap.view,
        };
        let current = |definition: DefinitionRef, evidence: Evidence| Candidate {
            in_review: ranker.in_review(&definition.location.path),
            definition,
            tier: CandidateTier::CurrentFile,
            evidence,
            // Parsed for this query, not looked up.
            source: CandidateSource::Scanned,
        };

        // 1. The current text.
        if let Some(defines) = defines {
            prepared.push(current(defines, Evidence::Bound(Binding::Local)));
            prepared.settled = true;
            return Ok(prepared);
        }
        if let Some(binding) = &site.binding {
            prepared.push(current(
                binding_ref(binding, &name, at),
                Evidence::Bound(Binding::Local),
            ));
            prepared.settled = true;
            return Ok(prepared);
        }
        for possible in &site.possible {
            prepared.push(current(
                binding_ref(possible, &name, at),
                Evidence::NameOnly,
            ));
        }
        for (def, local) in parsed.defs_with_local() {
            if *def.name == *site.name && accepts(&lang, site.syntax, def.kind) {
                let location = here(at, def.line, def.column, def.end_line);
                prepared.push(current(
                    def_ref(def, &name, local, location),
                    Evidence::NameOnly,
                ));
            }
        }

        // 2. Imports and bindings with evidence (the resolution extension
        // point; none with #17's empty rules).
        let step2 = crate::phases::span("2 imports");
        let rules = self.resolver(&name);
        let until = clock.now() + INTERACTIVE_DEADLINE;
        let pending: RefCell<Pending> = RefCell::default();
        // A file's effective content, its OID and its extraction, each file
        // extracted once. Only the snapshot's content counts
        // (`read_content`): a file changed since gives nothing here, and the
        // search counts it as unsearched.
        let load = |path: &str| -> Option<(Effective, BlobId, Option<Extraction>)> {
            let eff = snap.map.effective(path)?;
            let FileRead::Content(bytes) = self.read_content(snap, &eff) else {
                return None;
            };
            let key = eff.oid;
            let mut p = pending.borrow_mut();
            let (units, scanned, extracted) = &mut *p;
            if let Some(done) = extracted.get(&(eff.path.clone(), key)) {
                return Some((eff, key, done.clone()));
            }
            let done = match self
                .file_extraction(snap, &eff, key, &bytes, until, cancel, units, scanned)
            {
                // Not kept: the search extracts it again, or counts it.
                FileExtraction::Unsearched => return Some((eff, key, None)),
                other => other.defs(),
            };
            extracted.insert((eff.path.clone(), key), done.clone());
            Some((eff, key, done))
        };
        let extraction = |path: &str| load(path).and_then(|(_, _, e)| e);
        let ctx = ResolveContext {
            map: &snap.map,
            files: snap.map.base.files(),
            packages: Some(&snap.packages),
            extraction: &extraction,
        };
        for import in rules.imports(&parsed, &ctx) {
            let Some(exported) = import.exported(&site.name) else {
                continue;
            };
            let Some((eff, key, Some(found))) = load(&import.target) else {
                continue;
            };
            for def in &found.defs {
                if *def.name != *exported || def.visibility == Visibility::File {
                    continue;
                }
                if !accepts(&lang, site.syntax, def.kind) {
                    continue;
                }
                let location = Location {
                    worktree: snap.worktree.clone(),
                    path: eff.path.clone(),
                    origin: TextOrigin::Worktree,
                    oid: key,
                    line: def.line,
                    column: def.column,
                    end_line: def.end_line,
                };
                prepared.push(Candidate {
                    in_review: ranker.in_review(&eff.path),
                    definition: def_ref(def, eff.lang.name(), false, location),
                    tier: CandidateTier::Imported,
                    evidence: Evidence::Bound(Binding::Import),
                    source: CandidateSource::Resolver,
                });
            }
        }
        if let Some(bound) = rules.bind(&site, &parsed, &ctx) {
            let l = &bound.definition.location;
            let tier = ranker.tier(&l.path, l.origin, l.oid, false);
            prepared.push(Candidate {
                in_review: ranker.in_review(&l.path),
                tier,
                definition: bound.definition,
                evidence: Evidence::Bound(bound.binding),
                source: CandidateSource::Resolver,
            });
        }
        let (units, scanned, extracted) = pending.into_inner();
        drop(step2);
        prepared.units.extend(units);
        prepared.scanned.extend(scanned);
        prepared.extracted = extracted;
        if prepared.bound() {
            prepared.settled = true;
            return Ok(prepared);
        }

        // 3. The index.
        self.index_step(snap, &at.path, &ranker, &mut prepared);
        Ok(prepared)
    }

    /// Step 3 into `prepared`: the index's candidates, disambiguated and
    /// ranked, after those already there. Run again by
    /// [`look_up_again`](Self::look_up_again). Whether it added some.
    fn index_step(
        &self,
        snap: &Snap,
        from: &str,
        ranker: &Ranker<'_>,
        prepared: &mut Prepared,
    ) -> bool {
        let rules = self.resolver(prepared.lang.name());
        let mut drafts = self.index_candidates(
            snap,
            &prepared.lang,
            &prepared.site,
            from,
            ranker,
            &mut prepared.units,
        );
        let _t = crate::phases::span("3 disambiguate + rank");
        rules.disambiguate(&prepared.site, &mut drafts);
        let mut found: Vec<Candidate> = drafts.into_iter().map(|d| d.candidate).collect();
        ranker.sort(&mut found);
        crate::phases::count("candidates", found.len() as u64);
        let mut added = false;
        for candidate in found {
            added |= prepared.push(candidate);
        }
        added
    }

    /// After the hint reads of a step: whether the query's view still
    /// holds. Hints installed since (a batch committed after step 3 looked
    /// up, or after the prefetch did) are looked up again and the new
    /// candidates go out; evictions or the session's end mean the query
    /// ends `Stale` (false).
    fn hold_view(&self, run: &Run, snap: &Snap, at: &TextRef, p: &mut Prepared) -> bool {
        match self.check_view(snap, &p.view) {
            ViewCheck::Same => true,
            ViewCheck::Grew(hinted) => {
                p.view.hinted = hinted;
                self.look_up_again(run, snap, at, p);
                true
            }
            ViewCheck::Evicted | ViewCheck::Ended => false,
        }
    }

    /// Step 3 again, the new candidates out ([`hold_view`](Self::hold_view)).
    fn look_up_again(&self, run: &Run, snap: &Snap, at: &TextRef, prepared: &mut Prepared) {
        crate::phases::count("second lookups", 1);
        let ranker = self.ranker(
            snap,
            Some(&at.path),
            Some((at.origin, at.oid)),
            Some(prepared.lang.name()),
        );
        if self.index_step(snap, &at.path, &ranker, prepared) {
            let all: Arc<[Candidate]> = prepared.candidates.clone().into();
            let (jump, preselect) = jump_and_preselect(&prepared.candidates, Some(&prepared.site));
            self.publish_in(snap, run.id, |r| {
                r.candidates = all;
                r.jump = jump;
                r.preselect = preselect;
            });
        }
    }

    /// Step 3: the stored definitions named like the site, in the units of
    /// the current version of every language of its group, at every path
    /// the map has for their OID, visible from `from` and of a kind the
    /// site's syntax accepts.
    fn index_candidates(
        &self,
        snap: &Snap,
        lang: &Lang,
        site: &Site,
        from: &str,
        ranker: &Ranker<'_>,
        units: &mut Vec<Unit>,
    ) -> Vec<CandidateDraft> {
        let versions: Vec<Version> = snap
            .table
            .iter()
            .filter(|l| !snap.disabled.contains(l.name()))
            .filter(|l| compatible(&snap.table, l.name(), lang.name()))
            .map(Version::of)
            .collect();
        let stored = {
            let _t = crate::phases::span("3 index lookup (SQL)");
            snap.project
                .store
                .defs_by_name(&site.name, &versions)
                .unwrap_or_default()
        };
        crate::phases::count("stored defs", stored.len() as u64);
        let _t = crate::phases::span("3 filter + tier");
        let mut out = Vec::new();
        for s in stored {
            if !accepts(lang, site.syntax, s.def.kind) {
                continue;
            }
            let mut used = false;
            for eff in snap.map.paths(&s.oid, &s.lang.0) {
                if !visible(s.def.visibility, &eff.path, from) {
                    continue;
                }
                used = true;
                let location = Location {
                    worktree: snap.worktree.clone(),
                    path: eff.path.clone(),
                    origin: TextOrigin::Worktree,
                    oid: s.oid,
                    line: s.def.line,
                    column: s.def.column,
                    end_line: s.def.end_line,
                };
                out.push(CandidateDraft {
                    candidate: Candidate {
                        tier: ranker.tier(&eff.path, TextOrigin::Worktree, s.oid, false),
                        in_review: ranker.in_review(&eff.path),
                        definition: def_ref(&s.def, &s.lang, false, location),
                        evidence: Evidence::NameOnly,
                        source: CandidateSource::Index,
                    },
                    arity: s.def.arity,
                });
            }
            if used {
                units.push(s.unit);
            }
        }
        out
    }

    /// A `Definition` query: steps 1–3 (or a fresh prefetch of them), then
    /// the search.
    pub(crate) fn run_definition(
        self: &Arc<Self>,
        run: &Run,
        snap: &Snap,
        at: &TextRef,
        position: Position,
        prepared: Option<Arc<Prepared>>,
    ) {
        // Steps 1–3 under one view: units evicted meanwhile (the lookup and
        // the hints read later would disagree) make them again once, under
        // a new snapshot, before anything is published (「查询的视图」).
        let mut again: Option<Snap> = None;
        let mut prepared = prepared;
        let mut attempt = 0;
        let mut p = loop {
            let s = again.as_ref().unwrap_or(snap);
            let made = match prepared.take() {
                Some(p) => Ok((*p).clone()),
                None => self.prepare_definition(s, at, position, &run.cancel),
            };
            let p = match made {
                Ok(p) => p,
                Err(error) => {
                    if !run.stopped(s) {
                        self.publish_in(s, run.id, |r| r.state = QueryState::Failed(error));
                    }
                    return;
                }
            };
            if run.stopped(s) {
                return;
            }
            self.step_hook(Step::Lookup, s);
            match self.check_view(s, &p.view) {
                // No hint is read yet: what grew is looked up after the
                // scope's reads.
                ViewCheck::Same | ViewCheck::Grew(_) => break p,
                ViewCheck::Ended => return,
                ViewCheck::Evicted if attempt == 0 => {
                    attempt += 1;
                    match self.snap(&snap.project.key) {
                        Some(fresh) => again = Some(fresh),
                        None => return self.end_stale(s, run.id),
                    }
                }
                ViewCheck::Evicted => return self.end_stale(s, run.id),
            }
        };
        let snap = again.as_ref().unwrap_or(snap);
        {
            let _t = crate::phases::span("publish first batch");
            let (jump, preselect) = jump_and_preselect(&p.candidates, Some(&p.site));
            let first: Arc<[Candidate]> = p.candidates.clone().into();
            let symbol = p.symbol.clone();
            let published = self.publish_in(snap, run.id, |r| {
                r.symbol = Some(symbol);
                r.candidates = first;
                r.jump = jump;
                r.preselect = preselect;
            });
            if !published {
                return;
            }
        }
        if !p.settled {
            let _t = crate::phases::span("escalate");
            self.escalate_definition(run.id, snap, at, position, &p);
        }
        let completeness = if p.settled {
            Some(Completeness::Complete)
        } else {
            let scope = {
                let _t = crate::phases::span("4 search scope");
                snap.search_scope(&at.path, &p.lang, &p.skip)
            };
            crate::phases::count("scope files", scope.files.len() as u64);
            self.step_hook(Step::Hints, snap);
            {
                let _t = crate::phases::span("view check");
                if !self.hold_view(run, snap, at, &mut p) {
                    return self.end_stale(snap, run.id);
                }
            }
            let search = crate::phases::span("4 search");
            let extracted = std::mem::take(&mut p.extracted);
            let ranker = self.ranker(
                snap,
                Some(&at.path),
                Some((at.origin, at.oid)),
                Some(p.lang.name()),
            );
            let name = p.symbol.name.clone();
            let (lang, site) = (p.lang.clone(), p.site.clone());
            let mut units = std::mem::take(&mut p.units);
            let mut scanned = std::mem::take(&mut p.scanned);
            let result = self.search(
                run,
                snap,
                &scope,
                &lang,
                &name,
                run.started + SEARCH_FOR,
                |eff, key, bytes, until| {
                    let extraction = match extracted.get(&(eff.path.clone(), key)) {
                        Some(done) => done.clone(),
                        None => match self.file_extraction(
                            snap,
                            eff,
                            key,
                            &bytes,
                            until,
                            &run.cancel,
                            &mut units,
                            &mut scanned,
                        ) {
                            FileExtraction::Unsearched => return Searched::Unsearched,
                            other => other.defs(),
                        },
                    };
                    let Some(extraction) = extraction else {
                        return Searched::Done;
                    };
                    let mut found = Vec::new();
                    for def in &extraction.defs {
                        if *def.name != *name || !accepts(&lang, site.syntax, def.kind) {
                            continue;
                        }
                        if !visible(def.visibility, &eff.path, &at.path) {
                            continue;
                        }
                        let location = Location {
                            worktree: snap.worktree.clone(),
                            path: eff.path.clone(),
                            origin: TextOrigin::Worktree,
                            oid: key,
                            line: def.line,
                            column: def.column,
                            end_line: def.end_line,
                        };
                        found.push(Candidate {
                            tier: ranker.tier(&eff.path, TextOrigin::Worktree, key, false),
                            in_review: ranker.in_review(&eff.path),
                            definition: def_ref(def, eff.lang.name(), false, location),
                            evidence: Evidence::NameOnly,
                            source: CandidateSource::Scanned,
                        });
                    }
                    ranker.sort(&mut found);
                    let before = p.candidates.len();
                    for c in found {
                        p.push(c);
                    }
                    if p.candidates.len() > before {
                        let all: Arc<[Candidate]> = p.candidates.clone().into();
                        let (jump, preselect) = jump_and_preselect(&p.candidates, Some(&site));
                        self.publish_in(snap, run.id, |r| {
                            r.candidates = all;
                            r.jump = jump;
                            r.preselect = preselect;
                        });
                    }
                    Searched::Done
                },
            );
            p.units = units;
            p.scanned = scanned;
            drop(search);
            let _t = crate::phases::span("view check");
            // The counts of `Partial` read hints too.
            if result.is_some() && !self.hold_view(run, snap, at, &mut p) {
                self.store_scanned(&snap.project, snap.session, std::mem::take(&mut p.scanned));
                return self.end_stale(snap, run.id);
            }
            result
        };
        self.store_scanned(&snap.project, snap.session, std::mem::take(&mut p.scanned));
        let Some(completeness) = completeness else {
            return;
        };
        {
            let _t = crate::phases::span("publish done");
            self.publish_in(snap, run.id, |r| r.state = QueryState::Done(completeness));
        }
        let _t = crate::phases::span("touch units");
        crate::phases::count("touched units", p.units.len() as u64);
        snap.project.store.touch(&p.units);
    }

    /// Ends a query whose view moved after it published (「查询的视图」):
    /// what it published stays; its state says to ask again.
    pub(crate) fn end_stale(&self, snap: &Snap, id: QueryId) {
        self.publish_in(snap, id, |r| {
            r.state = QueryState::Failed(QueryError::Stale)
        });
    }

    /// A `Named` query: step 3 only, every definition of the name in any
    /// language, ranked from `near`; all `NameOnly`, no jump. Units evicted
    /// during it make it again once under a new snapshot (nothing is
    /// published before its end), then `Stale`.
    pub(crate) fn run_named(
        self: &Arc<Self>,
        run: &Run,
        snap: &Snap,
        name: &str,
        near: Option<&str>,
    ) {
        let mut again: Option<Snap> = None;
        for attempt in 0..2 {
            let s = again.as_ref().unwrap_or(snap);
            match self.named_once(run, s, name, near) {
                Ended::Done | Ended::Stopped => return,
                Ended::Evicted if attempt == 0 => match self.snap(&snap.project.key) {
                    Some(fresh) => again = Some(fresh),
                    None => return self.end_stale(s, run.id),
                },
                Ended::Evicted => return self.end_stale(s, run.id),
            }
        }
    }

    fn named_once(&self, run: &Run, snap: &Snap, name: &str, near: Option<&str>) -> Ended {
        let view = snap.view;
        let versions: Vec<Version> = snap
            .table
            .iter()
            .filter(|l| !snap.disabled.contains(l.name()))
            .map(Version::of)
            .collect();
        let ranker = self.ranker(snap, near, None, None);
        let mut candidates = Vec::new();
        let mut delivered = HashSet::new();
        let mut units = Vec::new();
        let mut look_up = |candidates: &mut Vec<Candidate>, units: &mut Vec<Unit>| {
            let stored = {
                let _t = crate::phases::span("named: lookup (SQL)");
                snap.project
                    .store
                    .defs_by_name(name, &versions)
                    .unwrap_or_default()
            };
            crate::phases::count("named: stored defs", stored.len() as u64);
            let _t = crate::phases::span("named: candidates");
            for s in stored {
                for eff in snap.map.paths(&s.oid, &s.lang.0) {
                    let location = Location {
                        worktree: snap.worktree.clone(),
                        path: eff.path.clone(),
                        origin: TextOrigin::Worktree,
                        oid: s.oid,
                        line: s.def.line,
                        column: s.def.column,
                        end_line: s.def.end_line,
                    };
                    if !delivered.insert(loc_key(&location)) {
                        continue;
                    }
                    candidates.push(Candidate {
                        tier: ranker.tier(&eff.path, TextOrigin::Worktree, s.oid, false),
                        in_review: ranker.in_review(&eff.path),
                        definition: def_ref(&s.def, &s.lang, false, location),
                        evidence: Evidence::NameOnly,
                        source: CandidateSource::Index,
                    });
                    units.push(s.unit);
                }
            }
        };
        look_up(&mut candidates, &mut units);
        self.step_hook(Step::Lookup, snap);
        if run.stopped(snap) {
            return Ended::Stopped;
        }
        // Not searched: what is not indexed yet is reported, never taken
        // as complete.
        let dir = near.and_then(|n| snap.subproject_of(n)).unwrap_or("");
        let counts = crate::phases::span("named: counts");
        let (unindexed, flying) = snap.unindexed_under(dir);
        let outside = snap.unindexed_outside(dir, None);
        drop(counts);
        self.step_hook(Step::Hints, snap);
        match self.check_view(snap, &view) {
            ViewCheck::Same => {}
            // A file the counts took as indexed whose unit was written
            // after the lookup: looked up again.
            ViewCheck::Grew(_) => look_up(&mut candidates, &mut units),
            ViewCheck::Evicted => return Ended::Evicted,
            ViewCheck::Ended => return Ended::Stopped,
        }
        {
            let _t = crate::phases::span("named: rank");
            ranker.sort(&mut candidates);
        }
        let completeness = if unindexed == 0 && flying == 0 && outside == 0 {
            Completeness::Complete
        } else {
            Completeness::Partial {
                unsearched: unindexed.saturating_add(flying),
                outside,
            }
        };
        let preselect = (!candidates.is_empty()).then_some(0);
        let publish = crate::phases::span("named: publish");
        let all: Arc<[Candidate]> = candidates.into();
        self.publish_in(snap, run.id, |r| {
            r.candidates = all;
            r.preselect = preselect;
            r.state = QueryState::Done(completeness);
        });
        drop(publish);
        let _t = crate::phases::span("named: touch");
        units.sort_by_key(|u| u.id());
        units.dedup();
        snap.project.store.touch(&units);
        Ended::Done
    }
}

/// How one pass of `Named` ended.
enum Ended {
    Done,
    /// Stopped: closed, or its session ended.
    Stopped,
    /// Units were evicted during it.
    Evicted,
}
