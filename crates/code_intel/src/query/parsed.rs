//! The parsed texts kept for jumps (「跳转到定义」 step 1, 「性能」, ADR
//! 0037): the whole `ParsedFile` of a current text (a site at any position,
//! its definitions, its binding model), keyed by its blob OID and its
//! language's version (architecture rule 5), so the next jump in the same
//! text skips the parse. Warmed in a `Background` task when a text becomes
//! the current one (`Focus::Current`) and after a jump or prefetch that
//! found none; a jump never waits for one.
//!
//! Bounds: at most [`ENTRIES`] texts of at most [`MAX_SOURCE`] bytes each,
//! their estimated bytes ([`estimate`]) at most [`BYTES`]; the current
//! text's entry stays while it is current, any other unused for
//! [`KEEP_FOR`] (from its last use or from when it stopped being current)
//! is dropped (the watchdog's due entry), and so is every entry of a
//! language the table replaced (its grammar's lease).

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use lectern_executor::Priority;
use lectern_services::code_intel::TextRef;
use lectern_services::project::{BlobId, ProjectKey};

use crate::LangName;
use crate::extract::{INTERACTIVE_DEADLINE, Lang};
use crate::index::{Inner, lock};
use crate::site::ParsedFile;
use crate::store::Version;

/// Texts kept at once: the current one and the one before it.
pub const ENTRIES: usize = 2;
/// The largest text kept (the 「性能」 jump row's 100 KB with room): past
/// it, every jump parses it for its site (the cold path).
pub const MAX_SOURCE: usize = 128 << 10;
/// The kept texts' estimated bytes ([`estimate`]) at most: two of the
/// largest.
pub const BYTES: usize = 12 << 20;
/// An entry unused this long is dropped.
pub const KEEP_FOR: Duration = Duration::from_secs(60);

/// A parsed text's estimated live bytes: its source times the measured
/// ratio of a `ParsedFile`'s bytes (the syntax tree, the binding model,
/// the definitions and reference positions) to its source.
pub fn estimate(source_len: usize) -> usize {
    source_len.saturating_mul(BYTES_PER_SOURCE_BYTE)
}

/// The ratio, rounded up: 46.6 measured for 100 KB of TypeScript
/// (`code_intel/parse_cache_bytes`, 「实际验证」).
const BYTES_PER_SOURCE_BYTE: usize = 48;

struct Kept {
    oid: BlobId,
    version: Version,
    parsed: Arc<ParsedFile>,
    bytes: usize,
    used: Instant,
}

/// The kept texts, the most recently used last.
#[derive(Default)]
pub(crate) struct ParseCache {
    kept: Vec<Kept>,
    /// The current text (the last `Focus::Current`) and its project: its
    /// entry never expires and is evicted last; [`KEEP_FOR`] counts from
    /// when it stops being the current one.
    current: Option<(ProjectKey, BlobId)>,
}

/// What a cache operation dropped: dropped by the caller outside the lock
/// (a `ParsedFile` may hold a replaced grammar's last lease).
pub(crate) type Dropped = Vec<Arc<ParsedFile>>;

impl ParseCache {
    /// The parse of the text `oid` in `version`, if kept; marks it used.
    pub fn get(&mut self, oid: BlobId, version: &Version, now: Instant) -> Option<Arc<ParsedFile>> {
        let i = self
            .kept
            .iter()
            .position(|k| k.oid == oid && k.version == *version)?;
        let mut kept = self.kept.remove(i);
        kept.used = now;
        let parsed = kept.parsed.clone();
        self.kept.push(kept);
        Some(parsed)
    }

    /// Whether the parse of `oid` in `version` is kept (tests, warming).
    pub fn has(&self, oid: BlobId, version: &Version) -> bool {
        self.kept
            .iter()
            .any(|k| k.oid == oid && k.version == *version)
    }

    /// Keeps `parsed` as the parse of `oid` in `version`, dropping the least
    /// recently used ones past the bounds. A text over [`MAX_SOURCE`] is not
    /// kept.
    pub fn put(
        &mut self,
        oid: BlobId,
        version: Version,
        parsed: Arc<ParsedFile>,
        now: Instant,
    ) -> Dropped {
        let mut dropped = Vec::new();
        let len = parsed.source().len();
        if len > MAX_SOURCE {
            return dropped;
        }
        if let Some(i) = self
            .kept
            .iter()
            .position(|k| k.oid == oid && k.version == version)
        {
            dropped.push(self.kept.remove(i).parsed);
        }
        self.kept.push(Kept {
            oid,
            version,
            parsed,
            bytes: estimate(len),
            used: now,
        });
        // The least recently used first, the current text's last.
        while self.kept.len() > ENTRIES || self.bytes() > BYTES {
            let at = self
                .kept
                .iter()
                .position(|k| !self.is_current(k.oid))
                .unwrap_or(0);
            dropped.push(self.kept.remove(at).parsed);
        }
        dropped
    }

