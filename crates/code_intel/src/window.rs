//! Windows of an opened large file (docs/design/code_intel.md「大文件」, ADR
//! 0034): where a window parsed alone is cut, so that the next window
//! starts at a boundary its tree confirmed and no name is split, and the
//! shift of a window's positions into the whole file's.
//!
//! The cut, from the root down: the last complete named child (not
//! `ERROR`, no error or `MISSING` inside, ending strictly before the
//! window's end) of the root; when the root has none, the same in the child
//! the window truncated (the one ending at the window's end), and so on
//! down until a level has one: the last statement or member boundary the
//! tree confirms. A node or leaf ending at the window's end never counts:
//! the parser took the window's end for the file's, so `const fo` of
//! `const foo = 1` is a complete declaration and `fo` a complete identifier
//! there. A node ending strictly before the end has at least one more byte
//! the parser saw after its last token, so that token (an identifier, a
//! number, a string) was ended by the lexer.
//!
//! The first level with a complete child, not the deepest one: the deepest
//! complete child of a truncated top-level function is often an identifier
//! in its last statement, and cutting there splits a top-level node that
//! the root's own boundary keeps whole (「与设计的差异」第 8 步). A file
//! wrapped in one function (an IIFE, a UMD bundle, one minified line) has
//! no complete child at the root: the cut goes down into the function, and
//! from the next window on the windows start between its statements.

use tree_sitter::{Node, Tree};

use crate::Position;
use crate::extract::Def;

/// A window's length (「大文件」): about 64 KiB, its tree 2–5 MiB.
pub const WINDOW: usize = 64 << 10;
/// A window with no complete node is widened to this once.
pub const WIDE_WINDOW: usize = 256 << 10;

/// An identifier's byte, as the time-boxed search's whole-word rule: ASCII
/// letters, digits, `_`, `$`, and every byte of a UTF-8 multi-byte
/// sequence.
pub fn ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}

/// The tree's cut of a window of `len` bytes (its end not the file's);
/// None when no complete node ends before the window's end.
///
/// Inside an `ERROR` node (the root of a window whose open constructs the
/// parser could not close) the children are the parser's stack: tokens
/// and pieces of the open constructs (`export`, `class`, a name, a
/// parameter list, `{`), and the complete statements inside them. A piece
/// is complete by itself, and cutting after one would leave the
/// definition it names in neither window, so there a child counts only
/// when it ends as a statement does: the next child starts on a later
/// line (or none follows; a piece shares its line with the rest of its
/// construct), or its last byte is `;` (one minified line).
pub(crate) fn tree_cut(tree: &Tree, window: &[u8]) -> Option<usize> {
    let len = window.len();
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
                if !pieces || window[end - 1] == b';' {
                    complete = Some(end);
                } else {
                    pending = Some(child);
                }
            }
        }
        if let Some(p) = pending {
            complete = Some(p.end_byte());
        }
        if complete.is_some() {
            return complete;
        }
        node = truncated?;
    }
}

/// How much of a window read up to `bytes.len()` (not the file's end) to
/// parse, so that the parser sees whole lines and, in ordinary code, whole
/// top-level constructs: up to the last line in its second half that
/// starts a statement at column 0 (not blank, not a closing bracket), else
/// up to its last line break there (an indented body: a function wrapping
/// the file), else all of it (one long line).
pub fn parse_len(bytes: &[u8]) -> usize {
    let half = bytes.len() / 2;
    let mut fallback = None;
    for i in memchr::memrchr_iter(b'\n', bytes) {
        let start = i + 1;
        if start < half {
            break;
        }
        fallback.get_or_insert(start);
        let starts_statement = bytes
            .get(start)
            .is_some_and(|b| !b.is_ascii_whitespace() && !matches!(b, b'}' | b')' | b']'));
        if starts_statement {
            return start;
        }
    }
    fallback.unwrap_or(bytes.len())
}

/// The end of the last leaf (token) ending strictly before `len`, of
/// non-zero width and not `MISSING`; None when there is none.
pub(crate) fn leaf_cut(tree: &Tree, len: usize) -> Option<usize> {
    let mut best: Option<usize> = None;
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        if node.start_byte() >= len {
            // Every later node starts later.
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

/// From `from` in `bytes`, the first byte that is not an identifier's: the
/// identifier boundary at or after `from`. None when `bytes` ends before
/// one (read more, or the file ends there).
pub fn ident_end(bytes: &[u8], from: usize) -> Option<usize> {
    bytes
        .get(from..)?
        .iter()
        .position(|b| !ident(*b))
        .map(|i| from + i)
}

/// Where a window starts in its file: the byte, and the line and byte
/// column of that byte.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Base {
    pub byte: u64,
    pub line: u32,
    pub column: u32,
}

impl Base {
    /// The base after `bytes` (this base's window's bytes, up to the next
    /// window's start).
    pub fn after(self, bytes: &[u8]) -> Base {
        let newlines = memchr::memchr_iter(b'\n', bytes).count() as u32;
        let column = match memchr::memrchr(b'\n', bytes) {
            Some(i) => (bytes.len() - i - 1) as u32,
            None => self.column + bytes.len() as u32,
        };
        Base {
            byte: self.byte + bytes.len() as u64,
            line: self.line + newlines,
            column,
        }
    }

    /// A position in the window, in the file.
    pub fn position(self, at: Position) -> Position {
        Position {
            line: self.line + at.line,
            column: if at.line == 0 {
                self.column + at.column
            } else {
                at.column
            },
        }
    }

    /// A definition of the window, in the file.
    pub fn def(self, mut def: Def) -> Def {
        let at = self.position(Position {
            line: def.line,
            column: def.column,
        });
        def.line = at.line;
        def.column = at.column;
        def.byte += self.byte;
        def.end_line += self.line;
        def
    }
}
