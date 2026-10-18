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
    Location, Position, QueryError, QueryState, Symbol, TextOrigin, TextRef,
};
use lectern_services::project::{BlobId, RelPath};

use super::rank::{Ranker, visible};
use super::search::{FileDeadline, Searched};
use super::{Run, SEARCH_FOR, Snap};
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
    /// The hint installations counted before step 3's lookup (the
    /// snapshot's, a prefetch's own): hints installed after it are for
    /// files the lookup may have missed ([`Snap::hinted_since`]).
    pub hinted: u64,
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
        let lang = snap
            .table
            .for_path(&at.path, &*self.packs)
            .filter(|l| !snap.disabled.contains(l.name()))
            .cloned()
            .ok_or(QueryError::UnsupportedLanguage)?;
        let bytes = self.read_text(snap, at)?;
        let clock = self.watchdog.clock().clone();
        let deadline = clock.now() + INTERACTIVE_DEADLINE;
        let parsed =
            ParsedFile::parse(&lang, bytes, &*clock, deadline, cancel).map_err(|e| match e {
                ParseError::Unsupported => QueryError::UnsupportedLanguage,
                ParseError::TimedOut => QueryError::Unreadable("parsing timed out".into()),
                ParseError::Cancelled => QueryError::Unreadable("cancelled".into()),
            })?;
        let site = parsed.site_at(position).ok_or(QueryError::NoSymbol)?;
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
        let ranker = self.ranker(snap, Some(&at.path), Some((at.origin, at.oid)), Some(&name));
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
            hinted: snap.hinted,
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
        let rules = self.resolver(&name);
        let until = clock.now() + INTERACTIVE_DEADLINE;
        let pending: RefCell<Pending> = RefCell::default();
        // A file's effective content, the OID of the bytes read and its
        // extraction, each file extracted once.
        let load = |path: &str| -> Option<(Effective, BlobId, Option<Extraction>)> {
            let eff = snap.map.effective(path)?;
            let bytes = self.read_file(snap, path)?;
            let key = self.key_of(snap, path, &bytes, eff.oid);
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
        rules.disambiguate(&prepared.site, &mut drafts);
        let mut found: Vec<Candidate> = drafts.into_iter().map(|d| d.candidate).collect();
        ranker.sort(&mut found);
        let mut added = false;
        for candidate in found {
            added |= prepared.push(candidate);
        }
        added
    }

    /// When a hint was installed after `prepared`'s cut (a batch committed
    /// since step 3 looked up, or since the prefetch did), step 3 again,
    /// and the new candidates go out. Called after the reads of hints that
    /// leave files out of the search, so none of them is missed.
    fn look_up_again(&self, run: &Run, snap: &Snap, at: &TextRef, prepared: &mut Prepared) {
        if !snap.hinted_since(&mut prepared.hinted) {
            return;
        }
        let ranker = self.ranker(
            snap,
            Some(&at.path),
            Some((at.origin, at.oid)),
            Some(prepared.lang.name()),
        );
        if self.index_step(snap, &at.path, &ranker, prepared) {
            let all: Arc<[Candidate]> = prepared.candidates.clone().into();
            let (jump, preselect) = jump_and_preselect(&prepared.candidates, Some(&prepared.site));
            self.publish(run.id, |r| {
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
        let stored = snap
            .project
            .store
            .defs_by_name(&site.name, &versions)
            .unwrap_or_default();
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
        let prepared = match prepared {
            Some(p) => Ok((*p).clone()),
            None => self.prepare_definition(snap, at, position, &run.cancel),
        };
        let mut p = match prepared {
            Ok(p) => p,
            Err(error) => {
                if !run.stopped(snap) {
                    self.publish(run.id, |r| r.state = QueryState::Failed(error));
                }
                return;
            }
        };
        if run.stopped(snap) {
            return;
        }
        let (jump, preselect) = jump_and_preselect(&p.candidates, Some(&p.site));
        let first: Arc<[Candidate]> = p.candidates.clone().into();
        let symbol = p.symbol.clone();
        self.publish(run.id, |r| {
            r.symbol = Some(symbol);
            r.candidates = first;
            r.jump = jump;
            r.preselect = preselect;
        });
        if !p.settled {
            self.escalate_definition(run.id, snap, at, position, &p);
        }
        let completeness = if p.settled {
            Some(Completeness::Complete)
        } else {
            self.run_after_lookup(snap);
            let scope = snap.search_scope(&at.path, &p.lang, &p.skip);
            self.look_up_again(run, snap, at, &mut p);
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
                        self.publish(run.id, |r| {
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
            if result.is_some() {
                // The counts of `Partial` read hints too.
                self.look_up_again(run, snap, at, &mut p);
            }
            result
        };
        self.store_scanned(&snap.project, snap.session, std::mem::take(&mut p.scanned));
        let Some(completeness) = completeness else {
            return;
        };
        self.publish(run.id, |r| r.state = QueryState::Done(completeness));
        snap.project.store.touch(&p.units);
    }

    /// A `Named` query: step 3 only, every definition of the name in any
    /// language, ranked from `near`; all `NameOnly`, no jump.
    pub(crate) fn run_named(
        self: &Arc<Self>,
        run: &Run,
        snap: &Snap,
        name: &str,
        near: Option<&str>,
    ) {
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
            let stored = snap
                .project
                .store
                .defs_by_name(name, &versions)
                .unwrap_or_default();
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
        let mut cut = snap.hinted;
        look_up(&mut candidates, &mut units);
        self.run_after_lookup(snap);
        if run.stopped(snap) {
            return;
        }
        // Not searched: what is not indexed yet is reported, never taken
        // as complete.
        let dir = near.and_then(|n| snap.subproject_of(n)).unwrap_or("");
        let (unindexed, flying) = snap.unindexed_under(dir);
        let outside = snap.unindexed_outside(dir, None);
        // A file those counts took as indexed whose unit was written after
        // the lookup: looked up again.
        if snap.hinted_since(&mut cut) {
            look_up(&mut candidates, &mut units);
        }
        ranker.sort(&mut candidates);
        let completeness = if unindexed == 0 && flying == 0 && outside == 0 {
            Completeness::Complete
        } else {
            Completeness::Partial {
                unsearched: unindexed.saturating_add(flying),
                outside,
            }
        };
        let preselect = (!candidates.is_empty()).then_some(0);
        let all: Arc<[Candidate]> = candidates.into();
        self.publish(run.id, |r| {
            r.candidates = all;
            r.preselect = preselect;
            r.state = QueryState::Done(completeness);
        });
        units.sort_by_key(|u| u.id());
        units.dedup();
        snap.project.store.touch(&units);
    }
}
