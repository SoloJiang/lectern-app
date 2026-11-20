//! What the view makes of the text of one display row: tab stops, the
//! colour runs, and the words of the toolbar's second line
//! (docs/design/file_reader.md「文本与显示行」「界面」). Pure functions; the
//! renderer draws what comes out.

use std::collections::BTreeSet;
use std::ops::Range;

use gpui::{FontStyle, HighlightStyle, SharedString, UnderlineStyle, px};
use lectern_services::project::BlobId;
use lectern_syntax::{Token, TokenKind};
use lectern_ui::{Palette, SystemColor, code_style};

/// Tab stops every this many cells (`syntax`'s `cells::TAB`).
const TAB_CELLS: usize = 4;

/// The text of one row as it is drawn: its tabs are spaces.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Shown {
    pub text: SharedString,
    /// For each tab, the offset it had in the row's text and the bytes that
    /// the tabs up to it and itself added.
    tabs: Vec<(u32, u32)>,
}

impl Shown {
    /// The offset in `text` of an offset in the row's text.
    pub(crate) fn map(&self, offset: u32) -> usize {
        let at = self.tabs.partition_point(|(tab, _)| *tab < offset);
        let added = if at == 0 { 0 } else { self.tabs[at - 1].1 };
        offset as usize + added as usize
    }
}

/// `text` with every tab replaced by the spaces up to the next tab stop.
/// A cell is a character here (`syntax` counts a wide character as two, so
/// the stops after one are off by a cell, a known limitation of the view).
pub(crate) fn expand(text: &str) -> Shown {
    if !text.contains('\t') {
        return Shown {
            text: SharedString::from(text.to_owned()),
            tabs: Vec::new(),
        };
    }
    let mut out = String::with_capacity(text.len() + 8);
    let mut tabs = Vec::new();
    let mut cell = 0usize;
    let mut added = 0u32;
    for (offset, c) in text.char_indices() {
        if c == '\t' {
            let spaces = TAB_CELLS - cell % TAB_CELLS;
            out.extend(std::iter::repeat_n(' ', spaces));
            added += spaces as u32 - 1;
            tabs.push((offset as u32, added));
            cell += spaces;
        } else {
            out.push(c);
            cell += 1;
        }
    }
    Shown {
        text: SharedString::from(out),
        tabs,
    }
}

/// The GPUI style of a token kind: the `ui` code colouring (every colour
/// from the palette) as a `HighlightStyle`. The reading view's rows and the
/// edit mode's highlighter both draw with it.
pub(crate) fn highlight_style(kind: TokenKind, palette: &Palette) -> HighlightStyle {
    let style = code_style(kind, palette);
    HighlightStyle {
        color: Some(style.color),
        font_weight: (style.weight != gpui::FontWeight::NORMAL).then_some(style.weight),
        font_style: style.italic.then_some(FontStyle::Italic),
        underline: style.underline.then(|| UnderlineStyle {
            thickness: px(1.0),
            color: Some(style.color),
            wavy: false,
        }),
        ..HighlightStyle::default()
    }
}

/// The colour runs of one row: the highlight tokens in the `code_style` of
/// their kind, the find matches on a tint and the current match on a
/// stronger one. Ranges are in the row's text (`Row::Text::tokens` and
/// `found`) and are moved to the drawn text. Sorted, not overlapping, only
/// where there is something to say.
pub(crate) fn runs(
    shown: &Shown,
    tokens: &[Token],
    found: &[Range<u32>],
    current: Option<&Range<u32>>,
    palette: &Palette,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let text: &str = shown.text.as_ref();
    let mapped = |start: u32, end: u32| -> Option<Range<usize>> {
        if start >= end {
            return None;
        }
        let range = shown.map(start)..shown.map(end).min(text.len());
        (range.start < range.end
            && text.is_char_boundary(range.start)
            && text.is_char_boundary(range.end))
        .then_some(range)
    };
    let styled: Vec<(Range<usize>, HighlightStyle)> = tokens
        .iter()
        .filter_map(|token| {
            let range = mapped(token.start(), token.end())?;
            Some((range, highlight_style(token.kind(), palette)))
        })
        .collect();
    let matches: Vec<Range<usize>> = found
        .iter()
        .filter_map(|range| mapped(range.start, range.end))
        .collect();
    if matches.is_empty() {
        return styled;
    }
    let current = current.and_then(|range| mapped(range.start, range.end));
    let tint = palette.system(SystemColor::Yellow).opacity(MATCH_OPACITY);
    let strong = palette.system(SystemColor::Orange).opacity(CURRENT_OPACITY);
    let mut points: BTreeSet<usize> = BTreeSet::new();
    for (range, _) in &styled {
        points.insert(range.start);
        points.insert(range.end);
    }
    for range in matches.iter().chain(current.iter()) {
        points.insert(range.start);
        points.insert(range.end);
    }
    let points: Vec<usize> = points.into_iter().collect();
    let mut out = Vec::new();
    for pair in points.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        let token = styled
            .iter()
            .find(|(range, _)| range.start <= from && to <= range.end);
        let mut style = token.map(|(_, style)| *style).unwrap_or_default();
        if matches
            .iter()
            .any(|range| range.start <= from && to <= range.end)
        {
            style.background_color = Some(tint);
        }
        if current
            .as_ref()
            .is_some_and(|range| range.start <= from && to <= range.end)
        {
            style.background_color = Some(strong);
        }
        if style != HighlightStyle::default() {
            out.push((from..to, style));
        }
    }
    out
}

