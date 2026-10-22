//! Edit mode's domain part (docs/design/file_reader.md「编辑」): an
//! [`EditSession`] holds the buffer's text as the view last gave it, its
//! spans, a tree for the incremental parse, and the merge base with the
//! file's identity on disk.
//!
//! Two lanes of work, each one task at a time, every task holding a
//! `BusyGuard` at `Interactive`:
//!
//! - the parse: rounds of [`EditParse`] in 8 ms slices, while the buffer is
//!   ahead of the last round's text. Edits that arrive during a round are
//!   kept and the round's tokens are moved by them when it lands, so a
//!   round always finishes with the text it started from;
//! - the disk, in order: saves, merges and the checks after fs_watch events
//!   (so a check never sees half of a save), and the close.
//!
//! The UI thread only calls [`EditSession::changed`], [`spans`](EditSession::spans)
//! and [`state`](EditSession::state) (a lock, an `Arc`, and moving the
//! spans by the edit) and queues the rest.

use std::collections::VecDeque;
use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use lectern_executor::Priority;
use lectern_git::{Markers, merge3};
use lectern_platform::{
    FileIdentity, FullRead, Opened, RelPath, ReplaceError, SafeError, SafeRoot,
};
use lectern_services::project::ProjectKey;

use crate::change::{self, Format, TextEdit};
use crate::highlight::{Budget, EditParse, Outcome};
use crate::limits::{SLICE, TIER_B_MAX};
use crate::spans::WindowSpans;
use crate::util::{lock, log, yield_now};

use super::content::Content;
use super::engine::{Doc, EngineInner};
use super::{Editable, Highlighting, NotEditable, watch};

/// The labels of a merge's conflict markers: file content, not interface
/// strings (they are not translated).
const MARKERS: Markers<'static> = Markers {
    ours: "editor",
    theirs: "disk",
};

/// An edit session of a document ([`DocHandle::edit`](super::DocHandle::edit)).
/// Dropping the last handle ends it: its tree is released off the calling
/// thread, and the document opens the file again when it was saved or
/// changed on disk meanwhile.
pub struct EditSession {
    session: Arc<Session>,
    /// The buffer's text when this handle was made: the text at entering
    /// edit for the first, the session's current buffer for another.
    initial: Arc<str>,
}

/// What the disk side found, for the view. Each result stays until the
/// next of its kind; the counters tell a new one from the last.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct EditState {
    /// The file on disk is not the merge base any more: an event found
    /// other content, or a save found another identity.
    pub dirty_on_disk: bool,
    /// The file on disk cannot be read as the buffer's text (gone, not a
    /// regular file, not UTF-8): only copying the buffer or discarding it
    /// is left (`file-reader-unavailable-while-editing`).
    pub unavailable: bool,
    /// The saves finished, whatever their result; `save` is the last's.
    pub saves: u64,
    pub save: Option<SaveResult>,
    /// The merges finished; `merged` is the last's, None when it could not
    /// read the file (`unavailable`).
    pub merges: u64,
    pub merged: Option<Arc<MergeOutcome>>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SaveResult {
    Saved,
    /// The file's identity on disk is not the one last read or saved:
    /// nothing was written.
    Conflict,
    Failed(Arc<str>),
}

/// A three-way merge's result for the view to put in the buffer, still
/// unsaved.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MergeOutcome {
    pub text: Arc<str>,
    /// Byte ranges in `text`, each from a `<<<<<<< editor` line to the end
    /// of its `>>>>>>> disk` line; empty when the merge is clean.
    pub conflicts: Arc<[Range<usize>]>,
}