    fn is_current(&self, oid: BlobId) -> bool {
        self.current.as_ref().is_some_and(|(_, c)| *c == oid)
    }

    /// `project`'s current text is now `oid` (None: it has none, or the
    /// project closed). The text that stops being current is marked used
    /// at `now`, so it is kept [`KEEP_FOR`] from here; a `None` for
    /// another project than the current text's changes nothing.
    pub fn set_current(&mut self, project: &ProjectKey, oid: Option<BlobId>, now: Instant) {
        let ends = match (&self.current, oid) {
            (Some((_, old)), Some(new)) => *old != new,
            (Some((p, _)), None) => p == project,
            (None, _) => false,
        };
        if ends && let Some((_, old)) = self.current.take() {
            for k in self.kept.iter_mut().filter(|k| k.oid == old) {
                k.used = now;
            }
        }
        if let Some(new) = oid {
            for k in self.kept.iter_mut().filter(|k| k.oid == new) {
                k.used = now;
            }
            self.current = Some((project.clone(), new));
        }
    }

    /// Drops the entries unused for [`KEEP_FOR`] at `now`, never the
    /// current text's; when the next one is due, if any is left.
    pub fn expire(&mut self, now: Instant) -> (Dropped, Option<Instant>) {
        let current = self.current.as_ref().map(|(_, c)| *c);
        let (gone, kept): (Vec<Kept>, Vec<Kept>) =
            std::mem::take(&mut self.kept).into_iter().partition(|k| {
                Some(k.oid) != current && now.saturating_duration_since(k.used) >= KEEP_FOR
            });
        self.kept = kept;
        let next = self
            .kept
            .iter()
            .filter(|k| Some(k.oid) != current)
            .map(|k| k.used + KEEP_FOR)
            .min();
        (gone.into_iter().map(|k| k.parsed).collect(), next)
    }

    /// Drops the entries of the languages `changed` (the table replaced
    /// their grammar or removed them).
    pub fn without(&mut self, changed: &[LangName]) -> Dropped {
        let (gone, kept): (Vec<Kept>, Vec<Kept>) = std::mem::take(&mut self.kept)
            .into_iter()
            .partition(|k| changed.contains(&k.version.lang));
        self.kept = kept;
        gone.into_iter().map(|k| k.parsed).collect()
    }

    /// Drops every entry (tests and benchmarks).
    #[cfg(any(test, feature = "test-support"))]
    pub fn without_all(&mut self) -> Dropped {
        std::mem::take(&mut self.kept)
            .into_iter()
            .map(|k| k.parsed)
            .collect()
    }

    /// The kept texts' estimated bytes.
    pub fn bytes(&self) -> usize {
        self.kept.iter().map(|k| k.bytes).sum()
    }

    /// Texts kept now.
    #[cfg_attr(
        not(any(test, feature = "test-support")),
        allow(dead_code, reason = "read by the tests and benchmarks")
    )]
    pub fn len(&self) -> usize {
        self.kept.len()
    }
}

impl Inner {
    /// The kept parse of the text `oid` in `version`, if any.
    pub(crate) fn cached_parse(&self, oid: BlobId, version: &Version) -> Option<Arc<ParsedFile>> {
        let now = self.watchdog.clock().now();
        lock(&self.parse_cache).get(oid, version, now)
    }

    /// Parses the text `at` of `project` in a `Background` task and keeps
    /// it, unless it is kept already, cannot be read as that content, is over
    /// [`MAX_SOURCE`] or its language is not enabled. Returns at once; any
    /// thread (`set_focus` runs on the UI thread). After a first screen's
    /// `Interactive` work, and never before a project is open.
    pub(crate) fn warm_parse(self: &Arc<Self>, project: &ProjectKey, at: &TextRef) {
        if self.project(project).is_none() || !self.warms() {
            return;
        }
        let me = self.clone();
        let (project, at) = (project.clone(), at.clone());
        self.host
            .spawn_busy("warm parse", Priority::Background, move || {
                let Some(snap) = me.snap(&project) else {
                    return;
                };
                let Some(lang) = snap
                    .map
                    .effective(&at.path)
                    .map(|e| e.lang)
                    .or_else(|| snap.table.for_path(&at.path, &*me.packs).cloned())
                    .filter(|l| !snap.disabled.contains(l.name()))
                else {
                    return;
                };
                if lock(&me.parse_cache).has(at.oid, &Version::of(&lang)) {
                    return;
                }
                if let Ok(bytes) = me.read_text(&snap, &at) {
                    me.keep_parse(&lang, at.oid, bytes);
                }
            });
    }

