//! The window chain of an opened large file (docs/design/code_intel.md
//! 「大文件」, ADR 0034): a file of 1–16 MiB opened (tier 1) is extracted
//! window by window in the batch tiers, before tier 3, under a
//! non-yielding guard; each window is a unit of its own, written as soon as
//! it is extracted, so a pause, a cancellation or an exit loses only the
//! window in flight.
//!
//! The start streams the file in 1 MiB ranges through `git::BlobHasher`
//! and checks the identity of every range: only the file whose OID is the
//! one to index goes on (its raw bytes', or, once the raw OID differs, the
//! whole file's `canonical_oid`, read once). Then the chain resumes at the
//! first gap of the windows stored, so evicted windows are filled again.
//! Every step holds the large slot (`LargeSlot`, taking it over from a
//! parked yielding task): a 1 MiB range, a window and its tree, or the
//! converted file's bytes, one at a time.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use lectern_cache::Written;
use lectern_git::BlobHasher;
use lectern_kernel::BackgroundGuard;
use lectern_platform::{FileIdentity, FullRead, OpenFile, Opened, RelPath as SafeRel, Timestamp};
use lectern_services::project::{BlobId, RelPath};

use super::batch::{Ctx, Done};
use super::state::{Item, Project, Source, Tier};
use super::{Inner, bump, lock};
use crate::LangName;
use crate::extract::{
    BACKGROUND_MAX_LEN, CutRule, Deadline, ExtractStatus, Extraction, Lang, Windowed,
    extract_window, is_binary,
};
use crate::git_access::{blob_id, hash_blob, hash_kind};
use crate::store::{Part, UnitMeta, Version, WindowRow};
use crate::util::log;
use crate::window::{Base, WIDE_WINDOW, WINDOW, ident_end, parse_len};

/// An opened file over this size is not extracted (ADR 0034).
pub const OPEN_MAX_LEN: u64 = 16 << 20;
/// The ranges the start hashes the file in.
pub const HASH_RANGE: u64 = 1 << 20;
/// A file whose status changed this close to the read waits this long
/// (the racy-timestamp rule of the dependency tier's stat table).
pub const RACY: Duration = Duration::from_secs(2);

/// One opened large file's chain.
pub(crate) struct Chain {
    pub path: RelPath,
    /// The OID to index: the map's, when the file was opened.
    pub oid: BlobId,
    pub lang: Lang,
    /// Not before this, on the watchdog's clock (a racy timestamp).
    pub not_before: Option<Instant>,
    /// Started: the file, as verified.
    pub open: Option<Open>,
}

impl Chain {
    pub fn key(&self) -> (BlobId, LangName) {
        (self.oid, self.lang.name().clone())
    }
}

/// A chain's file once its start verified it.
pub(crate) struct Open {
    file: Arc<OpenFile>,
    identity: FileIdentity,
    len: u64,
    /// The raw bytes are the OID's content (`UnitMeta::canonical`).
    canonical: bool,
    /// Where the next window starts.
    next: Base,
    /// A window of the chain timed out: no `Windowed` marker.
    timed_out: bool,
}

/// What one step of a chain did.
enum Step {
    /// Go on with the next window.
    Go(Open),
    /// The pause interrupted it: the same step again after the pause.
    Paused(Option<Open>),
    /// Not before this (a racy timestamp).
    Later(Instant),
    /// The chain is over: complete, or given up (the file changed, is gone,
    /// is not that content, the session ended).
    Over,
}

/// How long a file whose status changed at `ctime` waits at `now`: None
/// when the change is at least [`RACY`] old.
pub(crate) fn racy_wait(now: SystemTime, ctime: Timestamp) -> Option<Duration> {
    let changed = if ctime.secs >= 0 {
        SystemTime::UNIX_EPOCH + Duration::new(ctime.secs as u64, ctime.nanos)
    } else {
        SystemTime::UNIX_EPOCH
    };
    match now.duration_since(changed) {
        Ok(age) if age >= RACY => None,
        Ok(age) => Some(RACY - age),
        // A change in the future of this clock: as racy as can be.
        Err(_) => Some(RACY),
    }
}

