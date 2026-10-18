//! The blob → path map of a project's current worktree
//! (docs/design/code_intel.md「blob → 路径映射」): the base part from the
//! file list's index OIDs, the overlay from the worktree's content, the
//! per-path `indexed` hints, and the HEAD difference (`HeadDiff`). In memory
//! only, rebuilt after a project opens. Lookups go by (OID, language): the
//! same bytes under `.java` and `.js` have one OID and two languages.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use lectern_services::project::{BlobId, FileKind, FileList, RelPath};

use crate::extract::{Lang, LanguageTable};
use crate::grammar::GrammarSource;
use crate::lang::extension;

/// The first 8 bytes of an OID, the sort key of the `by_oid` tables.
pub fn prefix(oid: &BlobId) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&oid.as_bytes()[..8]);
    u64::from_be_bytes(b)
}

/// Language lookup by path against one language table; the packs' claims
/// are asked once per extension.
pub(crate) struct LangLookup<'a> {
    table: &'a LanguageTable,
    packs: &'a dyn GrammarSource,
    cache: HashMap<Box<str>, u8>,
}

impl<'a> LangLookup<'a> {
    pub(crate) fn new(table: &'a LanguageTable, packs: &'a dyn GrammarSource) -> Self {
        LangLookup {
            table,
            packs,
            cache: HashMap::new(),
        }
    }

    /// The path's language as its table position plus one; 0 when no
    /// language of the table claims it.
    pub(crate) fn code(&mut self, path: &str) -> u8 {
        let Some(ext) = extension(path) else {
            return 0;
        };
        if let Some(code) = self.cache.get(ext) {
            return *code;
        }
        let code = self
            .table
            .position_for_extension(ext, self.packs)
            .and_then(|i| u8::try_from(i + 1).ok())
            .unwrap_or(0);
        self.cache.insert(ext.into(), code);
        code
    }
}

/// The base part: the file list's entries that have an OID, a supported
/// language and are regular or executable files. Replaced as a whole with
/// the file list or the language table.
pub struct BaseMap {
    files: Arc<FileList>,
    table: LanguageTable,
    /// Per file list entry: the language's table position plus one, 0 when
    /// the entry is not in the map.
    langs: Box<[u8]>,
    /// `by_oid`, as two arrays (12 bytes per entry): OID prefixes in order,
    /// and the file list index of each.
    prefixes: Box<[u64]>,
    indices: Box<[u32]>,
}

impl BaseMap {
    /// Builds the base part. Pure computation, no IO; 100 000 entries take a
    /// few milliseconds (`code_intel/map_100k`).
    pub fn build(
        files: Arc<FileList>,
        table: &LanguageTable,
        packs: &dyn GrammarSource,
    ) -> BaseMap {
        let mut lookup = LangLookup::new(table, packs);
        let mut langs = vec![0u8; files.len()].into_boxed_slice();
        let mut by_oid = Vec::with_capacity(files.len());
        for (i, (path, kind)) in files.iter().enumerate() {
            if !matches!(kind, FileKind::File | FileKind::Executable) {
                continue;
            }
            let Some(oid) = files.oid(i) else {
                continue;
            };
            let code = lookup.code(path);
            if code == 0 {
                continue;
            }
            langs[i] = code;
            by_oid.push((prefix(&oid), i as u32));
        }
        by_oid.sort_unstable();
        let prefixes = by_oid.iter().map(|(p, _)| *p).collect();
        let indices = by_oid.iter().map(|(_, i)| *i).collect();
        BaseMap {
            files,
            table: table.clone(),
            langs,
            prefixes,
            indices,
        }
    }

    /// No file list yet.
    pub fn empty(table: &LanguageTable) -> BaseMap {
        BaseMap::build(Arc::new(FileList::default()), table, &crate::NoGrammarPacks)
    }

    pub fn files(&self) -> &Arc<FileList> {
        &self.files
    }

    /// The number of file list entries in the map.
    pub fn len(&self) -> usize {
        self.prefixes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.prefixes.is_empty()
    }

    /// The language table the map was built against.
    pub fn table(&self) -> &LanguageTable {
        &self.table
    }

    /// The language of file list entry `i` when it is in the map.
    pub fn lang(&self, i: usize) -> Option<&Lang> {
        let code = *self.langs.get(i)?;
        self.lang_of_code(code)
    }

    pub(crate) fn lang_of_code(&self, code: u8) -> Option<&Lang> {
        code.checked_sub(1)
            .and_then(|i| self.table.langs().get(usize::from(i)))
    }

    /// Entry `i`'s index OID when it is in the map.
    pub fn oid(&self, i: usize) -> Option<BlobId> {
        if *self.langs.get(i)? == 0 {
            return None;
        }
        self.files.oid(i)
    }

