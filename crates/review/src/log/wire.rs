//! The annotation log's line format (docs/design/review.md「标注日志」).
//!
//! The encoding is the log's own, independent of how the domain types
//! serialize elsewhere: paths are strings when UTF-8 and byte arrays
//! otherwise, OIDs and SHA-256 digests lowercase hex, `at` Unix
//! milliseconds.
//!
//! ```text
//! {"lectern":"review-log","v":1,"repo":"/abs/common/dir"}
//! {"t":"seen","at":…,"target":{"session":{"source":"…","id":"…"}},"key":{…}}
//! {"t":"unseen","at":…,"target":…,"file":{"old":null,"new":"src/a.rs"}}
//! {"t":"base","at":…,"session":{"source":"…","id":"…"},"commit":"…","entry_at":…}
//! ```
//!
//! Reading borrows from the line where it can and shares the strings that
//! repeat across records (sources, session IDs, worktrees) through an
//! [`Interner`], so the index holds one copy of each.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use lectern_services::project::BlobId;
use lectern_services::session::Timestamp;
use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::{BaseRecord, Record, SessionId};
use crate::seen::{SeenKey, StatusKind, TargetKey};
use crate::types::changes::{
    BaseSpec, ChangePath, FileMode, RawIdentity, SideIdentity, UnavailableKind,
};
use crate::types::review::FileKey;

pub(super) const MAGIC: &str = "review-log";
pub(super) const VERSION: u32 = 1;

/// The first line.
#[derive(Serialize, Deserialize)]
pub(super) struct Header<'a> {
    #[serde(borrow)]
    pub lectern: Cow<'a, str>,
    pub v: u32,
    #[serde(borrow)]
    pub repo: Bytes<'a>,
}

/// Only the record type: tells an unknown record type, kept verbatim, from
/// a bad line.
#[derive(Deserialize)]
struct Tag<'a> {
    #[serde(borrow)]
    t: Cow<'a, str>,
}

/// Raw bytes: a string when UTF-8, an array of numbers otherwise.
pub(super) struct Bytes<'a>(pub Cow<'a, [u8]>);

impl Serialize for Bytes<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match std::str::from_utf8(&self.0) {
            Ok(text) => s.serialize_str(text),
            Err(_) => s.collect_seq(self.0.iter()),
        }
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for Bytes<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Bytes<'a>, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Bytes<'de>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string or an array of bytes")
            }
            fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Bytes<'de>, E> {
                Ok(Bytes(Cow::Borrowed(v.as_bytes())))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Bytes<'de>, E> {
                Ok(Bytes(Cow::Owned(v.as_bytes().to_vec())))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Bytes<'de>, A::Error> {
                let mut bytes = Vec::with_capacity(seq.size_hint().unwrap_or(0));
                while let Some(b) = seq.next_element::<u8>()? {
                    bytes.push(b);
                }
                Ok(Bytes(Cow::Owned(bytes)))
            }
        }
        d.deserialize_any(V)
    }
}

/// 32 bytes as 64 hex digits.
pub(super) struct Hex32(pub [u8; 32]);

impl Serialize for Hex32 {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut text = String::with_capacity(64);
        for b in self.0 {
            text.push_str(&format!("{b:02x}"));
        }
        s.serialize_str(&text)
    }
}

