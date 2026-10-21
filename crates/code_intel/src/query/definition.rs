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
use super::large::CurrentText;
use super::rank::{Ranker, visible};
use super::search::{FileDeadline, Searched};
use super::text::Content;
use super::{Run, Snap, Step, View, ViewCheck};
use crate::extract::{
    Deadline, Def, ExtractStatus, Extraction, INTERACTIVE_DEADLINE, Lang, Visibility, compatible,
};
use crate::index::{Inner, Scanned};
use crate::map::Effective;
use crate::resolve::{CandidateDraft, ImportTarget, ResolveContext};
use crate::site::{LocalBinding, ParseError, ParsedFile, Site, accepts};
use crate::store::{NAME_IN_LIMIT, NAME_PAGE, NamePager, StoredDef, Version};
use crate::{DefKind, LangName};

/// A delivered location: (origin, path, OID, line, column).
pub(crate) type LocKey = (TextOrigin, RelPath, BlobId, u32, u32);

/// What step 1 has of the current text: the site at the position, the
/// text's definitions of its name (with whether each is local), the whole
/// parse for the resolution extension point (None for a large text: its
/// window's parse is in the window's coordinates), whether byte offsets go
/// to views, and whether the text is large (「大文件」).
struct Step1 {
    site: Site,
    defs: Vec<(Def, bool)>,
    parsed: Option<Arc<ParsedFile>>,
    canonical: bool,
    large: bool,
}

pub(crate) fn loc_key(l: &Location) -> LocKey {
    (l.origin.clone(), l.path.clone(), l.oid, l.line, l.column)
}

/// One file's definitions for a query, and whether their byte offsets go to
/// views (the bytes they were extracted from are the OID's content as they
/// are: `UnitMeta::canonical`).
pub(crate) type FileDefs = (Extraction, bool);

/// What step 2's extractions add: units used, extractions to write, each
/// file's extraction by (path, OID of the bytes read), and what was read.
type Pending = (
    Vec<Unit>,
    Vec<Scanned>,
    HashMap<(RelPath, BlobId), Option<FileDefs>>,
    Account,
);