/// Where a chain over `rows` (by start) resumes: the first gap's start,
/// the stored window that ends there (None at 0), and whether a window
/// on the way timed out.
pub(crate) fn resume_at(rows: &[WindowRow], from: u64) -> (u64, Option<WindowRow>, bool) {
    let mut at = from;
    let mut prev = None;
    let mut timed_out = false;
    for row in rows {
        if row.part.start == at && row.part.end > at {
            timed_out |= row.status != ExtractStatus::Done;
            at = row.part.end;
            prev = Some(*row);
        }
    }
    (at, prev, timed_out)
}

impl Inner {
    /// Queues the window chain of the opened file `item` (tier 1, the
    /// worktree's, a core language), once per key; the batch tasks pick
    /// it up first.
    pub(crate) fn enqueue_chain(
        self: &Arc<Self>,
        project: &Arc<Project>,
        session: u64,
        item: &Item,
    ) {
        if !matches!(item.source, Source::Worktree) || !item.lang.walks() {
            return;
        }
        {
            let mut st = lock(&project.state);
            if !st.live(session) || !st.batch.chain_keys.insert(item.key()) {
                return;
            }
            st.batch.chains.push_back(Chain {
                path: item.path.clone(),
                oid: item.oid,
                lang: item.lang.clone(),
                not_before: None,
                open: None,
            });
        }
        self.kick_batch();
    }

    /// Whether the project has a chain a batch task can step now.
    pub(super) fn chain_ready(&self, st: &super::state::State) -> bool {
        let now = self.watchdog.clock().now();
        st.batch
            .chains
            .iter()
            .any(|c| c.not_before.is_none_or(|t| t <= now))
    }

    /// One step of the first ready chain: its start, or one window. False
    /// when there was none.
    pub(super) fn chain_step(
        self: &Arc<Self>,
        project: &Arc<Project>,
        ctx: &Ctx,
        guard: &Arc<BackgroundGuard>,
    ) -> bool {
        let Some(mut chain) = self.take_chain(project, ctx) else {
            return false;
        };
        let stop = || guard.pause_requested() || ctx.cancel.load(Ordering::Acquire);
        // Non-yielding work: takes the slot over from a parked yielding
        // holder, whose saved tree goes first.
        let Some(turn) = self.large.acquire(stop, true, || self.wake_waiters()) else {
            self.put_chain(project, ctx, chain);
            return true;
        };
        let step = match chain.open.take() {
            None => self.start_chain(project, ctx, guard, &chain),
            Some(open) => self.window_step(project, ctx, guard, &chain, open),
        };
        drop(turn);
        match step {
            Step::Go(open) => {
                chain.open = Some(open);
                self.put_chain(project, ctx, chain);
            }
            Step::Paused(open) => {
                chain.open = open;
                self.put_chain(project, ctx, chain);
            }
            Step::Later(at) => {
                chain.not_before = Some(at);
                self.put_chain(project, ctx, chain);
                let me = Arc::downgrade(self);
                // The watchdog's thread only submits: the kick starts tasks.
                self.watchdog.schedule(
                    at,
                    Box::new(move || {
                        if let Some(me) = me.upgrade() {
                            me.kick_batch();
                        }
                    }),
                );
            }
            Step::Over => {
                lock(&project.state).batch.chain_keys.remove(&chain.key());
            }
        }
        true
    }

    /// Takes the first ready chain out of the queue.
    fn take_chain(&self, project: &Arc<Project>, ctx: &Ctx) -> Option<Chain> {
        let now = self.watchdog.clock().now();
        let mut st = lock(&project.state);
        if !st.live(ctx.session) {
            return None;
        }
        let i = st
            .batch
            .chains
            .iter()
            .position(|c| c.not_before.is_none_or(|t| t <= now))?;
        st.batch.chains.remove(i)
    }

    /// Puts a chain back at the front, unless its session ended.
    fn put_chain(&self, project: &Arc<Project>, ctx: &Ctx, chain: Chain) {
        let mut st = lock(&project.state);
        if st.live(ctx.session) {
            st.batch.chains.push_front(chain);
        }
    }