impl<'de> Deserialize<'de> for Hex32 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Hex32, D::Error> {
        let text = Cow::<str>::deserialize(d)?;
        // A SHA-256 OID has the same width; reuse its hex parser.
        match BlobId::from_hex(&text) {
            Some(BlobId::Sha256(bytes)) => Ok(Hex32(bytes)),
            _ => Err(de::Error::invalid_value(
                de::Unexpected::Str(&text),
                &"64 hex digits",
            )),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Session<'a> {
    #[serde(borrow)]
    source: Cow<'a, str>,
    #[serde(borrow)]
    id: Cow<'a, str>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Target<'a> {
    Session(#[serde(borrow)] Session<'a>),
    Outside {
        #[serde(borrow)]
        worktree: Cow<'a, str>,
    },
    Range {
        #[serde(borrow)]
        worktree: Cow<'a, str>,
        #[serde(borrow)]
        base: Spec<'a>,
    },
}

/// A range's `BaseSpec`.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Spec<'a> {
    Head,
    Commit(BlobId),
    MergeBase(#[serde(borrow)] Cow<'a, str>),
}

#[derive(Serialize, Deserialize)]
struct Key<'a> {
    #[serde(borrow)]
    old_path: Option<Bytes<'a>>,
    #[serde(borrow)]
    new_path: Option<Bytes<'a>>,
    old: Option<Side>,
    new: Option<Side>,
    status: Status,
}

#[derive(Serialize, Deserialize)]
struct Side {
    mode: Mode,
    content: Content,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
    File,
    Executable,
    Symlink,
    Gitlink,
    Special,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Content {
    Blob(BlobId),
    Unhashed {
        size: u64,
        mtime_ns: i64,
        inode: u64,
    },
    Filtered(Raw),
    Unavailable(Unavailable),
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Raw {
    Sha256(Hex32),
    Stat {
        size: u64,
        mtime_ns: i64,
        inode: u64,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Unavailable {
    UnsupportedType,
    PathThroughSymlink,
    NestedRepository,
    Missing,
    Unreadable,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Added,
    Deleted,
    Modified,
    TypeChanged,
    Renamed,
    Conflicted,
}

#[derive(Serialize, Deserialize)]
struct File<'a> {
    #[serde(borrow)]
    old: Option<Bytes<'a>>,
    #[serde(borrow)]
    new: Option<Bytes<'a>>,
}

/// A record line, for writing.
#[derive(Serialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Out<'a> {
    Seen {
        at: i64,
        target: Target<'a>,
        key: Key<'a>,
    },
    Unseen {
        at: i64,
        target: Target<'a>,
        file: File<'a>,
    },
    Base {
        at: i64,
        session: Session<'a>,
        /// None: the empty tree (an unborn HEAD).
        commit: Option<BlobId>,
        entry_at: Option<i64>,
    },
}

/// A record line read in one pass: the fields of every known type, all
/// optional. A line of an unknown type with a field of the same name but
/// another shape fails here and is told apart by [`Tag`].
#[derive(Deserialize)]
struct Line<'a> {
    #[serde(borrow)]
    t: Cow<'a, str>,
    at: Option<i64>,
    #[serde(borrow)]
    target: Option<Target<'a>>,
    #[serde(borrow)]
    key: Option<Key<'a>>,
    #[serde(borrow)]
    file: Option<File<'a>>,
    #[serde(borrow)]
    session: Option<Session<'a>>,
    commit: Option<BlobId>,
    entry_at: Option<i64>,
}

/// What a record line holds.
pub(super) enum Decoded {
    Record(Record),
    /// A record type this version does not know: kept verbatim.
    Unknown,
    /// Not a record: skipped and counted.
    Bad,
}

/// One copy of each string that repeats across the records of one read.
#[derive(Default)]
pub(super) struct Interner {
    strs: HashSet<Arc<str>>,
}

impl Interner {
    fn str(&mut self, s: &str) -> Arc<str> {
        if let Some(shared) = self.strs.get(s) {
            return shared.clone();
        }
        let shared: Arc<str> = Arc::from(s);
        self.strs.insert(shared.clone());
        shared
    }
}

/// Appends `record` and a newline to `out`.
pub(super) fn encode(record: &Record, out: &mut Vec<u8>) {
    let line = match record {
        Record::Seen { at, key } => Out::Seen {
            at: at.0,
            target: target(&key.target),
            key: Key {
                old_path: key.old_path.as_ref().map(bytes),
                new_path: key.new_path.as_ref().map(bytes),
                old: key.old.as_ref().map(side),
                new: key.new.as_ref().map(side),
                status: match key.status {
                    StatusKind::Added => Status::Added,
                    StatusKind::Deleted => Status::Deleted,
                    StatusKind::Modified => Status::Modified,
                    StatusKind::TypeChanged => Status::TypeChanged,
                    StatusKind::Renamed => Status::Renamed,
                    StatusKind::Conflicted => Status::Conflicted,
                },
            },
        },
        Record::Unseen {
            at,
            target: t,
            file,
        } => Out::Unseen {
            at: at.0,
            target: target(t),
            file: File {
                old: file.old.as_ref().map(bytes),
                new: file.new.as_ref().map(bytes),
            },
        },
        Record::Base(b) => Out::Base {
            at: b.at.0,
            session: Session {
                source: Cow::Borrowed(&b.session.source),
                id: Cow::Borrowed(&b.session.id),
            },
            commit: b.commit,
            entry_at: b.entry_at,
        },
    };
    // Writing these types into a Vec cannot fail: every map key is a string.
    serde_json::to_writer(&mut *out, &line).expect("a log record serializes");
    out.push(b'\n');
}

/// The header line, with its newline.
pub(super) fn header(repo: &[u8]) -> Vec<u8> {
    let mut out = serde_json::to_vec(&Header {
        lectern: Cow::Borrowed(MAGIC),
        v: VERSION,
        repo: Bytes(Cow::Borrowed(repo)),
    })
    .expect("the header serializes");
    out.push(b'\n');
    out
}

fn bytes(path: &ChangePath) -> Bytes<'_> {
    Bytes(Cow::Borrowed(&path.0))
}

fn target(t: &TargetKey) -> Target<'_> {
    match t {
        TargetKey::Session { source, id } => Target::Session(Session {
            source: Cow::Borrowed(source),
            id: Cow::Borrowed(id),
        }),
        TargetKey::Outside { worktree } => Target::Outside {
            worktree: Cow::Borrowed(worktree),
        },
        TargetKey::Range { worktree, base } => Target::Range {
            worktree: Cow::Borrowed(worktree),
            base: match base {
                BaseSpec::Head => Spec::Head,
                BaseSpec::Commit(oid) => Spec::Commit(*oid),
                BaseSpec::MergeBase(name) => Spec::MergeBase(Cow::Borrowed(name)),
            },
        },
    }
}

fn side((mode, content): &(FileMode, SideIdentity)) -> Side {
    Side {
        mode: match mode {
            FileMode::File => Mode::File,
            FileMode::Executable => Mode::Executable,
            FileMode::Symlink => Mode::Symlink,
            FileMode::Gitlink => Mode::Gitlink,
            FileMode::Special => Mode::Special,
        },
        content: match content {
            // `SeenKey::new` already folds `NotRetained` into `Blob`.
            SideIdentity::Blob(oid) | SideIdentity::NotRetained(oid) => Content::Blob(*oid),
            SideIdentity::Unhashed {
                size,
                mtime_ns,
                inode,
            } => Content::Unhashed {
                size: *size,
                mtime_ns: *mtime_ns,
                inode: *inode,
            },
            SideIdentity::Filtered(RawIdentity::Sha256(d)) => {
                Content::Filtered(Raw::Sha256(Hex32(*d)))
            }
            SideIdentity::Filtered(RawIdentity::Stat {
                size,
                mtime_ns,
                inode,
            }) => Content::Filtered(Raw::Stat {
                size: *size,
                mtime_ns: *mtime_ns,
                inode: *inode,
            }),
            SideIdentity::Unavailable(kind) => Content::Unavailable(match kind {
                UnavailableKind::UnsupportedType => Unavailable::UnsupportedType,
                UnavailableKind::PathThroughSymlink => Unavailable::PathThroughSymlink,
                UnavailableKind::NestedRepository => Unavailable::NestedRepository,
                UnavailableKind::Missing => Unavailable::Missing,
                UnavailableKind::Unreadable => Unavailable::Unreadable,
            }),
        },
    }
}

/// Decodes a record line. Known types are read in one pass; only a line
/// that fails it is parsed again for its type alone.
pub(super) fn decode(line: &[u8], strs: &mut Interner) -> Decoded {
    let l = match serde_json::from_slice::<Line<'_>>(line) {
        Ok(l) => l,
        Err(_) => {
            return match serde_json::from_slice::<Tag<'_>>(line) {
                Ok(tag) if !known(&tag.t) => Decoded::Unknown,
                _ => Decoded::Bad,
            };
        }
    };
    if !known(&l.t) {
        return Decoded::Unknown;
    }
    match known_record(l, strs) {
        Some(record) => Decoded::Record(record),
        None => Decoded::Bad,
    }
}