pub(crate) struct Session {
    engine: Weak<EngineInner>,
    doc: Weak<Doc>,
    project: ProjectKey,
    root: Arc<Path>,
    path: Arc<str>,
    lang: Option<&'static str>,
    /// The file's format at entering edit (the reader's spans are of it).
    format: Format,
    initial: Arc<str>,
    /// The root's `SafeRoot` when the session began, if the engine had it:
    /// saving still works after the project closed.
    safe: Option<Arc<SafeRoot>>,
    notify: Arc<dyn Fn() + Send + Sync>,
    handles: AtomicUsize,
    closed: AtomicBool,
    /// The close went through the disk queue: the saves queued before it
    /// are done and the document has checked the file.
    finished: AtomicBool,
    cancel: Arc<AtomicBool>,
    st: Mutex<SessionMut>,
    /// Runs once on the parse's thread right after the next round began
    /// (tests: edits that arrive during a round).
    #[cfg(test)]
    pub(crate) during_round: Mutex<Option<RoundHook>>,
}

/// Gets the session whose round just began.
#[cfg(test)]
pub(crate) type RoundHook = Box<dyn FnOnce(&Arc<Session>) + Send>;

struct SessionMut {
    /// The buffer as the view last gave it.
    text: Arc<str>,
    /// Its spans: the last round's moved by the edits since.
    shown: Arc<WindowSpans>,
    /// The edit from the last round's text to `text` (from the initial
    /// text before the first round); during a round, from the round's text.
    pending: Option<TextEdit>,
    /// `text` has not been parsed (the first round is still to come).
    stale: bool,
    parse_busy: bool,
    /// Between rounds; the parse task holds it during one.
    parse: Option<EditParse>,
    /// No highlighting: no language, or its grammar is unusable.
    plain: bool,
    /// The reader's spans of the file's raw bytes, moved onto the buffer at
    /// the first round (a BOM or `\r\n` line endings).
    raw: Option<(Arc<WindowSpans>, Arc<[u8]>)>,
    /// The merge base: the text at entering edit, last saved or merged.
    base: Arc<str>,
    /// What a save puts back (the BOM, the line endings): the file's at
    /// entering edit, the disk's after a merge.
    format: Format,
    /// The file's identity when it was last read, saved or merged.
    identity: Option<FileIdentity>,
    ops: VecDeque<Op>,
    io_busy: bool,
    /// A save went through: the document reads the file again at the end.
    saved: bool,
    state: Arc<EditState>,
}

enum Op {
    Save {
        text: Arc<str>,
        force: bool,
    },
    Merge {
        ours: Arc<str>,
    },
    /// An fs_watch event for the file.
    Check,
    /// The last handle dropped: the document checks the file.
    Close,
}

/// [`DocHandle::edit`](super::DocHandle::edit): a new session, or another
/// handle to the live one.
pub(crate) fn begin(engine: &Arc<EngineInner>, doc: &Arc<Doc>) -> Result<EditSession, NotEditable> {
    let (session, initial) = {
        let mut st = doc.lock();
        if let Editable::No(why) = st.editable {
            return Err(why);
        }
        if let Some(live) = st.edit.as_ref().and_then(Weak::upgrade) {
            if !live.is_closed() {
                // Another handle starts from the buffer as it is now: its
                // `changed` edits are from that text.
                live.handles.fetch_add(1, Ordering::SeqCst);
                let initial = live.lock().text.clone();
                return Ok(EditSession {
                    session: live,
                    initial,
                });
            }
            if !live.finished.load(Ordering::SeqCst) {
                // Its saves are still to be written and the file read
                // again: a session now would start from the old content.
                return Err(NotEditable::Reloading);
            }
        }
        let Content::Text(text) = &st.content else {
            return Err(NotEditable::NotLoaded);
        };
        let Some((initial, format)) = Format::decode(text.as_bytes()) else {
            return Err(NotEditable::NotText);
        };
        let len = initial.len() as u64;
        let plain = st.lang.is_none() || st.highlight == Highlighting::Plain;
        let empty = || Arc::new(WindowSpans::empty(len));
        // Meanwhile the reader's whole-file spans, as they are when the
        // bytes are the buffer's, else moved onto it by the first round.
        let (shown, raw) = match &st.whole {
            Some(whole) if !plain && format.plain() => (whole.clone(), None),
            Some(whole) if !plain => (empty(), Some((whole.clone(), text.shared_bytes().clone()))),
            _ => (empty(), None),
        };
        let session = Arc::new(Session {
            engine: Arc::downgrade(engine),
            doc: Arc::downgrade(doc),
            project: doc.target.project.clone(),
            root: doc.root.clone(),
            path: doc.target.path.clone(),
            lang: st.lang,
            format,
            initial: initial.clone(),
            safe: engine.cached_root(&doc.target.project, &doc.root),
            notify: doc.notifier(),
            handles: AtomicUsize::new(1),
            closed: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            cancel: Arc::new(AtomicBool::new(false)),
            st: Mutex::new(SessionMut {
                text: initial.clone(),
                shown,
                pending: None,
                stale: true,
                parse_busy: false,
                parse: None,
                plain,
                raw,
                base: initial.clone(),
                format,
                identity: st.identity,
                ops: VecDeque::new(),
                io_busy: false,
                saved: false,
                state: Arc::default(),
            }),
            #[cfg(test)]
            during_round: Mutex::new(None),
        });
        st.edit = Some(Arc::downgrade(&session));
        (session, initial)
    };
    session.kick_parse(engine);
    Ok(EditSession { session, initial })
}

