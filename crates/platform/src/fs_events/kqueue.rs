//! `Scope::Entries`: one kqueue for every target, served by a dedicated
//! thread (docs/design/fs_watch.md「platform 后端」).
//!
//! The directory is watched for entries being added, removed or renamed
//! (`NOTE_WRITE`) and for itself going away; each named entry that exists is
//! opened with `O_EVTONLY | O_SYMLINK` (a symbolic link is watched itself,
//! not followed) and watched for content and metadata changes. When the
//! directory changes, every named entry is `fstatat`-ed: one that appeared,
//! vanished or was replaced (atomic rename over it) is reopened or closed
//! and reported.
//!
//! All state is behind one lock. The thread delivers while holding it and an
//! `EntriesWatch` is removed under it, so once its drop returns the thread
//! never calls its `Deliver` again.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsString;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::ptr;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use lectern_fs_watch::{Armed, BackendError, Flags, RawBatch, RawEvent};
use rustix::event::kqueue::{
    Event, EventFilter, EventFlags, UserDefinedFlags, UserFlags, VnodeEvents, kevent, kqueue,
};
use rustix::fs::{AtFlags, CWD, FileType, Mode, OFlags, Stat};
use rustix::io::Errno;

use super::{Deliver, errno_error, lock};

/// Descriptors all `Entries` targets may hold together, the directories
/// included; an `arm` that would exceed it returns `Exhausted`.
pub(crate) const MAX_DESCRIPTORS: usize = 256;

/// How long `flush` waits for the thread.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

/// `O_EVTONLY` (`sys/fcntl.h`): open for event notification only, which does
/// not keep the volume from being unmounted. rustix has no name for it.
const O_EVTONLY: OFlags = OFlags::from_bits_retain(0x0000_8000);

/// The ident of the `EVFILT_USER` event that wakes the thread (flush, stop).
const WAKE: isize = 1;

const DIR_NOTES: VnodeEvents = VnodeEvents::WRITE
    .union(VnodeEvents::DELETE)
    .union(VnodeEvents::RENAME)
    .union(VnodeEvents::REVOKE);
const ENTRY_NOTES: VnodeEvents = VnodeEvents::WRITE
    .union(VnodeEvents::EXTEND)
    .union(VnodeEvents::ATTRIBUTES)
    .union(VnodeEvents::DELETE)
    .union(VnodeEvents::RENAME)
    .union(VnodeEvents::REVOKE)
    .union(VnodeEvents::LINK);
/// The vnode is gone from where it was.
const GONE: VnodeEvents = VnodeEvents::DELETE
    .union(VnodeEvents::RENAME)
    .union(VnodeEvents::REVOKE);

pub(crate) struct Kqueue {
    kq: OwnedFd,
    state: Mutex<State>,
    /// Signalled when `State::done` moves or the thread stops.
    flushed: Condvar,
}

#[derive(Default)]
struct State {
    watches: HashMap<u64, Watch>,
    index: Index,
    next_watch: u64,
    /// Descriptors reserved by the watches (`Watch::reserved`).
    descriptors: usize,
    /// Flush generations: requested by `flush`, done by the thread.
    requested: u64,
    done: u64,
    /// `stop` was called or the thread failed; nothing more is delivered.
    stopped: bool,
}

/// Which descriptor a kevent's `udata` token stands for. Tokens are never
/// reused, so an event for a descriptor closed meanwhile finds nothing.
#[derive(Default)]
struct Index {
    tokens: HashMap<usize, (u64, Slot)>,
    next_token: usize,
}

#[derive(Clone, Copy)]
enum Slot {
    Dir,
    Entry(usize),
}

struct Watch {
    dir: OwnedFd,
    entries: Vec<Entry>,
    /// The directory plus one per name, whether or not it exists.
    reserved: usize,
    /// No names: entries being added and removed are reported as the
    /// directory itself (an empty path); kqueue does not say which entry.
    report_dir: bool,
    deliver: Deliver,
}

struct Entry {
    name: OsString,
    open: Option<Open>,
}

struct Open {
    /// Closing it removes its kevent registration.
    _fd: OwnedFd,
    token: usize,
    dev: rustix::fs::Dev,
    ino: u64,
    /// FILE, DIR or SYMLINK.
    kind: Flags,
}

/// What one thread wakeup collected, per watch.
#[derive(Default)]
struct Changes {
    events: BTreeMap<Box<[u8]>, Flags>,
    /// The directory changed: re-check every named entry.
    resync_all: bool,
    /// Named entries whose own vnode went away (deleted, renamed, revoked):
    /// reopened even when `fstatat` finds the same device and inode, which
    /// a new file could have been given.
    gone: BTreeSet<usize>,
}

impl Changes {
    fn add(&mut self, path: &[u8], flags: Flags) {
        *self.events.entry(path.into()).or_default() |= flags;
    }
}

