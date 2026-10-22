//! Changes to an edit session's buffer and the buffer's format on disk
//! (docs/design/file_reader.md「编辑」): what [`TextEdit`]s say, how several
//! of them become one, and how the file's bytes become the buffer's text and
//! back (the BOM and the line endings). Pure; any thread.

use std::sync::Arc;

/// One change to a buffer, in byte offsets: `[start, old_end)` of the text
/// before it became `[start, new_end)` of the text after it. Several edits
/// passed together apply in order, each to the text the ones before it
/// left. The same three numbers as tree-sitter's `InputEdit`, without its
/// points, so the view needs no tree-sitter.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TextEdit {
    pub start: usize,
    pub old_end: usize,
    pub new_end: usize,
}

impl TextEdit {
    /// The length of the text after this edit, from the length before it;
    /// None when the edit does not fit that text.
    fn apply_len(self, len: usize) -> Option<usize> {
        if self.start > self.old_end || self.old_end > len || self.new_end < self.start {
            return None;
        }
        (len - (self.old_end - self.start)).checked_add(self.new_end - self.start)
    }
}

/// One edit doing what `first` then `then` do: `first` takes text X to Y,
/// `then` takes Y to Z; the result takes X to Z, covering both changes and
/// the unchanged bytes between them.
pub(crate) fn compose(first: TextEdit, then: TextEdit) -> TextEdit {
    // In Y, the changed region ends at the later of the two ends; past
    // `first`'s new end, Y and X differ by `first`'s shift, and past `then`'s
    // old end, Z and Y differ by `then`'s.
    let y_end = first.new_end.max(then.old_end);
    TextEdit {
        start: first.start.min(then.start),
        old_end: first.old_end + (y_end - first.new_end),
        new_end: y_end - then.old_end + then.new_end,
    }
}

/// `edits` (from a text of `old_len` bytes, in order) as one edit to a text
/// of `new_len` bytes. None when the list is empty or does not fit the
/// lengths: the caller then compares the texts ([`diff`]).
pub(crate) fn compose_all(edits: &[TextEdit], old_len: usize, new_len: usize) -> Option<TextEdit> {
    let mut len = old_len;
    let mut all: Option<TextEdit> = None;
    for &edit in edits {
        len = edit.apply_len(len)?;
        all = Some(match all {
            Some(all) => compose(all, edit),
            None => edit,
        });
    }
    (len == new_len).then_some(all).flatten()
}

/// `then` after `first`, either of which may be absent.
pub(crate) fn compose_opt(first: Option<TextEdit>, then: TextEdit) -> TextEdit {
    match first {
        Some(first) => compose(first, then),
        None => then,
    }
}

/// The edit from `old` to `new`: the bytes between their common prefix and
/// their common suffix. None when they are equal. Linear in the texts.
pub(crate) fn diff(old: &[u8], new: &[u8]) -> Option<TextEdit> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    if prefix == old.len() && prefix == new.len() {
        return None;
    }
    let room = old.len().min(new.len()) - prefix;
    let suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(room)
        .take_while(|(a, b)| a == b)
        .count();
    Some(TextEdit {
        start: prefix,
        old_end: old.len() - suffix,
        new_end: new.len() - suffix,
    })
}

/// Whether `edit` describes `old` to `new`: the bytes before its start and
/// after its ends are the same. Linear in the texts.
pub(crate) fn describes(edit: TextEdit, old: &[u8], new: &[u8]) -> bool {
    edit.apply_len(old.len()) == Some(new.len())
        && old[..edit.start] == new[..edit.start]
        && old[edit.old_end..] == new[edit.new_end..]
}

/// The start of the line holding `at` (lines end at `\n`).
pub(crate) fn line_start(text: &[u8], at: usize) -> usize {
    let at = at.min(text.len());
    memchr::memrchr(b'\n', &text[..at]).map_or(0, |i| i + 1)
}

/// The byte after the `\n` ending the line that holds `at`, or the text's
/// end.
pub(crate) fn line_end(text: &[u8], at: usize) -> usize {
    let at = at.min(text.len());
    memchr::memchr(b'\n', &text[at..]).map_or(text.len(), |i| at + i + 1)
}

/// The row (from 0) and the byte column of `at`, tree-sitter's point.
pub(crate) fn point(text: &[u8], at: usize) -> (usize, usize) {
    let at = at.min(text.len());
    let row = memchr::memchr_iter(b'\n', &text[..at]).count();
    (row, at - line_start(text, at))
}

const BOM: &[u8] = b"\xEF\xBB\xBF";

/// What the buffer's text leaves out of the file's bytes: a UTF-8 BOM and
/// `\r\n` line endings (all lines end in `\r\n`, or none does: a file with
/// mixed line endings is not editable). Saving puts them back.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Format {
    pub bom: bool,
    pub crlf: bool,
}

impl Format {
    /// The bytes as they are on disk, buffer text in.
    pub(crate) fn plain(self) -> bool {
        !self.bom && !self.crlf
    }

    /// The buffer's text for the file's `bytes`: without the BOM, `\r\n`
    /// as `\n` (a lone `\r` stays). None when the bytes are not UTF-8.
    pub(crate) fn decode(bytes: &[u8]) -> Option<(Arc<str>, Format)> {
        let (bom, body) = match bytes.strip_prefix(BOM) {
            Some(body) => (true, body),
            None => (false, bytes),
        };
        let text = std::str::from_utf8(body).ok()?;
        let crlf = memchr::memmem::find(body, b"\r\n").is_some();
        let text: Arc<str> = if crlf {
            text.replace("\r\n", "\n").into()
        } else {
            text.into()
        };
        Some((text, Format { bom, crlf }))
    }

    /// The file's bytes for the buffer's `text`: the BOM back in front,
    /// every `\n` as `\r\n` (one already after a `\r` stays as it is).
    pub(crate) fn encode(self, text: &str) -> Vec<u8> {
        let bytes = text.as_bytes();
        let mut out = Vec::with_capacity(bytes.len() + if self.bom { BOM.len() } else { 0 });
        if self.bom {
            out.extend_from_slice(BOM);
        }
        if !self.crlf {
            out.extend_from_slice(bytes);
            return out;
        }
        let mut from = 0;
        for at in memchr::memchr_iter(b'\n', bytes) {
            out.extend_from_slice(&bytes[from..at]);
            if at == 0 || bytes[at - 1] != b'\r' {
                out.push(b'\r');
            }
            out.push(b'\n');
            from = at + 1;
        }
        out.extend_from_slice(&bytes[from..]);
        out
    }
}