    /// The file list entries in the map whose index OID is `oid`, checked
    /// against the full OID.
    pub fn find(&self, oid: &BlobId) -> impl Iterator<Item = usize> + '_ {
        let p = prefix(oid);
        let start = self.prefixes.partition_point(|x| *x < p);
        let oid = *oid;
        self.prefixes[start..]
            .iter()
            .take_while(move |x| **x == p)
            .zip(&self.indices[start..])
            .map(|(_, i)| *i as usize)
            .filter(move |i| self.files.oid(*i) == Some(oid))
    }

    /// Heap bytes held (not the file list, which belongs to `project`).
    pub fn heap_bytes(&self) -> usize {
        self.langs.len() + self.prefixes.len() * 8 + self.indices.len() * 4
    }
}

/// What the worktree holds at a path whose content differs from the index
/// (or which the index does not have).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WorktreeState {
    /// The content's OID (of the raw bytes).
    Content(BlobId),
    /// No file at the path: a deletion not staged, an entry outside the
    /// sparse checkout.
    Absent,
    /// A file not indexed: over 1 MiB, unreadable, a path through a
    /// symbolic link, a filter's pointer file (Git LFS).
    Unindexable,
}

/// The overlay: worktree states of paths with a supported language, in
/// path order. A path in the file list hides its index entry.
pub struct Overlay {
    entries: Box<[(RelPath, WorktreeState)]>,
    /// Per entry: the language code (table position plus one) and the file
    /// list index (`u32::MAX` when the file list does not have the path).
    langs: Box<[u8]>,
    base: Box<[u32]>,
    /// File list indices hidden by an entry, in order.
    hidden: Box<[u32]>,
    /// `by_oid` of the `Content` entries.
    prefixes: Box<[u64]>,
    indices: Box<[u32]>,
}

/// No file list index.
pub(crate) const UNTRACKED: u32 = u32::MAX;

impl Overlay {
    pub fn empty() -> Overlay {
        Overlay {
            entries: Box::new([]),
            langs: Box::new([]),
            base: Box::new([]),
            hidden: Box::new([]),
            prefixes: Box::new([]),
            indices: Box::new([]),
        }
    }

    /// Builds the overlay over `base`. Entries whose path has no language in
    /// `base`'s table are left out; entries equal to the index (`Content` of
    /// the index OID, `Absent` of a path the file list does not have) too.
    pub(crate) fn build(
        mut entries: Vec<(RelPath, WorktreeState)>,
        base: &BaseMap,
        packs: &dyn GrammarSource,
    ) -> Overlay {
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries.dedup_by(|later, earlier| {
            if later.0 == earlier.0 {
                earlier.1 = later.1;
                true
            } else {
                false
            }
        });
        let mut lookup = LangLookup::new(base.table(), packs);
        let files = base.files();
        let mut kept = Vec::with_capacity(entries.len());
        let mut langs = Vec::with_capacity(entries.len());
        let mut indices_in_base = Vec::with_capacity(entries.len());
        for (path, state) in entries {
            let code = lookup.code(&path);
            if code == 0 {
                continue;
            }
            let at = find_path(files, &path);
            let index_oid = at.and_then(|i| files.oid(i));
            let redundant = match state {
                WorktreeState::Content(oid) => index_oid == Some(oid),
                WorktreeState::Absent => at.is_none(),
                WorktreeState::Unindexable => false,
            };
            if redundant {
                continue;
            }
            kept.push((path, state));
            langs.push(code);
            indices_in_base.push(at.map_or(UNTRACKED, |i| i as u32));
        }
        let mut hidden: Vec<u32> = indices_in_base
            .iter()
            .copied()
            .filter(|i| *i != UNTRACKED)
            .collect();
        hidden.sort_unstable();
        let mut by_oid: Vec<(u64, u32)> = kept
            .iter()
            .enumerate()
            .filter_map(|(i, (_, s))| match s {
                WorktreeState::Content(oid) => Some((prefix(oid), i as u32)),
                _ => None,
            })
            .collect();
        by_oid.sort_unstable();
        Overlay {
            entries: kept.into(),
            langs: langs.into(),
            base: indices_in_base.into(),
            hidden: hidden.into(),
            prefixes: by_oid.iter().map(|(p, _)| *p).collect(),
            indices: by_oid.iter().map(|(_, i)| *i).collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[(RelPath, WorktreeState)] {
        &self.entries
    }

    /// The overlay entry of a path.
    pub fn get(&self, path: &str) -> Option<WorktreeState> {
        self.position(path).map(|i| self.entries[i].1)
    }

    pub(crate) fn position(&self, path: &str) -> Option<usize> {
        self.entries.binary_search_by(|(p, _)| (**p).cmp(path)).ok()
    }

    /// Whether file list entry `i` is hidden by an overlay entry.
    pub fn hides(&self, i: usize) -> bool {
        self.hidden.binary_search(&(i as u32)).is_ok()
    }

    /// The file list index of entry `i`, None when untracked.
    pub(crate) fn base_index(&self, i: usize) -> Option<usize> {
        let at = *self.base.get(i)?;
        (at != UNTRACKED).then_some(at as usize)
    }

    pub(crate) fn lang_code(&self, i: usize) -> u8 {
        self.langs.get(i).copied().unwrap_or(0)
    }

    /// The overlay entries whose content is `oid`.
    pub(crate) fn find(&self, oid: &BlobId) -> impl Iterator<Item = usize> + '_ {
        let p = prefix(oid);
        let start = self.prefixes.partition_point(|x| *x < p);
        let oid = *oid;
        self.prefixes[start..]
            .iter()
            .take_while(move |x| **x == p)
            .zip(&self.indices[start..])
            .map(|(_, i)| *i as usize)
            .filter(move |i| self.entries[*i].1 == WorktreeState::Content(oid))
    }
}

/// The index of `path` in a file list.
pub(crate) fn find_path(files: &FileList, path: &str) -> Option<usize> {
    let (mut lo, mut hi) = (0, files.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        match files.get(mid) {
            Some((p, _)) if p < path => lo = mid + 1,
            _ => hi = mid,
        }
    }
    files.get(lo).filter(|(p, _)| *p == path).map(|_| lo)
}

/// What a hint is compared with besides the OID: the generations of
/// eviction and of the language packs (「输入代数与派生状态」).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HintStamp {
    pub eviction: u64,
    pub grammar: u64,
}

