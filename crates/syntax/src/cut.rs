//! Where a window of the chain is cut (docs/design/file_reader.md
//! `ChainWindow`): the same rule as code_intel's windows (code_intel design
//! 「大文件」「窗口」, `crates/code_intel/src/window.rs`), on bytes read through
//! a [`Source`](crate::highlight::Source) instead of one slice.
//!
//! The cut, from the root down: the last complete named child (not
//! `ERROR`, no error or `MISSING` inside, ending strictly before the
//! window's end) of the root; when the root has none, the same in the child
//! the window truncated, and so on down until a level has one. A node or
//! leaf ending at the window's end never counts: the parser took the
//! window's end for the file's end. A window with no complete node is
//! widened once; then the end of the last leaf before the window's end;
//! then the first identifier boundary at or after the window's end.

use tree_sitter::{Node, Tree};

/// The bytes of a window, by offset from its start.
pub(crate) trait Bytes {
    fn byte(&self, at: usize) -> Option<u8>;
}

/// An identifier's byte (code_intel's whole-word rule): ASCII letters,
/// digits, `_`, `$`, and every byte of a UTF-8 multi-byte sequence.
pub(crate) fn ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}

/// The tree's cut of a window of `len` bytes (its end not the file's);
/// None when no complete node ends before the window's end.
///
/// Inside an `ERROR` node the children are the parser's stack: pieces of
/// open constructs and the complete statements inside them. There a child
/// counts only when it ends as a statement does: the next child starts on
/// a later line (or none follows but a line break does), or its last byte
/// is `;`.
pub(crate) fn tree_cut(tree: &Tree, window: &dyn Bytes, len: usize) -> Option<usize> {
    let mut node = tree.root_node();
    loop {
        let pieces = node.is_error();
        let mut cursor = node.walk();
        let mut truncated: Option<Node<'_>> = None;
        let mut complete = None;
        // A complete child of an ERROR node waiting for the next child.
        let mut pending: Option<Node<'_>> = None;
        for child in node.children(&mut cursor) {
            if let Some(p) = pending.take()
                && child.start_position().row > p.end_position().row
            {
                complete = Some(p.end_byte());
            }
            if !child.is_named() {
                continue;
            }
            let end = child.end_byte();
            if end >= len {
                truncated = Some(child);
            } else if !child.is_error() && !child.has_error() && !child.is_missing() && end > 0 {
                if !pieces || window.byte(end - 1) == Some(b';') {
                    complete = Some(end);
                } else {
                    pending = Some(child);
                }
            }
        }
        // The last one, when a line break follows it: with nothing after it
        // on its line it ended as a statement does. Not otherwise: on the
        // window's last line it may be complete only because the window
        // ends there (`return` before ` f(x)}` in one minified line, closed
        // by an automatic semicolon), the same rule as code_intel's cut.
        if let Some(p) = pending
            && node.end_position().row > p.end_position().row
        {
            complete = Some(p.end_byte());
        }
        if complete.is_some() {
            return complete;
        }
        node = truncated?;
    }
}

/// The end of the last leaf (token) ending strictly before `len`, of
/// non-zero width and not `MISSING`; None when there is none.
pub(crate) fn leaf_cut(tree: &Tree, len: usize) -> Option<usize> {
    let mut best: Option<usize> = None;
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        if node.start_byte() >= len {
            return best;
        }
        if node.child_count() == 0 {
            let (start, end) = (node.start_byte(), node.end_byte());
            if end < len && end > start && !node.is_missing() {
                best = Some(best.map_or(end, |b| b.max(end)));
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return best;
            }
        }
    }
}

/// How much of a window of `len` bytes (not the file's end) to parse, so
/// that the parser sees whole lines and, in ordinary code, whole top-level
/// constructs: up to the last line in its second half that starts a
/// statement at column 0 (not blank, not a closing bracket), else up to its
/// last line break there, else all of it (one long line). `line_starts`
/// gives the window's line starts after its first byte, ascending.
pub(crate) fn parse_len(
    window: &dyn Bytes,
    len: usize,
    line_starts: impl Iterator<Item = usize>,
) -> usize {
    let half = len / 2;
    let mut statement = None;
    let mut last = None;
    for start in line_starts.filter(|&s| s >= half && s <= len) {
        last = Some(start);
        let starts_statement = window
            .byte(start)
            .filter(|_| start < len)
            .is_some_and(|b| !b.is_ascii_whitespace() && !matches!(b, b'}' | b')' | b']'));
        if starts_statement {
            statement = Some(start);
        }
    }
    statement.or(last).unwrap_or(len)
}