impl EditSession {
    /// The buffer's text for `InputState::set_value`: the text at entering
    /// edit (no BOM, `\r\n` as `\n`), or for another handle to a live
    /// session the buffer as the session last got it.
    pub fn initial_text(&self) -> Arc<str> {
        self.initial.clone()
    }

    /// The buffer changed: its new text and the edits from the text of the
    /// last call (or [`initial_text`](Self::initial_text)), in order.
    /// Returns at once: the spans move by the edits now, the incremental
    /// parse runs in an `Interactive` task. Edits that do not fit the two
    /// texts' lengths are replaced by the texts' difference.
    pub fn changed(&self, text: Arc<str>, edits: &[TextEdit]) {
        self.session.changed(text, edits);
    }

    /// The latest spans of the buffer: the last parse's, moved by the edits
    /// that came after it. A lock and an `Arc`; no parse.
    pub fn spans(&self) -> Arc<WindowSpans> {
        self.session.lock().shown.clone()
    }

    /// Saves `text`: the BOM and the line endings put back, the file
    /// replaced atomically (`SafeRoot::replace_file`) if its identity is
    /// still the one last read, saved or merged. Otherwise nothing is written
    /// and the result is [`SaveResult::Conflict`]. Returns at once; the result
    /// comes in [`state`](Self::state), after a call to the document's
    /// `notify`.
    pub fn save(&self, text: Arc<str>) {
        self.session.queue(Op::Save { text, force: false });
    }

    /// The user chose Save Anyway: replaced atomically without comparing the
    /// identity.
    pub fn save_anyway(&self, text: Arc<str>) {
        self.session.queue(Op::Save { text, force: true });
    }

    /// Merges three ways: the base (the text at entering edit, or last saved
    /// or merged), `ours` (the buffer) and the file on disk now. The result
    /// comes in [`state`](Self::state); the file on disk becomes the base.
    pub fn merge(&self, ours: Arc<str>) {
        self.session.queue(Op::Merge { ours });
    }