impl HintStamp {
    /// Never 0, so an unset slot never matches.
    fn word(self) -> u64 {
        ((self.eviction & 0xffff_ffff) << 32) | (self.grammar & 0x7fff_ffff) | 0x8000_0000
    }
}

/// Where a path's hint lives.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum HintSlot {
    /// A file list entry.
    Base(u32),
    /// A path the file list does not have (an untracked file).
    Extra(RelPath),
}

/// One hint per path: (prefix of the effective OID, eviction generation,
/// language pack generation), 16 bytes. Only a hint: it matches when the
/// path's current effective OID and both generations are the ones stored,
/// and is set again after an existence check hit or a write. Slots are
/// atomics so batches set them without copying; a torn read can only make
/// a hint look unset.
pub struct IndexedHints {
    slots: Box<[AtomicU64]>,
    extra: Mutex<HashMap<RelPath, (u64, u64)>>,
}

impl IndexedHints {
    pub fn new(len: usize) -> IndexedHints {
        IndexedHints {
            slots: (0..len * 2).map(|_| AtomicU64::new(0)).collect(),
            extra: Mutex::new(HashMap::new()),
        }
    }

    /// Hints for `new`, carried over from `old` path by path (both lists are
    /// in path order): a carried hint still matches only if the OID stayed.
    pub fn carried(old: &IndexedHints, old_files: &FileList, new_files: &FileList) -> IndexedHints {
        let hints = IndexedHints::new(new_files.len());
        let (mut i, mut j) = (0, 0);
        while let (Some((a, _)), Some((b, _))) = (old_files.get(i), new_files.get(j)) {
            match a.cmp(b) {
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
                std::cmp::Ordering::Equal => {
                    if let (Some(from), Some(to)) = (
                        old.slots.get(2 * i..2 * i + 2),
                        hints.slots.get(2 * j..2 * j + 2),
                    ) {
                        to[0].store(from[0].load(Ordering::Relaxed), Ordering::Relaxed);
                        to[1].store(from[1].load(Ordering::Relaxed), Ordering::Relaxed);
                    }
                    i += 1;
                    j += 1;
                }
            }
        }
        let extra = old
            .extra
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        *hints.extra.lock().unwrap_or_else(PoisonError::into_inner) = extra;
        hints
    }

    pub fn matches(&self, slot: &HintSlot, oid: &BlobId, stamp: HintStamp) -> bool {
        let want = (prefix(oid), stamp.word());
        match slot {
            HintSlot::Base(i) => {
                let i = *i as usize * 2;
                match self.slots.get(i..i + 2) {
                    Some(s) => {
                        s[1].load(Ordering::Acquire) == want.1
                            && s[0].load(Ordering::Acquire) == want.0
                    }
                    None => false,
                }
            }
            HintSlot::Extra(path) => self
                .extra
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(path)
                .is_some_and(|v| *v == want),
        }
    }