    /// [`warm_parse`](Self::warm_parse) of bytes a query already read and
    /// verified as the text `oid` (a jump's cold path): parsed in a
    /// `Background` task, nothing read again.
    pub(crate) fn warm_parse_of(self: &Arc<Self>, lang: &Lang, oid: BlobId, bytes: Arc<[u8]>) {
        if bytes.len() > MAX_SOURCE || !self.warms() {
            return;
        }
        let me = self.clone();
        let lang = lang.clone();
        self.host
            .spawn_busy("warm parse", Priority::Background, move || {
                me.keep_parse(&lang, oid, bytes);
            });
    }

    /// Whether texts are warmed (tests may turn it off: they count the
    /// watchdog's due entries).
    fn warms(&self) -> bool {
        #[cfg(test)]
        {
            !self.no_warm.load(std::sync::atomic::Ordering::Acquire)
        }
        #[cfg(not(test))]
        {
            true
        }
    }

    /// Parses `bytes` (the text `oid`) whole and keeps the parse, while
    /// `lang` is its language's current version: checked under the cache's
    /// lock, so a parse of a grammar the table replaced meanwhile is never
    /// kept (the table changes before the cache drops that language's
    /// entries).
    fn keep_parse(self: &Arc<Self>, lang: &Lang, oid: BlobId, bytes: Arc<[u8]>) {
        let version = Version::of(lang);
        if bytes.len() > MAX_SOURCE || lock(&self.parse_cache).has(oid, &version) {
            return;
        }
        let clock = self.watchdog.clock().clone();
        let cancel = AtomicBool::new(false);
        let deadline = clock.now() + INTERACTIVE_DEADLINE;
        let Ok(parsed) = ParsedFile::parse(lang, bytes, &*clock, deadline, &cancel) else {
            return;
        };
        let now = clock.now();
        let dropped = {
            let mut cache = lock(&self.parse_cache);
            let current = lock(&self.langs)
                .table
                .get(&lang.name().0)
                .is_some_and(|l| Version::of(l) == version);
            if !current {
                drop(cache);
                // Its grammar's lease goes with it, outside the lock.
                drop(parsed);
                return;
            }
            cache.put(oid, version, Arc::new(parsed), now)
        };
        // Outside the lock: a dropped parse may hold a grammar's last lease.
        drop(dropped);
        self.expire_parses();
    }

    /// `project`'s current text is now `oid` (`Focus::Current`; None when
    /// it has none or closed): its kept parse stays while it is current,
    /// and the one that stops being current gets its [`KEEP_FOR`] from now.
    pub(crate) fn focus_parse(self: &Arc<Self>, project: &ProjectKey, oid: Option<BlobId>) {
        let now = self.watchdog.clock().now();
        lock(&self.parse_cache).set_current(project, oid, now);
        self.expire_parses();
    }

    /// Drops the kept parses unused for [`KEEP_FOR`] (never the current
    /// text's) and schedules a due entry for the next one, so none outlives
    /// its use, or its being current, by more than that.
    pub(crate) fn expire_parses(self: &Arc<Self>) {
        let now = self.watchdog.clock().now();
        let (dropped, next) = lock(&self.parse_cache).expire(now);
        if !dropped.is_empty() {
            self.drop_later(dropped);
        }
        let Some(at) = next else {
            return;
        };
        {
            let mut due = lock(&self.parse_expiry);
            if due.is_some_and(|d| d <= at) {
                return;
            }
            *due = Some(at);
        }
        let me = Arc::downgrade(self);
        // The watchdog's thread only submits: the drop runs in a task.
        self.watchdog.schedule(
            at,
            Box::new(move || {
                let Some(me) = me.upgrade() else {
                    return;
                };
                {
                    let mut due = lock(&me.parse_expiry);
                    if *due == Some(at) {
                        *due = None;
                    }
                }
                let task = me.clone();
                me.host
                    .spawn_busy("parse expiry", Priority::Background, move || {
                        task.expire_parses();
                    });
            }),
        );
    }
}
