//! What a snapshot's rows are read from: the first screen's pieces, the
//! full text, or tier C's pages (with the first screen's pieces under them
//! and, after the scan, the line index).

use std::borrow::Cow;
use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Arc;

use crate::highlight::Source;
use crate::limits::{PAGE, PAGE_LRU};
use crate::text::{LineIndex, Pages, Text};

/// A run of the file's bytes: `len` bytes from `start`, a view into a
/// buffer (a read piece, or object content someone else holds).
#[derive(Clone)]
pub(crate) struct Piece {
    pub start: u64,
    pub bytes: Arc<[u8]>,
    pub offset: usize,
    pub len: usize,
}

impl Piece {
    pub(crate) fn owned(start: u64, bytes: Vec<u8>) -> Piece {
        let len = bytes.len();
        Piece {
            start,
            bytes: bytes.into(),
            offset: 0,
            len,
        }
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        &self.bytes[self.offset..self.offset + self.len]
    }

    pub(crate) fn range(&self) -> Range<u64> {
        self.start..self.start + self.len as u64
    }
}

/// The first screen's bytes: the file's head and the read around the
/// target (or all of it), by start.
#[derive(Clone, Default)]
pub(crate) struct Pieces {
    pub size: u64,
    pub pieces: Vec<Piece>,
    /// A byte whose line is known: `(byte, line, exact)`.
    pub base: (u64, u64, bool),
    /// Lines per byte, for estimates.
    pub density: f64,
}

impl Source for Pieces {
    fn len(&self) -> u64 {
        self.size
    }

    fn chunk(&self, at: u64) -> Option<&[u8]> {
        self.pieces
            .iter()
            .find(|p| p.range().contains(&at))
            .map(|p| &p.as_slice()[(at - p.start) as usize..])
    }
}

/// Tier C's content: the pages in memory over the first screen's pieces.
pub(crate) struct Paged {
    pub pages: Arc<Pages>,
    pub pieces: Arc<Pieces>,
    pub index: Option<Arc<LineIndex>>,
}

impl Source for Paged {
    fn len(&self) -> u64 {
        self.pages.len()
    }

    fn chunk(&self, at: u64) -> Option<&[u8]> {
        self.pages.chunk(at).or_else(|| self.pieces.chunk(at))
    }
}

#[derive(Clone, Default)]
pub(crate) enum Content {
    #[default]
    Nothing,
    Pieces(Arc<Pieces>),
    Text(Arc<Text>),
    Paged(Arc<Paged>),
}

impl Content {
    pub(crate) fn source(&self) -> Option<&dyn Source> {
        match self {
            Content::Nothing => None,
            Content::Pieces(p) => Some(&**p),
            Content::Text(t) => Some(&**t),
            Content::Paged(p) => Some(&**p),
        }
    }

    pub(crate) fn index(&self) -> Option<&LineIndex> {
        match self {
            Content::Text(t) => Some(t.index()),
            Content::Paged(p) => p.index.as_deref(),
            _ => None,
        }
    }

    pub(crate) fn pieces(&self) -> Option<&Pieces> {
        match self {
            Content::Pieces(p) => Some(p),
            Content::Paged(p) => Some(&p.pieces),
            _ => None,
        }
    }
}

/// The bytes of `range`, borrowed when one chunk holds them all; None when
/// some are not in memory.
pub(crate) fn slice(src: &dyn Source, range: Range<u64>) -> Option<Cow<'_, [u8]>> {
    let len = (range.end - range.start) as usize;
    if len == 0 {
        return Some(Cow::Borrowed(&[]));
    }
    let first = src.chunk(range.start)?;
    if first.len() >= len {
        return Some(Cow::Borrowed(&first[..len]));
    }
    let mut out = Vec::with_capacity(len);
    out.extend_from_slice(first);
    while out.len() < len {
        let chunk = src
            .chunk(range.start + out.len() as u64)
            .filter(|c| !c.is_empty())?;
        let take = chunk.len().min(len - out.len());
        out.extend_from_slice(&chunk[..take]);
    }
    Some(Cow::Owned(out))
}

/// Tier C's pages by recency: at most [`PAGE_LRU`] bytes.
#[derive(Default)]
pub(crate) struct PageCache {
    pages: Arc<Pages>,
    /// Least recently used first.
    order: VecDeque<u64>,
}

impl PageCache {
    pub(crate) fn new(len: u64) -> PageCache {
        PageCache {
            pages: Arc::new(Pages::new(len)),
            order: VecDeque::new(),
        }
    }

    pub(crate) fn pages(&self) -> &Arc<Pages> {
        &self.pages
    }

    /// The pages of `range` that are not in memory.
    pub(crate) fn missing(&self, range: Range<u64>) -> Vec<u64> {
        let last = range.end.min(self.pages.len()).div_ceil(PAGE);
        (range.start / PAGE..last)
            .filter(|p| !self.pages.has(*p))
            .collect()
    }

    /// Marks the pages of `range` used.
    pub(crate) fn touch(&mut self, range: Range<u64>) {
        let last = range.end.min(self.pages.len()).div_ceil(PAGE);
        for p in range.start / PAGE..last {
            if let Some(i) = self.order.iter().position(|&q| q == p) {
                self.order.remove(i);
                self.order.push_back(p);
            }
        }
    }

    /// Adds pages read, then drops the least recently used beyond the
    /// limit.
    pub(crate) fn add(&mut self, read: Vec<(u64, Arc<[u8]>)>) {
        let mut pages = (*self.pages).clone();
        for (page, bytes) in read {
            if !pages.has(page) {
                self.order.push_back(page);
            }
            pages.insert(page, bytes);
        }
        while pages.bytes() as u64 > PAGE_LRU {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            pages.without(old);
        }
        self.pages = Arc::new(pages);
    }
}