    pub fn state(&self) -> Arc<EditState> {
        self.session.lock().state.clone()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl EditSession {
    /// No parse or disk work in flight or waiting (tests and benchmarks).
    pub fn idle(&self) -> bool {
        let st = self.session.lock();
        !st.parse_busy && !st.io_busy && (st.plain || !st.stale)
    }
}

#[cfg(test)]
impl EditSession {
    pub(crate) fn session(&self) -> &Arc<Session> {
        &self.session
    }
}

#[cfg(test)]
impl Session {
    pub(crate) fn spans(&self) -> Arc<WindowSpans> {
        self.lock().shown.clone()
    }

    pub(crate) fn state(&self) -> Arc<EditState> {
        self.lock().state.clone()
    }
}

impl Drop for EditSession {
    fn drop(&mut self) {
        if self.session.handles.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.session.close();
        }
    }
}

impl Session {
    fn lock(&self) -> MutexGuard<'_, SessionMut> {
        lock(&self.st)
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub(crate) fn changed(self: &Arc<Self>, text: Arc<str>, edits: &[TextEdit]) {
        let kick = {
            let mut st = self.lock();
            let last = std::mem::replace(&mut st.text, text.clone());
            let (old, new) = (last.as_bytes(), text.as_bytes());
            let Some(edit) =
                change::compose_all(edits, old.len(), new.len()).or_else(|| change::diff(old, new))
            else {
                return;
            };
            st.pending = Some(change::compose_opt(st.pending, edit));
            st.stale = true;
            st.shown = Arc::new(st.shown.shifted(edit, old, new));
            !st.plain && !st.parse_busy
        };
        if kick && let Some(engine) = self.engine.upgrade() {
            self.kick_parse(&engine);
        }
    }

    /// Starts the parse task unless it runs or there is nothing to parse.
    fn kick_parse(self: &Arc<Self>, engine: &Arc<EngineInner>) {
        {
            let mut st = self.lock();
            if st.parse_busy || st.plain || !st.stale || self.is_closed() {
                return;
            }
            st.parse_busy = true;
        }
        let (engine2, session) = (engine.clone(), self.clone());
        let spawned = engine.spawn(Priority::Interactive, async move {
            parse(&engine2, &session).await;
        });
        if !spawned {
            self.lock().parse_busy = false;
        }
    }

    fn queue(self: &Arc<Self>, op: Op) {
        let start = {
            let mut st = self.lock();
            if matches!(op, Op::Check) && st.ops.iter().any(|o| matches!(o, Op::Check)) {
                return;
            }
            st.ops.push_back(op);
            !std::mem::replace(&mut st.io_busy, true)
        };
        if start {
            self.start_disk();
        }
    }

    /// An fs_watch event for the file (from the document's sink).
    pub(crate) fn disk_event(self: &Arc<Self>) {
        self.queue(Op::Check);
    }

    fn start_disk(self: &Arc<Self>) {
        let session = self.clone();
        let spawned = self.engine.upgrade().is_some_and(|engine| {
            let engine2 = engine.clone();
            engine.spawn(Priority::Interactive, async move {
                disk(&engine2, &session);
            })
        });
        if !spawned {
            // The engine stopped: the saves waiting fail, so a view waiting
            // for them goes on.
            let mut st = self.lock();
            st.io_busy = false;
            let ops: Vec<Op> = st.ops.drain(..).collect();
            if ops.iter().any(|op| matches!(op, Op::Close)) {
                self.finished.store(true, Ordering::SeqCst);
            }
            let saves = ops
                .iter()
                .filter(|op| matches!(op, Op::Save { .. }))
                .count();
            if saves > 0 {
                let mut state = (*st.state).clone();
                state.saves += saves as u64;
                state.save = Some(SaveResult::Failed("the reader stopped".into()));
                st.state = Arc::new(state);
                drop(st);
                (self.notify)();
            }
        }
    }