/// How much of the system yellow a find match keeps, and how much of the
/// system orange the current one (`ui`'s palette has no find colour).
const MATCH_OPACITY: f32 = 0.35;
const CURRENT_OPACITY: f32 = 0.55;

/// The last part of a relative path.
pub(crate) fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The package a dependency file belongs to, as the toolbar names it:
/// `react 19.0.0` for a file under `node_modules/.pnpm/react@19.0.0/…`, the
/// package's name alone (`@types/react`) where the path does not carry the
/// version. None for a path under no `node_modules`.
pub(crate) fn dependency_label(path: &str) -> Option<String> {
    const MODULES: &str = "node_modules/";
    let at = path.rfind(MODULES)?;
    let after = &path[at + MODULES.len()..];
    let mut parts = after.split('/');
    let first = parts.next().filter(|p| !p.is_empty())?;
    let name = if first.starts_with('@') {
        format!("{first}/{}", parts.next().filter(|p| !p.is_empty())?)
    } else {
        first.to_owned()
    };
    // pnpm's virtual store names the version: `.pnpm/<name>@<version>[_peers]/`.
    let version = path.find(".pnpm/").and_then(|at| {
        let store = path[at + ".pnpm/".len()..].split('/').next()?;
        let (_, version) = store.rsplit_once('@')?;
        let version = version.split('_').next().filter(|v| !v.is_empty())?;
        Some(version.to_owned())
    });
    Some(match version {
        Some(version) => format!("{name} {version}"),
        None => name,
    })
}

/// The first seven hex digits of an OID: where a version from history comes
/// from, as the toolbar names it.
pub(crate) fn short_oid(oid: BlobId) -> String {
    oid.to_string().chars().take(7).collect()
}

/// How many decimal digits `n` has.
pub(crate) fn digits(n: u64) -> usize {
    n.checked_ilog10().map_or(1, |log| log as usize + 1)
}

/// Whether the cell of a row's text holds a character of an identifier,
/// and every character before it is one cell of one byte (printable ASCII),
/// so that the cell is also the byte's offset in the line.
pub(crate) fn identifier_cell(text: &str, cell: usize) -> bool {
    let bytes = text.as_bytes();
    let Some(&at) = bytes.get(cell) else {
        return false;
    };
    bytes[..=cell].iter().all(|b| (0x20..0x7f).contains(b))
        && (at.is_ascii_alphanumeric() || at == b'_' || at == b'$')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_tabs_go_to_the_next_stop_and_offsets_follow() {
        let shown = expand("\tab\tc");
        assert_eq!(shown.text, "    ab  c");
        // "\t" at 0 became 4 bytes, "ab" starts at 4; the second tab (at
        // offset 3 of the row's text) went to the stop at cell 8.
        assert_eq!(shown.map(0), 0);
        assert_eq!(shown.map(1), 4);
        assert_eq!(shown.map(3), 6);
        assert_eq!(shown.map(4), 8);
        assert_eq!(shown.map(5), 9);
        assert_eq!(expand("plain").text, "plain");
        assert_eq!(expand("plain").map(3), 3);
    }

    #[test]
    fn text_dependency_labels_come_from_the_path() {
        assert_eq!(
            dependency_label("node_modules/react/index.d.ts").as_deref(),
            Some("react")
        );
        assert_eq!(
            dependency_label("node_modules/@types/react/index.d.ts").as_deref(),
            Some("@types/react")
        );
        assert_eq!(
            dependency_label("node_modules/.pnpm/react@19.0.0/node_modules/react/index.js")
                .as_deref(),
            Some("react 19.0.0")
        );
        assert_eq!(
            dependency_label(
                "node_modules/.pnpm/react-dom@19.1.0_react@19.1.0/node_modules/react-dom/x.js"
            )
            .as_deref(),
            Some("react-dom 19.1.0")
        );
        assert_eq!(dependency_label("src/lib.rs"), None);
        assert_eq!(dependency_label("node_modules/"), None);
    }

    #[test]
    fn text_digits_and_names() {
        assert_eq!(digits(0), 1);
        assert_eq!(digits(9), 1);
        assert_eq!(digits(10), 2);
        assert_eq!(digits(123_456), 6);
        assert_eq!(file_name("a/b/c.rs"), "c.rs");
        assert_eq!(file_name("c.rs"), "c.rs");
        assert_eq!(short_oid(BlobId::Sha1([0xab; 20])), "abababa");
    }

    #[test]
    fn text_an_identifier_cell_needs_one_byte_cells_before_it() {
        assert!(identifier_cell("let value = 1;", 4));
        assert!(!identifier_cell("let value = 1;", 3), "a space");
        assert!(!identifier_cell("let value = 1;", 20), "past the end");
        // A tab or a non-ASCII character before the cell: the cell is not
        // the byte's offset any more.
        assert!(!identifier_cell("\tvalue", 1));
        assert!(!identifier_cell("é value", 3));
        assert!(identifier_cell("$a_1", 3));
    }
}