    pub fn set(&self, slot: &HintSlot, oid: &BlobId, stamp: HintStamp) {
        match slot {
            HintSlot::Base(i) => {
                let i = *i as usize * 2;
                if let Some(s) = self.slots.get(i..i + 2) {
                    // Clear the stamp first: a reader between the stores sees
                    // an unset hint, never the new prefix with an old stamp.
                    s[1].store(0, Ordering::Release);
                    s[0].store(prefix(oid), Ordering::Release);
                    s[1].store(stamp.word(), Ordering::Release);
                }
            }
            HintSlot::Extra(path) => {
                self.extra
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(path.clone(), (prefix(oid), stamp.word()));
            }
        }
    }

    /// Heap bytes held by the base slots.
    pub fn heap_bytes(&self) -> usize {
        self.slots.len() * 8
    }
}

/// A path's effective content: its overlay entry, or else its index entry.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Effective {
    pub path: RelPath,
    pub oid: BlobId,
    pub lang: Lang,
    pub slot: HintSlot,
}

/// One consistent view of the map. Queries and batches take it once and
/// use it throughout.
#[derive(Clone)]
pub struct LiveMap {
    pub base: Arc<BaseMap>,
    pub overlay: Arc<Overlay>,
    pub hints: Arc<IndexedHints>,
}

impl LiveMap {
    pub fn new(table: &LanguageTable) -> LiveMap {
        LiveMap {
            base: Arc::new(BaseMap::empty(table)),
            overlay: Arc::new(Overlay::empty()),
            hints: Arc::new(IndexedHints::new(0)),
        }
    }

    /// The effective content of a path, when it is indexable.
    pub fn effective(&self, path: &str) -> Option<Effective> {
        if let Some(i) = self.overlay.position(path) {
            let (path, state) = &self.overlay.entries[i];
            let WorktreeState::Content(oid) = state else {
                return None;
            };
            let lang = self.base.lang_of_code(self.overlay.lang_code(i))?.clone();
            let slot = match self.overlay.base_index(i) {
                Some(b) => HintSlot::Base(b as u32),
                None => HintSlot::Extra(path.clone()),
            };
            return Some(Effective {
                path: path.clone(),
                oid: *oid,
                lang,
                slot,
            });
        }
        let i = find_path(self.base.files(), path)?;
        self.base_effective(i)
    }

    /// The effective content of file list entry `i`, unless an overlay entry
    /// hides it.
    pub(crate) fn base_effective(&self, i: usize) -> Option<Effective> {
        if self.overlay.hides(i) {
            return None;
        }
        let oid = self.base.oid(i)?;
        let lang = self.base.lang(i)?.clone();
        let (path, _) = self.base.files().get(i)?;
        Some(Effective {
            path: path.into(),
            oid,
            lang,
            slot: HintSlot::Base(i as u32),
        })
    }

    /// The effective content of overlay entry `i` (a `Content` entry).
    pub(crate) fn overlay_effective(&self, i: usize) -> Option<Effective> {
        let (path, _) = self.overlay.entries.get(i)?;
        self.effective(path)
    }

    /// Every path whose effective content is `oid` in the language named
    /// `lang`: overlay entries, and index entries not hidden by one.
    pub fn paths(&self, oid: &BlobId, lang: &str) -> Vec<Effective> {
        let mut out: Vec<Effective> = self
            .overlay
            .find(oid)
            .filter_map(|i| self.overlay_effective(i))
            .chain(self.base.find(oid).filter_map(|i| self.base_effective(i)))
            .filter(|e| &*e.lang.name().0 == lang)
            .collect();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out
    }
}

/// The paths that differ from HEAD, from one worktree comparison, with the
/// generations of HEAD, the index and the file list it saw
/// (「依赖 git、fs_watch 与 cache 的接口」).
#[derive(Clone, Debug, Default)]
pub struct HeadDiff {
    /// Path order: each path's OID in HEAD, None when HEAD lacks it.
    entries: Box<[(RelPath, Option<BlobId>)]>,
    pub head: u64,
    pub index: u64,
    pub files: u64,
}

impl HeadDiff {
    pub(crate) fn new(
        mut entries: Vec<(RelPath, Option<BlobId>)>,
        head: u64,
        index: u64,
        files: u64,
    ) -> HeadDiff {
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries.dedup_by(|a, b| a.0 == b.0);
        HeadDiff {
            entries: entries.into(),
            head,
            index,
            files,
        }
    }

    /// The path's OID in HEAD when the comparison listed it: `Some(None)`
    /// when HEAD does not have it, None when the path was not listed (HEAD
    /// has the file list's OID).
    pub fn head_oid(&self, path: &str) -> Option<Option<BlobId>> {
        self.entries
            .binary_search_by(|(p, _)| (**p).cmp(path))
            .ok()
            .map(|i| self.entries[i].1)
    }
}