    /// The last handle dropped (any thread, the UI thread included): the
    /// parse stops, the tree goes to a task to be dropped. The saves already
    /// queued are still written (merges and checks are dropped); after them
    /// the document checks the file, and only then may a new session begin.
    fn close(self: &Arc<Self>) {
        self.closed.store(true, Ordering::SeqCst);
        self.cancel.store(true, Ordering::SeqCst);
        let (tree, writing, start) = {
            let mut st = self.lock();
            st.ops.retain(|op| matches!(op, Op::Save { .. }));
            // A save done, queued, or perhaps in flight.
            let writing = st.saved || st.io_busy || !st.ops.is_empty();
            st.ops.push_back(Op::Close);
            (
                st.parse.take(),
                writing,
                !std::mem::replace(&mut st.io_busy, true),
            )
        };
        // The file is likely read again: not editable until the close is
        // through (the close puts it back when nothing was read again).
        if writing && let Some(doc) = self.doc.upgrade() {
            let mut st = doc.lock();
            if st.editable == Editable::Yes {
                st.editable = Editable::No(NotEditable::Reloading);
                doc.commit(st);
            }
        }
        if let Some(engine) = self.engine.upgrade() {
            let _ = engine.spawn(Priority::Background, async move { drop(tree) });
        }
        if start {
            self.start_disk();
        }
    }

    fn publish(&self, mut st: MutexGuard<'_, SessionMut>, change: impl FnOnce(&mut EditState)) {
        let mut state = (*st.state).clone();
        change(&mut state);
        st.state = Arc::new(state);
        drop(st);
        (self.notify)();
    }

    /// The root's `SafeRoot`: the one taken at the start, else the
    /// engine's (blocking).
    fn safe_root(&self, engine: &EngineInner) -> Result<Arc<SafeRoot>, Arc<str>> {
        match &self.safe {
            Some(safe) => Ok(safe.clone()),
            None => engine
                .root(&self.project, &self.root)
                .map_err(|e| e.to_string().into()),
        }
    }

    fn rel(&self) -> Result<RelPath<'_>, Arc<str>> {
        RelPath::new(self.path.as_bytes()).ok_or_else(|| "not a valid relative path".into())
    }
}

/// The parse task: rounds until the buffer's text is parsed.
async fn parse(engine: &Arc<EngineInner>, session: &Arc<Session>) {
    let langs = engine.languages.clone();
    let Some(lang) = session.lang.and_then(|name| langs.get(name)) else {
        session.lock().parse_busy = false;
        return;
    };
    loop {
        let (parse, text, edit, raw) = {
            let mut st = session.lock();
            if session.is_closed() || st.plain || !st.stale {
                st.parse_busy = false;
                return;
            }
            st.stale = false;
            (
                st.parse.take(),
                st.text.clone(),
                st.pending.take(),
                st.raw.take(),
            )
        };
        if let Some((spans, bytes)) = raw {
            // The first round of a file whose bytes are not the buffer's:
            // the reader's spans moved onto the buffer meanwhile.
            let initial = session.initial.as_bytes();
            let decoded = spans.decoded(&bytes, session.format, initial.len() as u64);
            let decoded = match edit {
                Some(edit) => decoded.shifted(edit, initial, text.as_bytes()),
                None => decoded,
            };
            let mut st = session.lock();
            st.shown = Arc::new(match st.pending {
                Some(since) => decoded.shifted(since, text.as_bytes(), st.text.as_bytes()),
                None => decoded,
            });
            drop(st);
            (session.notify)();
        }
        // Compiling the query on first use blocks here, on the executor.
        let mut parse = parse.unwrap_or_else(|| EditParse::new(lang));
        parse.begin(text.clone(), edit);
        #[cfg(test)]
        if let Some(hook) = lock(&session.during_round).take() {
            hook(session);
        }
        let cancel = session.cancel.clone();
        let outcome = loop {
            let budget = Budget {
                budget: SLICE,
                cancel: &cancel,
                yield_probe: None,
            };
            match parse.step(budget) {
                Outcome::Done(spans) => break Some(spans),
                Outcome::TimedOut | Outcome::Yielded => {
                    yield_now().await;
                    if session.is_closed() {
                        break None;
                    }
                }
                Outcome::Cancelled | Outcome::Unavailable => break None,
            }
        };
        let mut st = session.lock();
        let Some(spans) = outcome else {
            st.parse_busy = false;
            if !session.is_closed() {
                // The grammar version is disabled, or the query does not
                // compile: plain text from now on.
                st.plain = true;
                st.shown = Arc::new(WindowSpans::empty(st.text.len() as u64));
                drop(st);
                (session.notify)();
            }
            return;
        };
        // Edits that came during the round: from its text to the buffer.
        st.shown = match st.pending {
            Some(since) => Arc::new(spans.shifted(since, text.as_bytes(), st.text.as_bytes())),
            None => spans,
        };
        st.parse = Some(parse);
        drop(st);
        (session.notify)();
    }
}