enum Note {
    Wake,
    Vnode { token: usize, notes: VnodeEvents },
}

impl Kqueue {
    pub(crate) fn new() -> Result<Kqueue, BackendError> {
        let kq = kqueue().map_err(errno_error)?;
        let wake = Event::new(
            EventFilter::User {
                ident: WAKE,
                flags: UserFlags::empty(),
                user_flags: UserDefinedFlags::new(0),
            },
            EventFlags::ADD | EventFlags::CLEAR,
            ptr::null_mut(),
        );
        change(&kq, wake).map_err(errno_error)?;
        Ok(Kqueue {
            kq,
            state: Mutex::new(State {
                next_watch: 1,
                index: Index {
                    tokens: HashMap::new(),
                    next_token: 1,
                },
                ..State::default()
            }),
            flushed: Condvar::new(),
        })
    }

    pub(crate) fn is_running(&self) -> bool {
        !lock(&self.state).stopped
    }

    /// Makes the thread exit; flushes in progress fail.
    pub(crate) fn stop(&self) {
        let mut state = lock(&self.state);
        state.stopped = true;
        // The thread may already be gone; then there is nobody to wake.
        let _ = self.wake();
        drop(state);
        self.flushed.notify_all();
    }

    pub(crate) fn arm(
        self: &Arc<Self>,
        path: &Path,
        names: &[OsString],
        deliver: Deliver,
    ) -> Result<EntriesWatch, BackendError> {
        let mut guard = lock(&self.state);
        let state = &mut *guard;
        if state.stopped {
            return Err(stopped());
        }
        let reserved = 1 + names.len();
        if state.descriptors + reserved > MAX_DESCRIPTORS {
            return Err(BackendError::Exhausted);
        }
        let dir = rustix::fs::openat(
            CWD,
            path,
            OFlags::RDONLY | O_EVTONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(errno_error)?;
        let id = state.next_watch;
        state.next_watch += 1;
        let dir_token = state.index.token();
        register(&self.kq, dir.as_raw_fd(), DIR_NOTES, dir_token).map_err(errno_error)?;
        state.index.tokens.insert(dir_token, (id, Slot::Dir));
        let mut watch = Watch {
            dir,
            entries: names
                .iter()
                .map(|name| Entry {
                    name: name.clone(),
                    open: None,
                })
                .collect(),
            reserved,
            report_dir: names.is_empty(),
            deliver,
        };
        for slot in 0..watch.entries.len() {
            // An entry that does not exist (or cannot be opened) is picked
            // up by the next directory change.
            watch.open(&self.kq, &mut state.index, id, slot);
        }
        state.descriptors += reserved;
        state.watches.insert(id, watch);
        Ok(EntriesWatch {
            kqueue: self.clone(),
            id,
        })
    }

    /// Delivers what already happened: wakes the thread and waits until it
    /// has drained the kqueue past this call.
    fn flush(&self) -> Result<(), BackendError> {
        let mut state = lock(&self.state);
        if state.stopped {
            return Err(stopped());
        }
        state.requested += 1;
        let generation = state.requested;
        self.wake().map_err(errno_error)?;
        let deadline = Instant::now() + FLUSH_TIMEOUT;
        while state.done < generation {
            if state.stopped {
                return Err(stopped());
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(BackendError::Other("the kqueue flush timed out".to_owned()));
            }
            state = match self.flushed.wait_timeout(state, deadline - now) {
                Ok((state, _)) => state,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
        Ok(())
    }

    fn wake(&self) -> Result<(), Errno> {
        let trigger = Event::new(
            EventFilter::User {
                ident: WAKE,
                flags: UserFlags::TRIGGER,
                user_flags: UserDefinedFlags::new(0),
            },
            EventFlags::empty(),
            ptr::null_mut(),
        );
        change(&self.kq, trigger)
    }

    /// The thread body; returns after `stop` or on a kevent error.
    pub(crate) fn run(self: Arc<Self>) {
        let mut buffer = [MaybeUninit::<Event>::uninit(); 64];
        loop {
            let notes = match self.poll(&mut buffer, None) {
                Ok(notes) => notes,
                Err(errno) => return self.fail(errno),
            };
            let mut state = lock(&self.state);
            let mut round = BTreeMap::new();
            let mut flush = None;
            for note in notes {
                if self.note(&mut state, note, &mut round) {
                    // Read after the wakeup arrived: every flush up to this
                    // generation started before now, so the events it must
                    // see are already in the kqueue and the drain below
                    // collects them.
                    flush = Some(state.requested);
                }
            }
            if state.stopped {
                drop(state);
                self.flushed.notify_all();
                return;
            }
            if flush.is_some() {
                loop {
                    let notes = match self.poll(&mut buffer, Some(Duration::ZERO)) {
                        Ok(notes) => notes,
                        Err(errno) => {
                            drop(state);
                            return self.fail(errno);
                        }
                    };
                    let more = notes.len() == buffer.len();
                    for note in notes {
                        self.note(&mut state, note, &mut round);
                    }
                    if !more {
                        break;
                    }
                }
            }
            self.finish(&mut state, round);
            if let Some(generation) = flush {
                state.done = state.done.max(generation);
                drop(state);
                self.flushed.notify_all();
            }
        }
    }

    fn poll(
        &self,
        buffer: &mut [MaybeUninit<Event>; 64],
        timeout: Option<Duration>,
    ) -> Result<Vec<Note>, Errno> {
        loop {
            // SAFETY: rustix asks that the descriptors registered with the
            // kqueue stay valid while it is in use. Every registered
            // descriptor is owned by a `Watch` and closed only under the
            // state lock; closing it removes its registration (kqueue(2)), so
            // the kqueue never refers to a closed descriptor.
            match unsafe { kevent(&self.kq, &[], &mut *buffer, timeout) } {
                Ok((events, _)) => {
                    return Ok(events
                        .iter()
                        .filter_map(|event| match event.filter() {
                            EventFilter::User { .. } => Some(Note::Wake),
                            EventFilter::Vnode { flags, .. } => Some(Note::Vnode {
                                token: event.udata().addr(),
                                notes: flags,
                            }),
                            _ => None,
                        })
                        .collect());
                }
                Err(Errno::INTR) => continue,
                Err(errno) => return Err(errno),
            }
        }
    }

    /// Records one kevent. Returns true for a wakeup.
    fn note(&self, state: &mut State, note: Note, round: &mut BTreeMap<u64, Changes>) -> bool {
        let (token, notes) = match note {
            Note::Wake => return true,
            Note::Vnode { token, notes } => (token, notes),
        };
        let Some(&(id, slot)) = state.index.tokens.get(&token) else {
            return false;
        };
        let Some(watch) = state.watches.get(&id) else {
            return false;
        };
        let changes = round.entry(id).or_default();
        match slot {
            Slot::Dir => {
                if notes.intersects(GONE) {
                    changes.add(b"", Flags::ROOT_CHANGED);
                }
                if notes.contains(VnodeEvents::WRITE) {
                    changes.resync_all = true;
                    if watch.report_dir {
                        changes.add(b"", Flags::DIR | Flags::MODIFIED);
                    }
                }
            }
            Slot::Entry(slot) => {
                let entry = &watch.entries[slot];
                let kind = entry
                    .open
                    .as_ref()
                    .map_or(Flags::default(), |open| open.kind);
                changes.add(entry.name.as_bytes(), kind | entry_flags(notes));
                if notes.intersects(GONE) {
                    changes.gone.insert(slot);
                }
            }
        }
        false
    }

    /// Re-checks the named entries the round touched, then delivers one
    /// batch per watch.
    fn finish(&self, state: &mut State, round: BTreeMap<u64, Changes>) {
        let State { watches, index, .. } = state;
        for (id, mut changes) in round {
            let Some(watch) = watches.get_mut(&id) else {
                continue;
            };
            let gone = std::mem::take(&mut changes.gone);
            let slots: Vec<usize> = if changes.resync_all {
                (0..watch.entries.len()).collect()
            } else {
                gone.iter().copied().collect()
            };
            for slot in slots {
                let force = gone.contains(&slot);
                if let Some(flags) = watch.resync(&self.kq, index, id, slot, force) {
                    changes.add(watch.entries[slot].name.as_bytes(), flags);
                }
            }
            if changes.events.is_empty() {
                continue;
            }
            let batch = RawBatch {
                events: changes
                    .events
                    .into_iter()
                    .map(|(path, flags)| RawEvent { path, flags })
                    .collect(),
            };
            // A panicking `deliver` loses this batch only; the thread goes
            // on.
            let _ = catch_unwind(AssertUnwindSafe(|| (watch.deliver)(batch)));
        }
    }

    fn fail(&self, errno: Errno) {
        eprintln!(
            "lectern-platform: the kqueue thread stopped: {}",
            io::Error::from(errno)
        );
        lock(&self.state).stopped = true;
        self.flushed.notify_all();
    }
}

impl Index {
    fn token(&mut self) -> usize {
        let token = self.next_token;
        self.next_token += 1;
        token
    }
}

impl Watch {
    /// Opens and registers named entry `slot` if it exists. Returns its kind
    /// when it was opened.
    fn open(&mut self, kq: &OwnedFd, index: &mut Index, id: u64, slot: usize) -> Option<Flags> {
        let entry = &mut self.entries[slot];
        let fd = rustix::fs::openat(
            &self.dir,
            &entry.name,
            OFlags::RDONLY | O_EVTONLY | OFlags::SYMLINK | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .ok()?;
        let stat = rustix::fs::fstat(&fd).ok()?;
        let token = index.token();
        register(kq, fd.as_raw_fd(), ENTRY_NOTES, token).ok()?;
        index.tokens.insert(token, (id, Slot::Entry(slot)));
        let kind = kind_of(&stat);
        entry.open = Some(Open {
            _fd: fd,
            token,
            dev: stat.st_dev,
            ino: stat.st_ino,
            kind,
        });
        Some(kind)
    }

    /// Compares named entry `slot` with what is open; reopens or closes it
    /// when it appeared, was replaced or vanished (or, with `force`, its open
    /// vnode is known to be gone). Returns the flags to report, None when
    /// nothing changed.
    fn resync(
        &mut self,
        kq: &OwnedFd,
        index: &mut Index,
        id: u64,
        slot: usize,
        force: bool,
    ) -> Option<Flags> {
        let entry = &mut self.entries[slot];
        match rustix::fs::statat(&self.dir, &entry.name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => {
                if !force
                    && entry
                        .open
                        .as_ref()
                        .is_some_and(|open| open.dev == stat.st_dev && open.ino == stat.st_ino)
                {
                    return None;
                }
                let replaced = entry
                    .open
                    .take()
                    .map(|open| index.tokens.remove(&open.token));
                let kind = self
                    .open(kq, index, id, slot)
                    .unwrap_or_else(|| kind_of(&stat));
                let mut flags = kind | Flags::CREATED;
                if replaced.is_some() {
                    flags |= Flags::RENAMED;
                }
                Some(flags)
            }
            // Gone (ENOENT), or the directory itself is (ENOTDIR and the
            // like): close what was open.
            Err(_) => {
                let open = entry.open.take()?;
                index.tokens.remove(&open.token);
                Some(open.kind | Flags::REMOVED)
            }
        }
    }
}

/// A started `Entries` watch. Dropping it closes its descriptors.
pub(crate) struct EntriesWatch {
    kqueue: Arc<Kqueue>,
    id: u64,
}

impl Armed for EntriesWatch {
    fn flush(&self) -> Result<(), BackendError> {
        self.kqueue.flush()
    }
}

impl Drop for EntriesWatch {
    fn drop(&mut self) {
        let mut state = lock(&self.kqueue.state);
        if let Some(watch) = state.watches.remove(&self.id) {
            state.descriptors -= watch.reserved;
            let id = self.id;
            state.index.tokens.retain(|_, (owner, _)| *owner != id);
            // Closed (and unregistered) under the lock, so the thread
            // cannot be delivering for this watch.
            drop(watch);
        }
    }
}

fn register(kq: &OwnedFd, fd: RawFd, notes: VnodeEvents, token: usize) -> Result<(), Errno> {
    let event = Event::new(
        EventFilter::Vnode {
            vnode: fd,
            flags: notes,
        },
        EventFlags::ADD | EventFlags::CLEAR,
        ptr::without_provenance_mut(token),
    );
    change(kq, event)
}

/// Applies one change to the kqueue without waiting for events.
fn change(kq: &OwnedFd, event: Event) -> Result<(), Errno> {
    let mut none: [MaybeUninit<Event>; 0] = [];
    loop {
        // SAFETY: see `Kqueue::poll`; a registered descriptor is owned by a
        // `Watch` and stays open until it is unregistered by closing it.
        // With no room for events, kevent only applies the change and
        // returns at once.
        match unsafe { kevent(kq, &[event], &mut none, None) } {
            Ok(_) => return Ok(()),
            Err(Errno::INTR) => continue,
            Err(errno) => return Err(errno),
        }
    }
}

fn entry_flags(notes: VnodeEvents) -> Flags {
    let mut flags = Flags::default();
    if notes.intersects(VnodeEvents::WRITE | VnodeEvents::EXTEND) {
        flags |= Flags::MODIFIED;
    }
    if notes.intersects(VnodeEvents::ATTRIBUTES | VnodeEvents::LINK) {
        flags |= Flags::META;
    }
    if notes.intersects(VnodeEvents::DELETE | VnodeEvents::REVOKE) {
        flags |= Flags::REMOVED;
    }
    if notes.contains(VnodeEvents::RENAME) {
        flags |= Flags::RENAMED;
    }
    flags
}

fn kind_of(stat: &Stat) -> Flags {
    match FileType::from_raw_mode(stat.st_mode) {
        FileType::RegularFile => Flags::FILE,
        FileType::Directory => Flags::DIR,
        FileType::Symlink => Flags::SYMLINK,
        _ => Flags::default(),
    }
}

fn stopped() -> BackendError {
    BackendError::Other("the kqueue thread stopped".to_owned())
}