    /// The chain's start: the file opened and verified (streamed through
    /// `BlobHasher`, its identity the same in every range), then where its
    /// stored windows end.
    fn start_chain(
        self: &Arc<Self>,
        project: &Arc<Project>,
        ctx: &Ctx,
        guard: &Arc<BackgroundGuard>,
        chain: &Chain,
    ) -> Step {
        let paused = || guard.pause_requested() && !ctx.cancel.load(Ordering::Acquire);
        let Some(root) = &ctx.root else {
            return Step::Over;
        };
        let Some(rel) = SafeRel::new(chain.path.as_bytes()) else {
            return Step::Over;
        };
        let file = match root.open_file(rel) {
            Ok(Opened::File(file)) => file,
            _ => return Step::Over,
        };
        let meta = *file.meta();
        let len = meta.size;
        if len <= BACKGROUND_MAX_LEN as u64 || len > OPEN_MAX_LEN {
            return Step::Over;
        }
        if let Some(wait) = racy_wait(self.wall_now(), meta.ctime) {
            return Step::Later(self.watchdog.clock().now() + wait);
        }
        let identity = file.identity();
        let kind = hash_kind(&chain.oid);
        let mut hasher = BlobHasher::new(kind, len);
        let mut binary = false;
        let mut at = 0;
        while at < len {
            if paused() {
                return Step::Paused(None);
            }
            if ctx.cancel.load(Ordering::Acquire) {
                return Step::Over;
            }
            let end = (at + HASH_RANGE).min(len);
            let Some(piece) = read_range(&file, identity, at, end) else {
                return Step::Over;
            };
            if at == 0 {
                binary = is_binary(&piece);
            }
            hasher.update(&piece);
            at = end;
        }
        let raw = hasher.finish().map(|o| blob_id(&o));
        let canonical = if raw == Some(chain.oid) {
            true
        } else {
            // The whole content, once and in the large slot: only
            // `canonical_oid` (or `for_blob`, for content sha1dc flags)
            // tells whether these bytes are the OID's.
            let bytes = match file.read_all(OPEN_MAX_LEN) {
                Ok(FullRead::File { bytes, meta }) if meta.identity() == identity => bytes,
                _ => return Step::Over,
            };
            if raw.is_none() && hash_blob(kind, &bytes) == chain.oid {
                true
            } else {
                let Some(tree) = &ctx.tree else {
                    return Step::Over;
                };
                match self.git.canonical_oid(tree, &chain.path, &bytes) {
                    Ok(Some(oid)) if oid == chain.oid => false,
                    // Changed since it was opened (fs_watch brings the new
                    // OID), or filtered.
                    _ => return Step::Over,
                }
            }
        };
        if binary {
            // Not source: nothing to index by windows.
            return Step::Over;
        }
        let version = Version::of(&chain.lang);
        let rows = project
            .store
            .windows(chain.oid, &version)
            .unwrap_or_default();
        let (next, prev, timed_out) = resume_at(&rows, 0);
        let file = Arc::new(file);
        if next >= len {
            if !timed_out {
                self.write_marker(project, ctx, chain, len, canonical);
            }
            return Step::Over;
        }
        let Some(next) = base_after(&file, identity, prev) else {
            return Step::Over;
        };
        Step::Go(Open {
            file,
            identity,
            len,
            canonical,
            next,
            timed_out,
        })
    }