/// The disk task: the queued operations in order.
fn disk(engine: &Arc<EngineInner>, session: &Arc<Session>) {
    loop {
        let op = {
            let mut st = session.lock();
            match st.ops.pop_front() {
                Some(op) => op,
                None => {
                    st.io_busy = false;
                    return;
                }
            }
        };
        match op {
            Op::Save { text, force } => save(engine, session, text, force),
            Op::Merge { ours } => merge(engine, session, &ours),
            Op::Check => check(engine, session),
            Op::Close => {
                // The saves queued before the close are written. The
                // reading view gets what is on disk now: another version
                // after a save, or the agent's.
                if let Some(doc) = session.doc.upgrade() {
                    let load = doc.lock().load;
                    watch::check(engine, &doc);
                    let mut st = doc.lock();
                    // Not read again (no save went through, nothing
                    // changed): editable as before.
                    if st.load == load && st.editable == Editable::No(NotEditable::Reloading) {
                        st.editable = Editable::Yes;
                        doc.commit(st);
                    }
                }
                session.finished.store(true, Ordering::SeqCst);
            }
        }
    }
}

fn save(engine: &EngineInner, session: &Session, text: Arc<str>, force: bool) {
    let (format, identity) = {
        let st = session.lock();
        (st.format, st.identity)
    };
    let bytes = format.encode(&text);
    let expect = if force { None } else { identity };
    let replaced = session.safe_root(engine).and_then(|safe| {
        let rel = session.rel()?;
        Ok(safe.replace_file(rel, &bytes, expect))
    });
    let mut st = session.lock();
    let (result, dirty, unavailable) = match replaced {
        Ok(Ok(identity)) => {
            st.identity = Some(identity);
            st.base = text;
            st.saved = true;
            (SaveResult::Saved, false, false)
        }
        Ok(Err(ReplaceError::Changed { .. })) => (SaveResult::Conflict, true, false),
        Ok(Err(ReplaceError::NotFound)) => (
            SaveResult::Failed("the file no longer exists".into()),
            true,
            true,
        ),
        Ok(Err(e)) => (
            SaveResult::Failed(e.to_string().into()),
            st.state.dirty_on_disk,
            st.state.unavailable,
        ),
        Err(e) => (
            SaveResult::Failed(e),
            st.state.dirty_on_disk,
            st.state.unavailable,
        ),
    };
    session.publish(st, |state| {
        state.saves += 1;
        state.save = Some(result);
        state.dirty_on_disk = dirty;
        state.unavailable = unavailable;
    });
}

/// Why the file on disk could not be read as the buffer's text.
enum Unreadable {
    Gone,
    NotText,
    Failed(Arc<str>),
}

/// The file as read from disk.
struct Disk {
    bytes: Vec<u8>,
    identity: FileIdentity,
}

impl Disk {
    /// As the buffer's text and the format around it; None when the bytes
    /// are not UTF-8.
    fn decode(&self) -> Option<(Arc<str>, Format)> {
        Format::decode(&self.bytes)
    }
}

