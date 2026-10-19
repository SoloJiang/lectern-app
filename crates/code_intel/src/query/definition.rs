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
//! The completeness is the accounting's (`account`): every file of the
//! jump's language group, indexed, searched, or not.

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

use super::account::{Account, Reason};
use super::rank::{Ranker, visible};
use super::search::{FileDeadline, Searched};
use super::{Run, SEARCH_FOR, Snap, Step, View, ViewCheck};
use crate::extract::{
    Deadline, Def, ExtractStatus, Extraction, INTERACTIVE_DEADLINE, Lang, Visibility, compatible,
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

/// What step 2's extractions add: units used, extractions to write, each
/// file's extraction by (path, OID of the bytes read), and what was read.
type Pending = (
    Vec<Unit>,
    Vec<Scanned>,
    HashMap<(RelPath, BlobId), Option<Extraction>>,
    Account,
);

/// One file's extraction for a query ([`Inner::file_extraction`]).
pub(crate) enum FileExtraction {
    /// `Done`: its definitions.
    Defs(Extraction),
    /// No definitions, and none to find: a language not in the snapshot's
    /// table, or binary content (stored or extracted `Binary`: no limit
    /// changes it; the accounting takes it as searched).
    Nothing,
    /// Nothing is known of its definitions, and why: the query's time ran
    /// out or it was cancelled (not stored), or it is stored or extracted
    /// with a status that has no definitions (`TooLarge`, `Binary`,
    /// `TimedOut`, a grammar failing).
    Unsearched(Reason),
}

/// Why a file stored with `status` gives no definitions; None for `Done`.
fn stored_reason(status: ExtractStatus) -> Option<Reason> {
    match status {
        ExtractStatus::Done => None,
        ExtractStatus::TimedOut => Some(Reason::TimedOut),
        ExtractStatus::TooLarge => Some(Reason::TooLarge),
        ExtractStatus::Binary => Some(Reason::Binary),
        ExtractStatus::Cancelled => Some(Reason::Cancelled),
        ExtractStatus::Unsupported => Some(Reason::Unreadable),
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
    /// The files steps 1–3 read (the current text, step 2's targets): the
    /// search goes on with it, the accounting classifies them by it.
    pub account: Account,
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
    /// the background deadline cut at `until` ([`FileDeadline`]) and the
    /// file's read limit (`Snap::read_limit`) and, when the result settles
    /// the file, queued to be written.
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
        // A unit stored `TooLarge` or `TimedOut` holds nothing of the file:
        // extracted now with tier 1's limits, cut at `until`.
        let mut unfinished = false;
        if let Some(found) = snap
            .project
            .store
            .exists(&version, &[key])
            .unwrap_or_default()
            .into_iter()
            .next()
        {
            match found.status {
                ExtractStatus::TooLarge | ExtractStatus::TimedOut => unfinished = true,
                // No definitions to have: as indexed.
                ExtractStatus::Binary => return FileExtraction::Nothing,
                status => {
                    if let Some(reason) = stored_reason(status) {
                        return FileExtraction::Unsearched(reason);
                    }
                    if let Some(extraction) = snap.project.store.load(found.unit) {
                        units.push(found.unit);
                        return FileExtraction::Defs(extraction);
                    }
                }
            }
        }
        let now = self.watchdog.clock().now();
        let file = if unfinished {
            FileDeadline::interactive(now, Some(until))
        } else {
            FileDeadline::new(now, Some(until))
        };
        let deadline = Deadline::at(&self.watchdog, file.at, snap.read_limit(eff));
        let extract = crate::index::lock(&self.extract).clone();
        let extraction = extract(&lang, bytes, &deadline, cancel);
        let status = extraction.status;
        // `Unsupported` is this run's grammar failing, not the file's: not
        // stored, as the batch tiers do.
        if file.settles(status) && status != ExtractStatus::Unsupported {
            scanned.push(Scanned {
                path: eff.path.clone(),
                oid: key,
                lang,
                extraction: extraction.clone(),
            });
        }
        match file.reason(status) {
            None => FileExtraction::Defs(extraction),
            Some(Reason::Binary) => FileExtraction::Nothing,
            Some(reason) => FileExtraction::Unsearched(reason),
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
        // The language the snapshot's map gives the path (the table it was
        // built with); a path the map does not have by its extension, in
        // that same table.
        let lang = snap
            .map
            .effective(&at.path)
            .map(|e| e.lang)
            .or_else(|| snap.table.for_path(&at.path, &*self.packs).cloned())
            .filter(|l| !snap.disabled.contains(l.name()))
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
            account: Account::default(),
            view: snap.view,
        };
        // The current text is read and parsed: its definitions are step 1's
        // (the map's file when the map has that content at the path).
        prepared.account.searched(&at.path, at.oid);
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
        // (`read_content`): a file changed since, too large or unreadable
        // gives nothing here, and the accounting counts it unsearched.
        let load = |path: &str| -> Option<(Effective, BlobId, Option<Extraction>)> {
            let eff = snap.map.effective(path)?;
            let key = eff.oid;
            {
                let p = pending.borrow();
                if let Some(done) = p.2.get(&(eff.path.clone(), key)) {
                    return Some((eff, key, done.clone()));
                }
                if p.3.outcome(&eff.path, key).is_some() {
                    return Some((eff, key, None));
                }
            }
            let bytes = match self.read_content(snap, &eff, snap.read_limit(&eff)) {
                Ok(bytes) => bytes,
                Err(reason) => {
                    pending.borrow_mut().3.unsearched(&eff.path, key, reason);
                    return None;
                }
            };
            let mut p = pending.borrow_mut();
            let (units, scanned, extracted, account) = &mut *p;
            let done = match self
                .file_extraction(snap, &eff, key, &bytes, until, cancel, units, scanned)
            {
                FileExtraction::Defs(e) => Some(e),
                FileExtraction::Nothing => None,
                // Not kept: the search extracts it again, or it is counted.
                FileExtraction::Unsearched(reason) => {
                    account.unsearched(&eff.path, key, reason);
                    return Some((eff, key, None));
                }
            };
            account.searched(&eff.path, key);
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
        let (units, scanned, extracted, account) = pending.into_inner();
        drop(step2);
        prepared.units.extend(units);
        prepared.scanned.extend(scanned);
        prepared.extracted = extracted;
        prepared.account.merge(account);
        if prepared.bound() {
            prepared.settled = true;
            return Ok(prepared);
        }

        // 3. The index.
        self.index_step(snap, &at.path, &ranker, &mut prepared, cancel);
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
        cancel: &Arc<AtomicBool>,
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
        // Their locality from their file's parse; not for the current text's
        // definitions, which step 1 listed with theirs (dropped below as
        // delivered).
        let delivered = &prepared.delivered;
        let targets: Vec<&mut DefinitionRef> = drafts
            .iter_mut()
            .filter(|d| {
                d.maybe_local && !delivered.contains(&loc_key(&d.candidate.definition.location))
            })
            .map(|d| &mut d.candidate.definition)
            .collect();
        self.recover_locality(snap, cancel, targets);
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
        if self.index_step(snap, &at.path, &ranker, prepared, &run.cancel) {
            let all: Arc<[Candidate]> = prepared.candidates.clone().into();
            let (jump, preselect) = jump_and_preselect(&prepared.candidates, Some(&prepared.site));
            self.publish_in(snap, run.id, |r| {
                r.candidates = all;
                r.jump = jump;
                r.preselect = preselect;
            });
        }
    }

    /// The locality (`DefinitionRef::local`) of definitions from the index,
    /// which does not store it: the parse of their file gives it, as it
    /// gives a jump's current text its own. Only `Visibility::File`
    /// definitions can be local (a definition inside a function or block is
    /// `File` in every language, `rules::finish`), so only those come here;
    /// each file is read (the snapshot's content, `read_content`) and parsed
    /// once, with tier 1's limits. A file that cannot be read or parsed
    /// leaves them not local: find references then searches the project,
    /// which finds the scope's references too.
    pub(crate) fn recover_locality(
        &self,
        snap: &Snap,
        cancel: &Arc<AtomicBool>,
        mut targets: Vec<&mut DefinitionRef>,
    ) {
        if targets.is_empty() {
            return;
        }
        let _t = crate::phases::span("locality");
        let mut files: HashMap<(RelPath, BlobId), Vec<usize>> = HashMap::new();
        for (i, d) in targets.iter().enumerate() {
            files
                .entry((d.location.path.clone(), d.location.oid))
                .or_default()
                .push(i);
        }
        crate::phases::count("locality parses", files.len() as u64);
        for ((path, oid), at) in files {
            let first = &targets[at[0]].location;
            let site = Position {
                line: first.line,
                column: first.column,
            };
            let Some(local) = self.file_locals(snap, cancel, &path, oid, site) else {
                continue;
            };
            for i in at {
                let l = &targets[i].location;
                targets[i].local = local.contains(&(l.line, l.column));
            }
        }
    }

    /// The (line, column) of every local definition of the snapshot's file
    /// `path` at `oid`; None when it cannot be read or parsed.
    fn file_locals(
        &self,
        snap: &Snap,
        cancel: &Arc<AtomicBool>,
        path: &RelPath,
        oid: BlobId,
        site: Position,
    ) -> Option<HashSet<(u32, u32)>> {
        let eff = snap.map.effective(path).filter(|e| e.oid == oid)?;
        let lang = snap.table.get(&eff.lang.name().0)?.clone();
        let bytes = self.read_content(snap, &eff, snap.read_limit(&eff)).ok()?;
        let clock = self.watchdog.clock().clone();
        let deadline = clock.now() + INTERACTIVE_DEADLINE;
        let parsed =
            ParsedFile::parse_at(&lang, bytes.into(), &*clock, deadline, cancel, site).ok()?;
        Some(
            parsed
                .defs_with_local()
                .filter(|(_, local)| *local)
                .map(|(d, _)| (d.line, d.column))
                .collect(),
        )
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
                    maybe_local: s.def.visibility == Visibility::File,
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
            // A prefetch only when made under the basis of the snapshot just
            // taken: a map published between `query` and this snapshot has
            // other OIDs (its steps would mix with this snapshot's search).
            let made = match prepared.take().filter(|p| p.view.same_basis(&s.view)) {
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
        // A binding the steps proved settles the jump: no other file can
        // change it (「跳转到定义」).
        let completeness = if p.settled {
            Some(Completeness::Complete)
        } else {
            let mut account = std::mem::take(&mut p.account);
            let scope = {
                let _t = crate::phases::span("4 search scope");
                snap.search_scope(&at.path, &p.lang, &p.skip, &mut account)
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
            let searched = self.search(
                run,
                snap,
                &scope,
                &name,
                run.started + SEARCH_FOR,
                &mut account,
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
                            FileExtraction::Defs(e) => Some(e),
                            FileExtraction::Nothing => None,
                            FileExtraction::Unsearched(reason) => {
                                return Searched::Unsearched(reason);
                            }
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
            // Every file of the jump's language group, classified once.
            let part = |l: &Lang| snap.in_group(l, &lang);
            let walked = if searched {
                let _t = crate::phases::span("4 accounting");
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
            match walked {
                Some(walked) => {
                    let _t = crate::phases::span("view check");
                    // The walk read hints: a file it took as indexed whose
                    // unit was written after the lookup is looked up again.
                    if !self.hold_view(run, snap, at, &mut p) {
                        self.store_scanned(
                            &snap.project,
                            snap.session,
                            std::mem::take(&mut p.scanned),
                        );
                        return self.end_stale(snap, run.id);
                    }
                    Some(snap.finish(&account, walked))
                }
                None => None,
            }
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
        // The index has no locality: each lookup's `File` definitions get
        // theirs from their file (`recover_locality`).
        let look_up = |candidates: &mut Vec<Candidate>,
                       units: &mut Vec<Unit>,
                       delivered: &mut HashSet<LocKey>| {
            let from = candidates.len();
            let mut maybe = Vec::new();
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
                    if s.def.visibility == Visibility::File {
                        maybe.push(candidates.len());
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
            drop(_t);
            let maybe: HashSet<usize> = maybe.into_iter().collect();
            let targets = candidates[from..]
                .iter_mut()
                .enumerate()
                .filter(|(i, _)| maybe.contains(&(from + i)))
                .map(|(_, c)| &mut c.definition)
                .collect();
            self.recover_locality(snap, &run.cancel, targets);
        };
        look_up(&mut candidates, &mut units, &mut delivered);
        self.step_hook(Step::Lookup, snap);
        if run.stopped(snap) {
            return Ended::Stopped;
        }
        // Not searched: what is not indexed yet is reported, never taken
        // as complete. Every file of every enabled language, classified
        // once (`Snap::walk`).
        let dir = near.and_then(|n| snap.subproject_of(n)).unwrap_or("");
        let part = |l: &Lang| !snap.disabled.contains(l.name());
        let mut account = Account::default();
        let counts = crate::phases::span("named: counts");
        let Some(walked) = self.walk(run, snap, &mut account, dir, &part, None) else {
            return Ended::Stopped;
        };
        drop(counts);
        self.step_hook(Step::Hints, snap);
        match self.check_view(snap, &view) {
            ViewCheck::Same => {}
            // A file the counts took as indexed whose unit was written
            // after the lookup: looked up again.
            ViewCheck::Grew(_) => look_up(&mut candidates, &mut units, &mut delivered),
            ViewCheck::Evicted => return Ended::Evicted,
            ViewCheck::Ended => return Ended::Stopped,
        }
        {
            let _t = crate::phases::span("named: rank");
            ranker.sort(&mut candidates);
        }
        let completeness = snap.finish(&account, walked);
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