    /// One window: read, extracted and cut, written as a unit of its own.
    fn window_step(
        self: &Arc<Self>,
        project: &Arc<Project>,
        ctx: &Ctx,
        guard: &Arc<BackgroundGuard>,
        chain: &Chain,
        mut open: Open,
    ) -> Step {
        let paused = || guard.pause_requested() && !ctx.cancel.load(Ordering::Acquire);
        let (s, len) = (open.next.byte, open.len);
        let read = |from: u64, to: u64| read_range(&open.file, open.identity, from, to);
        let deadline = || {
            let guard = guard.clone();
            Deadline::background(&self.watchdog)
                .with_pause(Arc::new(move || guard.pause_requested()))
        };
        let flag = Arc::new(AtomicBool::new(ctx.cancel.load(Ordering::Acquire)));
        bump(&self.stats.windows, 1);
        let end = (s + WINDOW as u64).min(len);
        let Some(mut bytes) = read(s, end) else {
            // Changed while read: its new OID comes with fs_watch's event.
            return Step::Over;
        };
        // A window that is not the file's end is parsed up to its last
        // line break (`parse_len`).
        let (rule, mut parsed) = if end == len {
            (CutRule::Eof, bytes.len())
        } else {
            (CutRule::Tree, parse_len(&bytes))
        };
        let mut out = extract_window(&chain.lang, &bytes[..parsed], rule, &deadline(), &flag);
        // The cut at an identifier boundary at or after the window's end,
        // when no node or leaf gives one (or a window timed out).
        let mut far_end = None;
        if matches!(out, Windowed::NoCut) {
            let end = (s + WIDE_WINDOW as u64).min(len);
            let Some(wide) = read(s, end) else {
                return Step::Over;
            };
            bytes = wide;
            let rule;
            (rule, parsed) = if end == len {
                (CutRule::Eof, bytes.len())
            } else {
                (CutRule::TreeOrLeaf, parse_len(&bytes))
            };
            out = extract_window(&chain.lang, &bytes[..parsed], rule, &deadline(), &flag);
            if matches!(out, Windowed::NoCut) {
                let Some(cut) = ident_cut(&open, &bytes, s) else {
                    return Step::Over;
                };
                far_end = Some(cut);
                let at = usize::try_from(cut - s).unwrap_or(usize::MAX);
                out = extract_window(&chain.lang, &bytes, CutRule::At(at), &deadline(), &flag);
            }
        }
        let (extraction, end) = match out {
            Windowed::Cut(extraction, cut) => (extraction, far_end.unwrap_or(s + cut as u64)),
            Windowed::Stopped(ExtractStatus::TimedOut) => {
                // Written too, so the chain goes on after it (at the line
                // start it was parsed to, else at an identifier boundary);
                // the chain is not complete.
                open.timed_out = true;
                let end = if parsed < bytes.len() {
                    s + parsed as u64
                } else {
                    let Some(end) = ident_cut(&open, &bytes, s) else {
                        return Step::Over;
                    };
                    end
                };
                (Extraction::empty(ExtractStatus::TimedOut), end)
            }
            Windowed::Stopped(ExtractStatus::Cancelled) if paused() => {
                return Step::Paused(Some(open));
            }
            Windowed::Stopped(ExtractStatus::Unsupported) => {
                log(format_args!("windows of {} failed", chain.path));
                return Step::Over;
            }
            Windowed::Stopped(_) | Windowed::NoCut => return Step::Over,
        };
        let part = Part {
            start: s,
            end,
            line: open.next.line,
            column: open.next.column,
        };
        let base = open.next;
        let extraction = Extraction {
            defs: extraction.defs.into_iter().map(|d| base.def(d)).collect(),
            ..extraction
        };
        let taken = usize::try_from(end - s).unwrap_or(usize::MAX);
        let next = if taken <= bytes.len() {
            base.after(&bytes[..taken])
        } else {
            // Past the bytes read: identifier bytes only, no newline.
            let at = base.after(&bytes);
            Base {
                byte: end,
                line: at.line,
                column: at.column + (taken - bytes.len()) as u32,
            }
        };
        drop(bytes);
        if !self.commit_window(project, ctx, chain, part, &extraction, open.canonical) {
            return Step::Over;
        }
        #[cfg(test)]
        if let Some(hook) = lock(&self.window_hook).as_mut() {
            hook(end);
        }
        // Windows stored past this one (a chain resumed in a gap) are not
        // made again.
        let version = Version::of(&chain.lang);
        let rows = project
            .store
            .windows(chain.oid, &version)
            .unwrap_or_default();
        let (after, prev, timed_out) = resume_at(&rows, end);
        open.timed_out |= timed_out;
        if after >= len {
            if !open.timed_out {
                self.write_marker(project, ctx, chain, len, open.canonical);
            }
            return Step::Over;
        }
        open.next = if after == end {
            next
        } else {
            match base_after(&open.file, open.identity, prev) {
                Some(b) => b,
                None => return Step::Over,
            }
        };
        Step::Go(open)
    }

