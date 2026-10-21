//! Find in a document (docs/design/file_reader.md「`syntax` 的公开项」
//! `FindQuery`, 「已知限制」): ASCII letters match either case, every other
//! byte matches only itself; matches do not overlap; at most
//! [`FIND_MAX`] of them. The text is searched in blocks of 1 MiB, and the
//! caller checks for cancellation between blocks.

use std::ops::Range;
use std::sync::Arc;

use memchr::memmem;

use crate::limits::FIND_MAX;

/// What to find.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct FindQuery {
    pub text: Arc<str>,
}

/// The matches found so far, as byte ranges of the file, ascending.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct FindResults {
    pub ranges: Arc<[Range<u64>]>,
    /// The whole text was searched, or the cap was reached.
    pub done: bool,
    /// [`FIND_MAX`] matches were found; there may be more.
    pub capped: bool,
}

/// A block of the search.
pub(crate) const BLOCK: usize = 1 << 20;

/// The longest query searched; a longer one finds nothing.
pub(crate) const QUERY_MAX: usize = 4 << 10;

/// One search in progress: the blocks fed in order, each overlapping the
/// previous one by the query's length less one byte.
pub(crate) struct Finder {
    needle: Vec<u8>,
    fold: bool,
    found: Vec<Range<u64>>,
    /// No match may start before this (the last match's end).
    from: u64,
    scratch: Vec<u8>,
}

impl Finder {
    /// None for an empty query or one over [`QUERY_MAX`].
    pub(crate) fn new(query: &str) -> Option<Finder> {
        let bytes = query.as_bytes();
        if bytes.is_empty() || bytes.len() > QUERY_MAX {
            return None;
        }
        let fold = bytes.iter().any(u8::is_ascii_alphabetic);
        Some(Finder {
            needle: bytes.to_ascii_lowercase(),
            fold,
            found: Vec::new(),
            from: 0,
            scratch: Vec::new(),
        })
    }

    /// How far a block reaches past the next block's start.
    pub(crate) fn overlap(&self) -> usize {
        self.needle.len() - 1
    }

    /// The matches in `block`, the text from `base`, that start before
    /// `limit` (the next block's start). True once the cap is reached.
    pub(crate) fn feed(&mut self, block: &[u8], base: u64, limit: u64) -> bool {
        let hay: &[u8] = if self.fold {
            self.scratch.clear();
            self.scratch
                .extend(block.iter().map(u8::to_ascii_lowercase));
            &self.scratch
        } else {
            block
        };
        let len = self.needle.len() as u64;
        for at in memmem::find_iter(hay, &self.needle) {
            let start = base + at as u64;
            if start >= limit {
                break;
            }
            if start < self.from {
                continue;
            }
            self.found.push(start..start + len);
            self.from = start + len;
            if self.found.len() >= FIND_MAX {
                return true;
            }
        }
        false
    }

    pub(crate) fn results(&self, done: bool) -> FindResults {
        FindResults {
            ranges: self.found.clone().into(),
            done: done || self.found.len() >= FIND_MAX,
            capped: self.found.len() >= FIND_MAX,
        }
    }
}

/// Searches contiguous text, calling `stop` between blocks (true: give up).
/// None when stopped.
pub(crate) fn find_in(
    text: &[u8],
    query: &str,
    mut stop: impl FnMut(&FindResults) -> bool,
) -> Option<FindResults> {
    let Some(mut finder) = Finder::new(query) else {
        return Some(FindResults {
            done: true,
            ..FindResults::default()
        });
    };
    let mut at = 0;
    while at < text.len() {
        let limit = (at + BLOCK).min(text.len());
        let end = (limit + finder.overlap()).min(text.len());
        if finder.feed(&text[at..end], at as u64, limit as u64) {
            break;
        }
        at = limit;
        if at < text.len() && stop(&finder.results(false)) {
            return None;
        }
    }
    Some(finder.results(true))
}