/// The file on disk, with the identity it was read with. Blocking.
fn read_disk(engine: &EngineInner, session: &Session) -> Result<Disk, Unreadable> {
    let safe = session.safe_root(engine).map_err(Unreadable::Failed)?;
    let rel = session.rel().map_err(Unreadable::Failed)?;
    let file = match safe.open_file(rel) {
        Ok(Opened::File(file)) => file,
        Ok(Opened::Symlink { .. } | Opened::Unsupported { .. }) | Err(SafeError::NotFound) => {
            return Err(Unreadable::Gone);
        }
        Err(e) => return Err(Unreadable::Failed(e.to_string().into())),
    };
    match file.read_all(TIER_B_MAX) {
        Ok(FullRead::File { bytes, meta }) => Ok(Disk {
            bytes,
            identity: meta.identity(),
        }),
        Ok(FullRead::TooLarge { .. }) => Err(Unreadable::NotText),
        Err(e) => Err(Unreadable::Failed(e.to_string().into())),
    }
}

/// The merge takes the disk's format too: the merged text is saved with
/// the BOM and line endings the file has now (a `dos2unix` by the agent
/// stays done).
fn merge(engine: &EngineInner, session: &Session, ours: &str) {
    let theirs = read_disk(engine, session).and_then(|disk| {
        let (text, format) = disk.decode().ok_or(Unreadable::NotText)?;
        Ok((text, format, disk.identity))
    });
    // Merged outside the lock: the UI thread's calls do not wait for it.
    let base = session.lock().base.clone();
    let outcome = match theirs {
        Ok((theirs, format, identity)) => {
            let merged = merge3(
                base.as_bytes(),
                ours.as_bytes(),
                theirs.as_bytes(),
                &MARKERS,
            );
            // Merged by lines, and `\n` never falls inside a UTF-8 sequence:
            // the bytes are UTF-8.
            let text = match String::from_utf8(merged.bytes) {
                Ok(text) => text,
                Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
            };
            let outcome = MergeOutcome {
                text: text.into(),
                conflicts: merged.conflicts.into(),
            };
            Some((Arc::new(outcome), theirs, format, identity))
        }
        Err(why) => {
            if let Unreadable::Failed(e) = &why {
                log(format_args!("merge of {}: {e}", session.path));
            }
            None
        }
    };
    let mut st = session.lock();
    let outcome = outcome.map(|(outcome, theirs, format, identity)| {
        st.base = theirs;
        st.format = format;
        st.identity = Some(identity);
        outcome
    });
    let read = outcome.is_some();
    session.publish(st, |state| {
        state.merges += 1;
        state.merged = outcome;
        state.dirty_on_disk = !read;
        state.unavailable = !read;
    });
}

/// After an fs_watch event: another identity on disk with other bytes than
/// a save of the base would write (other text, BOM or line endings) makes
/// the file dirty on disk; the same bytes (a touch, a rewrite) only take
/// the new identity, so the next save does not conflict.
fn check(engine: &EngineInner, session: &Session) {
    let identity = session.lock().identity;
    let stat = session
        .safe_root(engine)
        .and_then(|safe| Ok(safe.stat(session.rel()?)));
    if let Ok(Ok(meta)) = &stat
        && Some(meta.identity()) == identity
    {
        return;
    }
    let read = match stat {
        Ok(Err(SafeError::NotFound)) => Err(Unreadable::Gone),
        _ => read_disk(engine, session),
    };
    // The bytes a save of the base would write: a change of the BOM or of
    // the line endings alone is a change too.
    let (base, format) = {
        let st = session.lock();
        (st.base.clone(), st.format)
    };
    let same = matches!(&read, Ok(disk) if disk.bytes == format.encode(&base));
    let mut st = session.lock();
    let (dirty, unavailable) = match read {
        Ok(disk) if same => {
            st.identity = Some(disk.identity);
            (st.state.dirty_on_disk, st.state.unavailable)
        }
        Ok(disk) => (true, disk.decode().is_none()),
        Err(why) => {
            if let Unreadable::Failed(e) = &why {
                log(format_args!("check of {}: {e}", session.path));
            }
            (true, true)
        }
    };
    if (dirty, unavailable) == (st.state.dirty_on_disk, st.state.unavailable) {
        return;
    }
    session.publish(st, |state| {
        state.dirty_on_disk = dirty;
        state.unavailable = unavailable;
    });
}