/// The record of a line of a known type; None when a field it needs is
/// missing.
fn known_record(l: Line<'_>, strs: &mut Interner) -> Option<Record> {
    let at = Timestamp(l.at?);
    match &*l.t {
        "seen" => {
            let k = l.key?;
            let (old_path, new_path) = paths(k.old_path, k.new_path)?;
            let key = SeenKey {
                target: target_key(l.target?, strs),
                old_path,
                new_path,
                old: k.old.map(side_key),
                new: k.new.map(side_key),
                status: match k.status {
                    Status::Added => StatusKind::Added,
                    Status::Deleted => StatusKind::Deleted,
                    Status::Modified => StatusKind::Modified,
                    Status::TypeChanged => StatusKind::TypeChanged,
                    Status::Renamed => StatusKind::Renamed,
                    Status::Conflicted => StatusKind::Conflicted,
                },
            };
            Some(Record::Seen { at, key })
        }
        "unseen" => {
            let f = l.file?;
            let (old, new) = paths(f.old, f.new)?;
            let target = target_key(l.target?, strs);
            let file = FileKey { old, new };
            Some(Record::Unseen { at, target, file })
        }
        "base" => {
            let s = l.session?;
            Some(Record::Base(BaseRecord {
                at,
                session: SessionId {
                    source: strs.str(&s.source),
                    id: strs.str(&s.id),
                },
                commit: l.commit,
                entry_at: l.entry_at,
            }))
        }
        _ => None,
    }
}

