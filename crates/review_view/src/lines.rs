//! The text of one diff line and its colouring (docs/design/review_view.md
//! 「高亮」「界面」): where the line is in its side's bytes, what it looks
//! like once tabs are spaces and a long line is cut, and the runs of
//! colour: the highlight tokens of `syntax` through `ui::code_style`, with
//! the word-level changes on top. Pure functions; the renderer draws what
//! comes out.

use std::collections::BTreeSet;
use std::ops::Range;

use gpui::{FontStyle, HighlightStyle, Hsla, SharedString, UnderlineStyle, px};
use lectern_services::changes::SideText;
use lectern_services::review::RowsSide;
use lectern_syntax::Token;
use lectern_ui::{Palette, code_style};

/// A displayed line may be this long (bytes of the source line); a longer one
/// is cut where it is drawn.
pub(crate) const MAX_LINE_BYTES: usize = 4096;

/// How many cells a tab takes.
const TAB_CELLS: usize = 4;

/// One line as it is drawn.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct DisplayLine {
    pub text: SharedString,
    /// The offsets (in the source line) of the tabs, each drawn as
    /// `TAB_CELLS` spaces.
    tabs: Vec<u32>,
    /// How many bytes of the source line the text holds.
    kept: u32,
    /// The source was not valid UTF-8 (or was cut inside a character): the
    /// text has replacement characters and no offsets can be mapped.
    lossy: bool,
}

impl DisplayLine {
    /// The offset in `text` of an offset in the source line.
    fn map(&self, offset: u32) -> usize {
        let before = self.tabs.partition_point(|tab| *tab < offset);
        offset as usize + before * (TAB_CELLS - 1)
    }
}

/// The text of the line of a diff side: from `byte` when it is known,
/// otherwise by counting `line` lines (from 0); without text, empty.
pub(crate) fn display_line(side: &RowsSide, byte: Option<u64>, line: u32) -> DisplayLine {
    let SideText::Text(bytes) = &side.text else {
        return DisplayLine {
            text: SharedString::default(),
            tabs: Vec::new(),
            kept: 0,
            lossy: false,
        };
    };
    let start = match byte {
        Some(byte) if (byte as usize) <= bytes.len() => byte as usize,
        _ => nth_line_start(bytes, line),
    };
    let rest = &bytes[start.min(bytes.len())..];
    let end = rest.iter().position(|b| *b == b'\n').unwrap_or(rest.len());
    let mut source = &rest[..end];
    if source.last() == Some(&b'\r') {
        source = &source[..source.len() - 1];
    }
    let cut = source.len().min(MAX_LINE_BYTES);
    let source = &source[..cut];
    let (decoded, lossy) = match std::str::from_utf8(source) {
        Ok(text) => (std::borrow::Cow::Borrowed(text), false),
        Err(_) => (String::from_utf8_lossy(source), true),
    };
    let tabs: Vec<u32> = if lossy {
        Vec::new()
    } else {
        decoded
            .bytes()
            .enumerate()
            .filter(|(_, b)| *b == b'\t')
            .map(|(at, _)| at as u32)
            .collect()
    };
    let text = if decoded.contains('\t') {
        decoded.replace('\t', &" ".repeat(TAB_CELLS))
    } else {
        decoded.into_owned()
    };
    DisplayLine {
        text: SharedString::from(text),
        tabs,
        kept: cut as u32,
        lossy,
    }
}

/// The offset of line `n` (counted from 0) of `bytes`.
fn nth_line_start(bytes: &[u8], n: u32) -> usize {
    let mut start = 0;
    for _ in 0..n {
        match bytes[start..].iter().position(|b| *b == b'\n') {
            Some(at) => start += at + 1,
            None => return bytes.len(),
        }
    }
    start
}

/// The colour runs of a line: the tokens (ranges in the source line) in
/// the `code_style` of their kind, and the changed words of `inline` on a
/// `background`. Sorted, not overlapping, only where there is something to
/// say. Nothing for a line whose text was repaired (`lossy`).
pub(crate) fn highlights(
    line: &DisplayLine,
    tokens: &[Token],
    inline: &[Range<u32>],
    background: Option<Hsla>,
    palette: &Palette,
) -> Vec<(Range<usize>, HighlightStyle)> {
    if line.lossy {
        return Vec::new();
    }
    let text = line.text.as_ref();
    let mapped = |start: u32, end: u32| -> Option<Range<usize>> {
        let end = end.min(line.kept);
        if start >= end {
            return None;
        }
        let range = line.map(start)..line.map(end);
        (text.is_char_boundary(range.start) && text.is_char_boundary(range.end)).then_some(range)
    };
    let styled: Vec<(Range<usize>, HighlightStyle)> = tokens
        .iter()
        .filter_map(|token| {
            let range = mapped(token.start(), token.end())?;
            let style = code_style(token.kind(), palette);
            Some((
                range,
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
                },
            ))
        })
        .collect();
    let changed: Vec<Range<usize>> = match background {
        Some(_) => inline
            .iter()
            .filter_map(|range| mapped(range.start, range.end))
            .collect(),
        None => Vec::new(),
    };
    if changed.is_empty() {
        return styled;
    }
    let mut points: BTreeSet<usize> = BTreeSet::new();
    for (range, _) in &styled {
        points.insert(range.start);
        points.insert(range.end);
    }
    for range in &changed {
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
        let in_changed = changed
            .iter()
            .any(|range| range.start <= from && to <= range.end);
        let mut style = token.map(|(_, style)| *style).unwrap_or_default();
        if in_changed {
            style.background_color = background;
        }
        if style != HighlightStyle::default() {
            out.push((from..to, style));
        }
    }
    out
}