/// One file's extraction for a query ([`Inner::file_extraction`]).
pub(crate) enum FileExtraction {
    /// `Done` (or a `Windowed` file's windows): its definitions.
    Defs(FileDefs),
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
        ExtractStatus::Done | ExtractStatus::Windowed => None,
        ExtractStatus::TimedOut => Some(Reason::TimedOut),
        ExtractStatus::TooLarge => Some(Reason::TooLarge),
        ExtractStatus::Binary => Some(Reason::Binary),
        // Never stored; a query's extraction doesn't yield.
        ExtractStatus::Cancelled | ExtractStatus::Parked => Some(Reason::Cancelled),
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
    pub extracted: HashMap<(RelPath, BlobId), Option<FileDefs>>,
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
    /// Step 3's lookup when the first batch did not read every page of the
    /// name (a name many units define): the rest goes out in later batches.
    pub pager: Option<NamePager>,
    /// The definitions a lookup cut by the time box did not read (an upper
    /// bound, `Store::defs_left`): counted as unsearched.
    pub left: u64,
    /// Step 3's lookup in `code_intel.deps` when the first batch did not
    /// read every page (「依赖层」「查询里的依赖」): the rest after the
    /// project's pages, in the same time box, never counted unsearched.
    pub dep_pager: Option<NamePager>,
    /// `code_intel.deps` units the candidates came from, marked used.
    pub dep_units: Vec<Unit>,
    /// Step 2's dependency files extracted on demand, to be written.
    pub dep_scanned: Vec<super::deps::DepScanned>,
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

/// `completeness` with `left` more files unsearched (the rows a lookup cut
/// by the time box did not read, `Store::defs_left`: at most that many files
/// hold the definitions it left out).
pub(crate) fn plus_unsearched(completeness: Completeness, left: u64) -> Completeness {
    if left == 0 {
        return completeness;
    }
    let left = u32::try_from(left).unwrap_or(u32::MAX);
    match completeness {
        Completeness::Complete => Completeness::Partial {
            unsearched: left,
            outside: 0,
        },
        Completeness::Partial {
            unsearched,
            outside,
        } => Completeness::Partial {
            unsearched: unsearched.saturating_add(left),
            outside,
        },
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

/// A location in the text `at`, at `byte` when the bytes parsed are that
/// text's content as they are (「字节偏移」).
pub(crate) fn here(
    at: &TextRef,
    line: u32,
    column: u32,
    byte: Option<u64>,
    end_line: u32,
) -> Location {
    Location {
        worktree: at.worktree.clone(),
        path: at.path.clone(),
        origin: at.origin.clone(),
        oid: at.oid,
        line,
        column,
        byte,
        end_line,
    }
}

/// A definition's location in the text `at` (`canonical`: its bytes are
/// that text as they are).
pub(crate) fn here_def(at: &TextRef, def: &Def, canonical: bool) -> Location {
    here(
        at,
        def.line,
        def.column,
        canonical.then_some(def.byte),
        def.end_line,
    )
}

fn binding_ref(b: &LocalBinding, lang: &LangName, at: &TextRef, canonical: bool) -> DefinitionRef {
    match &b.def {
        Some(def) => def_ref(def, lang, b.local, here_def(at, def, canonical)),
        None => DefinitionRef {
            location: here(
                at,
                b.at.line,
                b.at.column,
                canonical.then_some(b.byte),
                b.at.line,
            ),
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
        let no_dep = |_: &str, _: &str| None;
        let no_dep_extraction = |_: &crate::resolve::DepTarget| None;
        let ctx = ResolveContext {
            map: &snap.map,
            files: snap.map.base.files(),
            packages: Some(&snap.packages),
            extraction: &none,
            resolve_dep: &no_dep,
            extract_dep: &no_dep_extraction,
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
    /// point): its stored unit when it is `Done`, else extracted now under
    /// the file's limits (`Snap::file_limits`: its deadline cut at `until`,
    /// [`FileDeadline::of`], and its read limit) and, when the result
    /// settles the file, queued to be written.
    #[allow(
        clippy::too_many_arguments,
        reason = "one file's part of a query: its snapshot, the file and what it adds to"
    )]
    pub(crate) fn file_extraction(
        &self,
        snap: &Snap,
        eff: &Effective,
        key: BlobId,
        content: Content<&[u8]>,
        until: Instant,
        cancel: &Arc<AtomicBool>,
        units: &mut Vec<Unit>,
        scanned: &mut Vec<Scanned>,
    ) -> FileExtraction {
        let Some(lang) = snap.table.get(&eff.lang.name().0).cloned() else {
            return FileExtraction::Nothing;
        };
        let bytes = content.bytes;
        let version = Version::of(&lang);
        // A unit stored `TooLarge` or `TimedOut` holds nothing of the file:
        // extracted now with tier 1's limits (its hint matches:
        // `Snap::file_limits`), cut at `until`.
        if let Some((found, windows)) = snap
            .project
            .store
            .exists_with_windows(&version, &[key])
            .and_then(|(found, windows)| Some((found.into_iter().next()?, windows)))
        {
            match found.status {
                ExtractStatus::TooLarge | ExtractStatus::TimedOut => {}
                // No definitions to have: as indexed.
                ExtractStatus::Binary => return FileExtraction::Nothing,
                // An opened large file's content is its windows (「大文件」).
                ExtractStatus::Windowed => {
                    if let Some(defs) = snap.project.store.load_windows(key, &version) {
                        units.push(found.unit);
                        units.extend(windows);
                        return FileExtraction::Defs(defs);
                    }
                }
                status => {
                    if let Some(reason) = stored_reason(status) {
                        return FileExtraction::Unsearched(reason);
                    }
                    if let Some(extraction) = snap.project.store.load(found.unit) {
                        units.push(found.unit);
                        return FileExtraction::Defs((extraction, content.canonical));
                    }
                }
            }
        }
        let now = self.watchdog.clock().now();
        let limits = snap.file_limits(eff);
        let file = FileDeadline::of(limits.interactive, now, Some(until));
        let deadline = Deadline::at(&self.watchdog, file.at, limits.max_len);
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
                canonical: content.canonical,
            });
        }
        match file.reason(status) {
            None => FileExtraction::Defs((extraction, content.canonical)),
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
        let clock = self.watchdog.clock().clone();
        // The text's parse kept by an earlier jump or prefetch, or warmed when
        // it became the current text (ADR 0037): no read, no parse. The text
        // is its OID's content, as a parse kept under that OID was checked to
        // be (`read_text`). Otherwise read, verified and parsed for this site
        // (the cold path), and its whole parse kept in the background. A
        // large text (1–16 MiB, 「大文件」) only around the position.
        let version = Version::of(&lang);
        let whole = match self.cached_parse(at.oid, &version) {
            Some(kept) => {
                crate::phases::count("1 kept parses", 1);
                Ok(kept)
            }
            None => {
                let text = {
                    let _t = crate::phases::span("1 read + verify current text");
                    self.current_text(snap, at, &lang, position, cancel)?
                };
                match text {
                    CurrentText::Whole(text) => {
                        let deadline = clock.now() + INTERACTIVE_DEADLINE;
                        if let Some(me) = self.arc() {
                            me.warm_parse_of(&lang, at.oid, text.bytes.clone(), text.canonical);
                        }
                        let parsed = {
                            let _t = crate::phases::span("1 parse + locals + site query");
                            ParsedFile::parse_at(
                                &lang, text.bytes, &*clock, deadline, cancel, position,
                            )
                            .map_err(|e| match e {
                                ParseError::Unsupported => QueryError::UnsupportedLanguage,
                                ParseError::TimedOut => {
                                    QueryError::Unreadable("parsing timed out".into())
                                }
                                ParseError::Cancelled => QueryError::Unreadable("cancelled".into()),
                            })?
                        };
                        Ok((Arc::new(parsed), text.canonical))
                    }
                    CurrentText::Large(large) => Err(*large),
                }
            }
        };
        let step1 = match whole {
            Ok((parsed, canonical)) => {
                let site = {
                    let _t = crate::phases::span("1 site_at");
                    parsed.site_at(position).ok_or(QueryError::NoSymbol)?
                };
                let defs = parsed
                    .defs_with_local()
                    .filter(|(d, _)| *d.name == *site.name)
                    .map(|(d, local)| (d.clone(), local))
                    .collect();
                Step1 {
                    site,
                    defs,
                    parsed: Some(parsed),
                    canonical,
                    large: false,
                }
            }
            Err(large) => Step1 {
                site: large.site,
                defs: large.defs,
                // The window's parse is in its own coordinates: the
                // resolution extension point gets whole parses only.
                parsed: None,
                canonical: large.canonical,
                large: true,
            },
        };
        let Step1 {
            site,
            defs: own,
            parsed,
            canonical,
            large,
        } = step1;
        let name = lang.name().clone();
        let defines = site.defines.as_ref().map(|d| {
            let location = here_def(at, &d.def, canonical);
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
            self.ranker(
                snap,
                Some(&at.path),
                Some((at.origin.clone(), at.oid)),
                Some(&name),
            )
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
            pager: None,
            left: 0,
            dep_pager: None,
            dep_units: Vec::new(),
            dep_scanned: Vec::new(),
        };
        // The current text is read and parsed: its definitions are step 1's
        // (the map's file when the map has that content at the path). A
        // large text's are its window units (step 3): searched when its
        // chain is complete (`Windowed`); the marker and its windows are
        // then marked used with this query's units.
        let windowed = large
            .then(|| snap.project.store.exists_with_windows(&version, &[at.oid]))
            .flatten()
            .and_then(|(found, windows)| {
                let marker = found
                    .into_iter()
                    .find(|f| f.status == ExtractStatus::Windowed)?;
                Some((marker.unit, windows))
            });
        if let Some((marker, windows)) = &windowed {
            prepared.units.push(*marker);
            prepared.units.extend(windows);
        }
        if !large || windowed.is_some() {
            prepared.account.searched(&at.path, at.oid);
        } else {
            prepared
                .account
                .unsearched(&at.path, at.oid, Reason::TooLarge);
        }
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
                binding_ref(binding, &name, at, canonical),
                Evidence::Bound(Binding::Local),
            ));
            prepared.settled = true;
            return Ok(prepared);
        }
        for possible in &site.possible {
            prepared.push(current(
                binding_ref(possible, &name, at, canonical),
                Evidence::NameOnly,
            ));
        }
        for (def, local) in &own {
            if accepts(&lang, site.syntax, def.kind) {
                let location = here_def(at, def, canonical);
                prepared.push(current(
                    def_ref(def, &name, *local, location),
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
        let load = |path: &str| -> Option<(Effective, BlobId, Option<FileDefs>)> {
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
            let content = match self.read_content(snap, &eff, snap.read_limit(&eff)) {
                Ok(content) => content,
                Err(reason) => {
                    pending.borrow_mut().3.unsearched(&eff.path, key, reason);
                    return None;
                }
            };
            let mut p = pending.borrow_mut();
            let (units, scanned, extracted, account) = &mut *p;
            let content = Content {
                bytes: &content.bytes[..],
                canonical: content.canonical,
            };
            let done = match self
                .file_extraction(snap, &eff, key, content, until, cancel, units, scanned)
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
        let extraction = |path: &str| load(path).and_then(|(_, _, e)| e.map(|(e, _)| e));
        // Bare module names into the dependency roots, extracted on demand
        // (「依赖层」按需), written to `code_intel.deps` afterwards.
        let dep_written: RefCell<Vec<super::deps::DepScanned>> = RefCell::default();
        let resolve_dep = |spec: &str, sub: &str| self.resolve_dependency(snap, spec, sub);
        let extract_dep = |t: &crate::resolve::DepTarget| {
            self.extract_dependency(snap, t, until, cancel, &mut dep_written.borrow_mut())
        };
        let ctx = ResolveContext {
            map: &snap.map,
            files: snap.map.base.files(),
            packages: Some(&snap.packages),
            extraction: &extraction,
            resolve_dep: &resolve_dep,
            extract_dep: &extract_dep,
        };
        let imports = parsed
            .as_ref()
            .map(|p| rules.imports(p, &ctx))
            .unwrap_or_default();
        for import in imports {
            let Some(exported) = import.exported(&site.name) else {
                continue;
            };
            let target = match import.target {
                ImportTarget::Worktree(path) => path,
                ImportTarget::Dependency(t) => {
                    // A dependency target's exported definitions: `Imported`,
                    // `Bound(Import)`, at the file in its root.
                    let Some((found, key)) = extract_dep(&t) else {
                        continue;
                    };
                    let path: RelPath = Arc::from(t.file.path());
                    for def in &found.defs {
                        if *def.name != *exported || def.visibility == Visibility::File {
                            continue;
                        }
                        if !accepts(&lang, site.syntax, def.kind) {
                            continue;
                        }
                        let location = Location {
                            worktree: snap.worktree.clone(),
                            path: path.clone(),
                            origin: TextOrigin::Dependency((*t.origin).clone()),
                            oid: key,
                            line: def.line,
                            column: def.column,
                            byte: Some(def.byte),
                            end_line: def.end_line,
                        };
                        prepared.push(Candidate {
                            in_review: false,
                            definition: def_ref(def, &name, false, location),
                            tier: CandidateTier::Imported,
                            evidence: Evidence::Bound(Binding::Import),
                            source: CandidateSource::Resolver,
                        });
                    }
                    continue;
                }
            };
            let Some((eff, key, Some((found, canonical)))) = load(&target) else {
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
                    byte: canonical.then_some(def.byte),
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
        if let Some(bound) = parsed.as_ref().and_then(|p| rules.bind(&site, p, &ctx)) {
            let l = &bound.definition.location;
            let tier = ranker.tier(&l.path, &l.origin, l.oid, false);
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
        prepared.dep_scanned.extend(dep_written.into_inner());
        prepared.units.extend(units);
        prepared.scanned.extend(scanned);
        prepared.extracted = extracted;
        prepared.account.merge(account);
        if prepared.bound() {
            prepared.settled = true;
            return Ok(prepared);
        }

        // 3. The index.
        self.index_step(snap, &at.path, &ranker, &mut prepared);
        Ok(prepared)
    }

    /// The current versions of the languages of `lang`'s group: what step
    /// 3 looks up.
    fn group_versions(snap: &Snap, lang: &Lang) -> Vec<Version> {
        snap.table
            .iter()
            .filter(|l| !snap.disabled.contains(l.name()))
            .filter(|l| compatible(&snap.table, l.name(), lang.name()))
            .map(Version::of)
            .collect()
    }

    /// Step 3 into `prepared`, the first batch's part (「候选的分层与过滤」):
    /// the first page of the name's definitions; when there are more (a
    /// name many units define), the definitions in the files nearest the
    /// jump as well (its directory, its sub-project), so the best tiers are
    /// in the first batch whatever the name's count. Both are bounded: one
    /// page of rows, one seek per near file. The other pages go out in
    /// later batches ([`rest_of_lookup`](Self::rest_of_lookup)).
    fn index_step(
        &self,
        snap: &Snap,
        from: &str,
        ranker: &Ranker<'_>,
        prepared: &mut Prepared,
    ) -> bool {
        let versions = Self::group_versions(snap, &prepared.lang);
        let name = prepared.site.name.clone();
        let mut pager = NamePager::new(&name, &versions);
        let mut stored = {
            let _t = crate::phases::span("3 index lookup (SQL)");
            snap.project
                .store
                .next_defs(&mut pager, NAME_PAGE)
                .unwrap_or_default()
        };
        if !pager.done() {
            let _t = crate::phases::span("3 near lookup (SQL)");
            let lang = prepared.lang.clone();
            let (siblings, rest) =
                snap.near_oids(from, &|l: &Lang| snap.in_group(l, &lang), &|p: &str| {
                    ranker.in_review(p)
                });
            crate::phases::count("near files", (siblings.len() + rest.len()) as u64);
            for near in [&siblings, &rest] {
                stored.extend(
                    snap.project
                        .store
                        .defs_by_name_in(&name, &versions, near, NAME_IN_LIMIT)
                        .unwrap_or_default(),
                );
            }
        }
        prepared.pager = (!pager.done()).then_some(pager);
        let added = self.add_index_candidates(snap, from, ranker, prepared, stored);
        // The dependencies' first page, after the project's first page and
        // nearest files (ADR 0033): tier `Dependency`, last.
        if snap.deps.len() > 0 {
            let _t = crate::phases::span("3 dependency lookup (SQL)");
            let mut dep_pager = NamePager::new(&name, &versions);
            let stored = snap
                .project
                .deps
                .next_defs(&mut dep_pager, NAME_PAGE)
                .unwrap_or_default();
            self.add_dep_candidates(snap, from, prepared, stored);
            prepared.dep_pager = (!dep_pager.done()).then_some(dep_pager);
        }
        added
    }

    /// The dependency candidates of `stored` (definitions by name in
    /// `code_intel.deps`) at every file the dependency map has for their
    /// OID (in the language its name gives), visible from `from` and of a
    /// kind the site accepts: tier `Dependency`, by package, path and line,
    /// after those already there. Whether it added some.
    fn add_dep_candidates(
        &self,
        snap: &Snap,
        from: &str,
        prepared: &mut Prepared,
        stored: Vec<StoredDef>,
    ) -> bool {
        let mut found = Vec::new();
        for s in stored {
            if !accepts(&prepared.lang, prepared.site.syntax, s.def.kind) {
                continue;
            }
            let mut used = false;
            for e in snap.deps.paths(&s.oid) {
                let lang = crate::deps::dep_lang(&snap.table, &e.path);
                if lang.is_none_or(|l| *l.name() != s.lang) {
                    continue;
                }
                if !visible(s.def.visibility, &e.path, from) {
                    continue;
                }
                used = true;
                let location = Location {
                    worktree: snap.worktree.clone(),
                    path: Arc::from(e.path),
                    origin: TextOrigin::Dependency((*e.origin).clone()),
                    oid: s.oid,
                    line: s.def.line,
                    column: s.def.column,
                    byte: s.byte(),
                    end_line: s.def.end_line,
                };
                found.push(Candidate {
                    tier: CandidateTier::Dependency,
                    in_review: false,
                    definition: def_ref(&s.def, &s.lang, false, location),
                    evidence: Evidence::NameOnly,
                    source: CandidateSource::Index,
                });
            }
            if used {
                prepared.dep_units.push(s.unit);
            }
        }
        // Within the tier: by package name, then path (「候选的分层与过滤」).
        found.sort_by_cached_key(|c| {
            let l = &c.definition.location;
            let package = match &l.origin {
                TextOrigin::Dependency(o) => o.package.clone(),
                _ => Arc::from(""),
            };
            (package, l.path.clone(), l.line, l.column)
        });
        let mut added = false;
        for c in found {
            added |= prepared.push(c);
        }
        added
    }

    /// The candidates of `stored` (stored definitions by name) into
    /// `prepared`, disambiguated and ranked among themselves, after those
    /// already there (results only grow); those already delivered are
    /// skipped. Whether it added some.
    fn add_index_candidates(
        &self,
        snap: &Snap,
        from: &str,
        ranker: &Ranker<'_>,
        prepared: &mut Prepared,
        stored: Vec<StoredDef>,
    ) -> bool {
        crate::phases::count("stored defs", stored.len() as u64);
        let rules = self.resolver(prepared.lang.name());
        let mut drafts = self.index_candidates(
            snap,
            &prepared.lang,
            &prepared.site,
            from,
            ranker,
            &mut prepared.units,
            stored,
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

    /// The pages of step 3's lookup the first batch did not read, each one
    /// statement of at most [`NAME_PAGE`] rows and its candidates published
    /// at once, until the lookup ends, the query stops or its time box (the
    /// search's) is due: then the rows not read count as unsearched
    /// (`Prepared::left`). False when the query stopped.
    fn rest_of_lookup(&self, run: &Run, snap: &Snap, at: &TextRef, p: &mut Prepared) -> bool {
        if !self.rest_of_project_lookup(run, snap, at, p) {
            return false;
        }
        self.rest_of_dep_lookup(run, snap, at, p)
    }

    /// The dependency pages the first batch did not read, after the
    /// project's, in the same time box; what the box leaves is not counted
    /// (dependencies are not part of any completeness, 「依赖层」). False
    /// when the query stopped.
    fn rest_of_dep_lookup(&self, run: &Run, snap: &Snap, at: &TextRef, p: &mut Prepared) -> bool {
        let Some(mut pager) = p.dep_pager.take() else {
            return true;
        };
        let clock = self.watchdog.clock().clone();
        while !pager.done() {
            if run.stopped(snap) {
                return false;
            }
            if clock.now() >= run.until {
                break;
            }
            let stored = snap
                .project
                .deps
                .next_defs(&mut pager, NAME_PAGE)
                .unwrap_or_default();
            if self.add_dep_candidates(snap, &at.path, p, stored) {
                let all: Arc<[Candidate]> = p.candidates.clone().into();
                let (jump, preselect) = jump_and_preselect(&p.candidates, Some(&p.site));
                self.publish_in(snap, run.id, |r| {
                    r.candidates = all;
                    r.jump = jump;
                    r.preselect = preselect;
                });
            }
        }
        true
    }

    /// The project's pages of step 3's lookup (`rest_of_lookup`).
    fn rest_of_project_lookup(
        &self,
        run: &Run,
        snap: &Snap,
        at: &TextRef,
        p: &mut Prepared,
    ) -> bool {
        let Some(mut pager) = p.pager.take() else {
            return true;
        };
        let _t = crate::phases::span("3 later pages");
        let ranker = self.ranker(
            snap,
            Some(&at.path),
            Some((at.origin.clone(), at.oid)),
            Some(p.lang.name()),
        );
        let clock = self.watchdog.clock().clone();
        while !pager.done() {
            if run.stopped(snap) {
                return false;
            }
            if clock.now() >= run.until {
                p.left += snap.project.store.defs_left(&pager);
                break;
            }
            let stored = snap
                .project
                .store
                .next_defs(&mut pager, NAME_PAGE)
                .unwrap_or_default();
            if self.add_index_candidates(snap, &at.path, &ranker, p, stored) {
                let all: Arc<[Candidate]> = p.candidates.clone().into();
                let (jump, preselect) = jump_and_preselect(&p.candidates, Some(&p.site));
                self.publish_in(snap, run.id, |r| {
                    r.candidates = all;
                    r.jump = jump;
                    r.preselect = preselect;
                });
            }
        }
        true
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

    /// Step 3 again, every page, the new candidates out as they come
    /// ([`hold_view`](Self::hold_view)); the time box and the query's stop
    /// end it as they end the first lookup's pages.
    fn look_up_again(&self, run: &Run, snap: &Snap, at: &TextRef, prepared: &mut Prepared) {
        crate::phases::count("second lookups", 1);
        let versions = Self::group_versions(snap, &prepared.lang);
        prepared.pager = Some(NamePager::new(&prepared.site.name, &versions));
        self.rest_of_lookup(run, snap, at, prepared);
    }

    /// Step 3's candidates of `stored` (definitions named like the site, in
    /// the units of the current version of every language of its group): at
    /// every path the map has for their OID, visible from `from` and of a
    /// kind the site's syntax accepts.
    #[allow(
        clippy::too_many_arguments,
        reason = "the snapshot, the site and its ranking, and what it adds to"
    )]
    fn index_candidates(
        &self,
        snap: &Snap,
        lang: &Lang,
        site: &Site,
        from: &str,
        ranker: &Ranker<'_>,
        units: &mut Vec<Unit>,
        stored: Vec<StoredDef>,
    ) -> Vec<CandidateDraft> {
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
                    byte: s.byte(),
                    end_line: s.def.end_line,
                };
                out.push(CandidateDraft {
                    candidate: Candidate {
                        tier: ranker.tier(&eff.path, &TextOrigin::Worktree, s.oid, false),
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
        self.store_dep_scanned(
            &snap.project,
            snap.session,
            std::mem::take(&mut p.dep_scanned),
        );
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
            {
                let _t = crate::phases::span("escalate");
                self.escalate_definition(run.id, snap, at, position, &p);
            }
            // The index's other pages (a name many units define), in later
            // batches before the search.
            if !self.rest_of_lookup(run, snap, at, &mut p) {
                self.store_scanned(&snap.project, snap.session, std::mem::take(&mut p.scanned));
                return;
            }
        }
        // A binding the steps proved settles the jump: no other file can
        // change it (「跳转到定义」).
        let completeness = if p.settled {
            Some(Completeness::Complete)
        } else {
            let mut account = std::mem::take(&mut p.account);
            let scope = {
                let _t = crate::phases::span("4 search scope");
                self.scope(run, snap, &at.path, &p.lang, &p.skip, &mut account)
            };
            let Some(scope) = scope else {
                self.store_scanned(&snap.project, snap.session, std::mem::take(&mut p.scanned));
                return;
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
                Some((at.origin.clone(), at.oid)),
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
                run.until,
                &mut account,
                |eff, key, content, until| {
                    let extraction = match extracted.get(&(eff.path.clone(), key)) {
                        Some(done) => done.clone(),
                        None => match self.file_extraction(
                            snap,
                            eff,
                            key,
                            Content {
                                bytes: &content.bytes[..],
                                canonical: content.canonical,
                            },
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
                    let Some((extraction, canonical)) = extraction else {
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
                            byte: canonical.then_some(def.byte),
                            end_line: def.end_line,
                        };
                        found.push(Candidate {
                            tier: ranker.tier(&eff.path, &TextOrigin::Worktree, key, false),
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
                    // Every hit that adds candidates goes out at once
                    // (「限时搜索」): no timer to tick between files.
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
                || {},
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
                    Some(plus_unsearched(snap.finish(&account, walked), p.left))
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
        p.units.sort_by_key(|u| u.id());
        p.units.dedup();
        crate::phases::count("touched units", p.units.len() as u64);
        crate::index::bump(&self.stats.touched, p.units.len());
        snap.project.store.touch(&p.units);
        if !p.dep_units.is_empty() {
            p.dep_units.sort_by_key(|u| u.id());
            p.dep_units.dedup();
            crate::index::bump(&self.stats.touched, p.dep_units.len());
            snap.project.deps.touch(&p.dep_units);
        }
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
        let mut found = NamedFound::default();
        // The first batch: the first page of the name's definitions and,
        // for a name many units define, those of the files nearest `near`
        // (its directory, its sub-project); both bounded.
        let mut pager = NamePager::new(name, &versions);
        {
            let _t = crate::phases::span("named: lookup (SQL)");
            let store = &snap.project.store;
            let mut stored = store.next_defs(&mut pager, NAME_PAGE).unwrap_or_default();
            if !pager.done()
                && let Some(near) = near
            {
                let part = |l: &Lang| !snap.disabled.contains(l.name());
                let (siblings, rest) = snap.near_oids(near, &part, &|p: &str| ranker.in_review(p));
                for oids in [&siblings, &rest] {
                    stored.extend(
                        store
                            .defs_by_name_in(name, &versions, oids, NAME_IN_LIMIT)
                            .unwrap_or_default(),
                    );
                }
            }
            crate::phases::count("named: stored defs", stored.len() as u64);
            found.add(snap, &ranker, stored);
        }
        self.step_hook(Step::Lookup, snap);
        if run.stopped(snap) {
            return Ended::Stopped;
        }
        // The candidates go out before the later pages and the accounting's
        // walk (which, once the namespace is full, checks the store until
        // the time box): what waits is the rest and the completeness. Units
        // evicted during the lookup make it again before anything is
        // published.
        match self.check_view(snap, &view) {
            ViewCheck::Same | ViewCheck::Grew(_) => {}
            ViewCheck::Evicted => return Ended::Evicted,
            ViewCheck::Ended => return Ended::Stopped,
        }
        {
            let _t = crate::phases::span("named: publish");
            if !self.publish_named(snap, run, &found, None) {
                return Ended::Stopped;
            }
        }
        // The other pages, each published as it comes.
        let mut left = match self.named_pages(run, snap, &ranker, &mut pager, &mut found) {
            Some(left) => left,
            None => return Ended::Stopped,
        };
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
            // after the lookup: looked up again, every page, the new ones
            // after those published (results only grow).
            ViewCheck::Grew(_) => {
                let mut again = NamePager::new(name, &versions);
                match self.named_pages(run, snap, &ranker, &mut again, &mut found) {
                    Some(more) => left += more,
                    None => return Ended::Stopped,
                }
            }
            // Published already: what it published stays, its state says
            // to ask again (「查询的视图」).
            ViewCheck::Evicted => {
                self.end_stale(snap, run.id);
                return Ended::Stopped;
            }
            ViewCheck::Ended => return Ended::Stopped,
        }
        let completeness = plus_unsearched(snap.finish(&account, walked), left);
        {
            let _t = crate::phases::span("named: publish");
            self.publish_named(snap, run, &found, Some(completeness));
        }
        let _t = crate::phases::span("named: touch");
        let mut units = found.units;
        units.sort_by_key(|u| u.id());
        units.dedup();
        snap.project.store.touch(&units);
        Ended::Done
    }

    /// Publishes `Named`'s candidates so far, and its end when `done`
    /// says how complete it is. False when not published.
    fn publish_named(
        &self,
        snap: &Snap,
        run: &Run,
        found: &NamedFound,
        done: Option<Completeness>,
    ) -> bool {
        let preselect = (!found.candidates.is_empty()).then_some(0);
        let all: Arc<[Candidate]> = found.candidates.clone().into();
        self.publish_in(snap, run.id, |r| {
            r.candidates = all;
            r.preselect = preselect;
            if let Some(c) = done {
                r.state = QueryState::Done(c);
            }
        })
    }

    /// `Named`'s pages from `pager` on, each one statement of at most
    /// [`NAME_PAGE`] rows, its new candidates published as they come,
    /// until the lookup ends or the query's time box (the search's) is due:
    /// the rows not read (`Store::defs_left`, counted unsearched). None
    /// when the query stopped.
    fn named_pages(
        &self,
        run: &Run,
        snap: &Snap,
        ranker: &Ranker<'_>,
        pager: &mut NamePager,
        found: &mut NamedFound,
    ) -> Option<u64> {
        let clock = self.watchdog.clock().clone();
        while !pager.done() {
            if run.stopped(snap) {
                return None;
            }
            if clock.now() >= run.until {
                return Some(snap.project.store.defs_left(pager));
            }
            let stored = snap
                .project
                .store
                .next_defs(pager, NAME_PAGE)
                .unwrap_or_default();
            if found.add(snap, ranker, stored) && !self.publish_named(snap, run, found, None) {
                return None;
            }
        }
        Some(0)
    }
}

/// What `Named` has found: its candidates in the order published, each
/// location once, and the units they came from.
#[derive(Default)]
struct NamedFound {
    candidates: Vec<Candidate>,
    delivered: HashSet<LocKey>,
    units: Vec<Unit>,
}

impl NamedFound {
    /// The candidates of `stored` at every path the map has for their OID,
    /// ranked among themselves after those already there (results only
    /// grow), each location once. Whether it added some.
    fn add(&mut self, snap: &Snap, ranker: &Ranker<'_>, stored: Vec<StoredDef>) -> bool {
        let _t = crate::phases::span("named: candidates");
        let from = self.candidates.len();
        for s in stored {
            for eff in snap.map.paths(&s.oid, &s.lang.0) {
                let location = Location {
                    worktree: snap.worktree.clone(),
                    path: eff.path.clone(),
                    origin: TextOrigin::Worktree,
                    oid: s.oid,
                    line: s.def.line,
                    column: s.def.column,
                    byte: s.byte(),
                    end_line: s.def.end_line,
                };
                if !self.delivered.insert(loc_key(&location)) {
                    continue;
                }
                self.candidates.push(Candidate {
                    tier: ranker.tier(&eff.path, &TextOrigin::Worktree, s.oid, false),
                    in_review: ranker.in_review(&eff.path),
                    definition: def_ref(&s.def, &s.lang, false, location),
                    evidence: Evidence::NameOnly,
                    source: CandidateSource::Index,
                });
                self.units.push(s.unit);
            }
        }
        let _r = crate::phases::span("named: rank");
        ranker.sort(&mut self.candidates[from..]);
        self.candidates.len() > from
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