    /// Writes one window unit in its own transaction (「大文件」: each
    /// window is written as soon as it is extracted). No hint changes: the
    /// file is indexed only once its `Windowed` marker is written. A write
    /// that evicts other units sets the namespace full, as a batch's does.
    /// False when nothing was written (the session ended, the grammar
    /// changed, the write was dropped).
    fn commit_window(
        &self,
        project: &Arc<Project>,
        ctx: &Ctx,
        chain: &Chain,
        part: Part,
        extraction: &Extraction,
        canonical: bool,
    ) -> bool {
        let version = Version::of(&chain.lang);
        let current = lock(&self.langs)
            .table
            .get(&chain.lang.name().0)
            .is_some_and(|l| Version::of(l) == version);
        if !current || ctx.cancel.load(Ordering::Acquire) {
            return false;
        }
        let meta = UnitMeta {
            part: Some(part),
            canonical,
            len: None,
        };
        let (eviction, committed, after) = {
            let _turn = lock(&project.commits);
            let eviction = project.store.evictions();
            let mut write = project.store.write();
            if !write.add_unit(chain.oid, &version, extraction, meta) {
                return false;
            }
            let committed = matches!(write.commit(), Written::Committed);
            let after = project.store.evictions();
            let replaced = project.replaced.load(Ordering::Acquire);
            (
                eviction.saturating_sub(replaced),
                committed,
                after.saturating_sub(replaced),
            )
        };
        if !committed {
            return false;
        }
        {
            let mut st = lock(&project.state);
            if st.session != ctx.session {
                return false;
            }
            st.gens.writes += 1;
            if after != eviction && st.batch.full.is_none() {
                let walked = st
                    .batch
                    .cursor
                    .as_ref()
                    .map_or(eviction, |c| c.deps.eviction);
                st.batch.full = Some(walked);
            }
        }
        if let Some(me) = self.arc() {
            // A held name table is behind: the window's names are new.
            me.names_changed(project);
        }
        true
    }

    /// The chain reached the file's end without a window timing out: the
    /// whole extraction's row becomes the `Windowed` marker (replacing
    /// `TooLarge` or `TimedOut`, an own eviction), the hints of the
    /// file's paths are set, and tier 1 takes the file as indexed.
    fn write_marker(
        &self,
        project: &Arc<Project>,
        ctx: &Ctx,
        chain: &Chain,
        len: u64,
        canonical: bool,
    ) {
        let item = Item {
            path: chain.path.clone(),
            oid: chain.oid,
            lang: chain.lang.clone(),
            source: Source::Worktree,
            tier: Tier::Current,
        };
        let done = Done {
            item,
            key: chain.oid,
            check: None,
            extraction: Extraction::empty(ExtractStatus::Windowed),
            flag: Arc::new(AtomicBool::new(false)),
            meta: UnitMeta {
                part: None,
                canonical,
                len: Some(len),
            },
        };
        if self.commit(project, ctx, vec![done], true) {
            let mut st = lock(&project.state);
            if st.session == ctx.session {
                st.focused
                    .satisfied
                    .insert((chain.oid, Version::of(&chain.lang)));
            }
        }
    }
}

/// `[from, to)` of `file`, all of it, read while its identity is still
/// `identity`; None otherwise (changed, truncated, unreadable).
fn read_range(file: &OpenFile, identity: FileIdentity, from: u64, to: u64) -> Option<Vec<u8>> {
    let read = file.read_ranges(std::slice::from_ref(&(from..to))).ok()?;
    if read.meta.identity() != identity {
        return None;
    }
    let piece = read.pieces.into_iter().next()?;
    (piece.len() as u64 == to - from).then_some(piece)
}

/// Where the window after `prev` starts: its start's line and column plus
/// the newlines of its bytes, read in ranges of at most 1 MiB; the file's
/// start for None.
fn base_after(file: &OpenFile, identity: FileIdentity, prev: Option<WindowRow>) -> Option<Base> {
    let Some(prev) = prev else {
        return Some(Base::default());
    };
    let mut base = Base {
        byte: prev.part.start,
        line: prev.part.line,
        column: prev.part.column,
    };
    while base.byte < prev.part.end {
        let to = (base.byte + HASH_RANGE).min(prev.part.end);
        let bytes = read_range(file, identity, base.byte, to)?;
        base = base.after(&bytes);
    }
    Some(base)
}

/// The identifier boundary at or after the end of `bytes` (read from `s`):
/// the first byte that is not an identifier's, read on in windows past the
/// bytes; the file's end when it ends first.
fn ident_cut(open: &Open, bytes: &[u8], s: u64) -> Option<u64> {
    let mut at = s + bytes.len() as u64;
    loop {
        if at >= open.len {
            return Some(open.len);
        }
        let to = (at + WINDOW as u64).min(open.len);
        let more = read_range(&open.file, open.identity, at, to)?;
        if let Some(i) = ident_end(&more, 0) {
            return Some(at + i as u64);
        }
        at = to;
    }
}