/// Whether `t` is a record type this version reads.
fn known(t: &str) -> bool {
    matches!(t, "seen" | "unseen" | "base")
}

type Paths = (Option<ChangePath>, Option<ChangePath>);

/// The old and new paths, sharing one copy when they are the same; None
/// when both are missing.
fn paths(old: Option<Bytes<'_>>, new: Option<Bytes<'_>>) -> Option<Paths> {
    let new = new.map(|b| ChangePath(Arc::from(&*b.0)));
    let old = match (old, &new) {
        (Some(o), Some(n)) if *o.0 == *n.0 => Some(n.clone()),
        (old, _) => old.map(|b| ChangePath(Arc::from(&*b.0))),
    };
    (old.is_some() || new.is_some()).then_some((old, new))
}

fn target_key(t: Target<'_>, strs: &mut Interner) -> TargetKey {
    match t {
        Target::Session(s) => TargetKey::Session {
            source: strs.str(&s.source),
            id: strs.str(&s.id),
        },
        Target::Outside { worktree } => TargetKey::Outside {
            worktree: strs.str(&worktree),
        },
        Target::Range { worktree, base } => TargetKey::Range {
            worktree: strs.str(&worktree),
            base: match base {
                Spec::Head => BaseSpec::Head,
                Spec::Commit(oid) => BaseSpec::Commit(oid),
                Spec::MergeBase(name) => BaseSpec::MergeBase(strs.str(&name)),
            },
        },
    }
}

fn side_key(s: Side) -> (FileMode, SideIdentity) {
    let mode = match s.mode {
        Mode::File => FileMode::File,
        Mode::Executable => FileMode::Executable,
        Mode::Symlink => FileMode::Symlink,
        Mode::Gitlink => FileMode::Gitlink,
        Mode::Special => FileMode::Special,
    };
    let content = match s.content {
        Content::Blob(oid) => SideIdentity::Blob(oid),
        Content::Unhashed {
            size,
            mtime_ns,
            inode,
        } => SideIdentity::Unhashed {
            size,
            mtime_ns,
            inode,
        },
        Content::Filtered(Raw::Sha256(d)) => SideIdentity::Filtered(RawIdentity::Sha256(d.0)),
        Content::Filtered(Raw::Stat {
            size,
            mtime_ns,
            inode,
        }) => SideIdentity::Filtered(RawIdentity::Stat {
            size,
            mtime_ns,
            inode,
        }),
        Content::Unavailable(kind) => SideIdentity::Unavailable(match kind {
            Unavailable::UnsupportedType => UnavailableKind::UnsupportedType,
            Unavailable::PathThroughSymlink => UnavailableKind::PathThroughSymlink,
            Unavailable::NestedRepository => UnavailableKind::NestedRepository,
            Unavailable::Missing => UnavailableKind::Missing,
            Unavailable::Unreadable => UnavailableKind::Unreadable,
        }),
    };
    (mode, content)
}
