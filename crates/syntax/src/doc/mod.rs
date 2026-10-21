//! Open documents (docs/design/file_reader.md「打开的文档」「数据流与线程」):
//! [`Engine`] opens a [`ReadTarget`], its tasks read and highlight the
//! content on executor threads, and the view reads immutable
//! [`DocSnapshot`]s through a [`DocHandle`]. The UI thread only queues
//! commands and copies an `Arc`.

pub(crate) mod content;
mod engine;
mod load;
mod rows;
mod watch;

use std::borrow::Cow;
use std::ops::Range;
use std::sync::Arc;

use lectern_services::project::BlobId;
use lectern_services::reader::ReadTarget;

use crate::find::FindResults;
use crate::spans::{Token, WindowSpans};

pub(crate) use content::{Content, Pieces};
pub use engine::{DocHandle, Engine, EngineDeps};

/// What the viewport holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Viewport {
    pub rows: u32,
    pub cols: u32,
}

/// The immutable state the view reads. Its methods do work in proportion to
/// the rows asked for, never to the file's size.
#[derive(Clone)]
pub struct DocSnapshot {
    /// Bumped when `anchor` changes (opened, located again): the view
    /// scrolls to the new anchor.
    pub generation: u64,
    pub target: ReadTarget,
    pub state: DocState,
    pub size: u64,
    /// The OID of the content read (the raw bytes as a git blob); None
    /// until the full read or the streaming scan (an object's is `expect`).
    pub oid: Option<BlobId>,
    pub tier: Option<Tier>,
    pub highlight: Highlighting,
    pub lines: LineCount,
    pub widest_cells: u32,
    pub editable: Editable,
    /// Where the view scrolls to.
    pub anchor: RowPos,
    pub find: Option<Arc<FindResults>>,
    pub(crate) content: Content,
    /// The spans to colour rows with, the preferred first: the whole file's,
    /// then windows.
    pub(crate) spans: Arc<[Arc<WindowSpans>]>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DocState {
    /// The first screen is not read yet: the view shows a skeleton by the
    /// 100 ms rule.
    Opening,
    /// There is content to draw (the first screen, or all of it).
    Ready,
    /// The last component is a symbolic link: its target is shown, not
    /// followed.
    Symlink {
        target: Arc<[u8]>,
    },
    /// A directory, FIFO or other type whose content is not read.
    Unsupported,
    Missing,
    Unreadable(ReadError),
}

/// Why a target cannot be read.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ReadError {
    /// A directory on the path is a symbolic link.
    PathThroughSymlink,
    /// The path is not a valid relative path.
    InvalidPath,
    /// A dependency file outside its root, with an extension not on ADR
    /// 0033's list, or in a cache root outside the worktree.
    NotAllowed,
    /// The object database has no such blob, or the project has none.
    ObjectMissing,
    /// The blob is over 16 MiB ([`OBJECT_MAX`](crate::limits::OBJECT_MAX))
    /// and nobody holds it.
    ObjectTooLarge,
    /// Permissions and other IO errors.
    Io(Arc<str>),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Tier {
    A,
    B,
    C,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Highlighting {
    Pending,
    On,
    Plain,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum LineCount {
    Estimated(u64),
    Exact(u64),
}

impl LineCount {
    pub fn get(self) -> u64 {
        match self {
            LineCount::Estimated(n) | LineCount::Exact(n) => n,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Editable {
    Yes,
    No(NotEditable),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum NotEditable {
    NotLoaded,
    TooLarge,
    LongLine,
    NotText,
    MixedLineEndings,
    ReadOnlySource,
    Reloading,
}

/// A display row: the start of its line and which display row of the line
/// (only lines over 4 KiB have more than one). The scroll position is this
/// plus a pixel offset. Before the full read a long line's rows are folded
/// from the first screen's start, which then stands for the line start.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct RowPos {
    pub line_start: u64,
    pub sub_row: u32,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Row<'a> {
    Text {
        pos: RowPos,
        /// From 0; `Estimated` before the line index exists.
        line: LineNo,
        /// The bytes of the file this row shows.
        bytes: Range<u64>,
        /// Invalid UTF-8 shows as U+FFFD, control characters as their
        /// U+2400 symbols; a line's final `\r` and a BOM are not shown.
        text: Cow<'a, str>,
        /// Relative to `text`; empty without highlighting.
        tokens: Cow<'a, [Token]>,
        /// The find matches meeting this row, as byte ranges of the file.
        matches: &'a [Range<u64>],
    },
    /// The content is not in memory (a page of tier C not read yet):
    /// `scrolled` gets it read.
    Pending { pos: RowPos },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum LineNo {
    Exact(u64),
    Estimated(u64),
}
