//! Test support (docs/design/terminal_view.md「测试」): a fake `dyn Terminals`
//! that records what the view sends and answers with frames the test gives,
//! a builder of frames from the text of their rows and the styles of runs,
//! synthetic shaping of a row, and the fixture of the
//! `terminal_view/snap_full` benchmark. No process and no
//! `lectern-terminal`.
//!
//! The builder is a fixture: its idea of wide characters (CJK, emoji) and of
//! combining marks covers what the tests use, not all of Unicode.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;
use std::sync::{Arc, Mutex};

use gpui::{FontId, GlyphId, LineLayout, Pixels, ShapedGlyph, ShapedRun, point, px};
use lectern_services::project::ProjectKey;
use lectern_services::session::Timestamp;
use lectern_services::terminal::{
    CellStyle, Cluster, Cursor, CursorShape, Frame, FrameColors, FrameInterest, GridRow, GridSize,
    Input, LaunchError, LaunchKind, Modes, OpenSpec, Rgb, RunState, ScrollState, SelectionSpan,
    StyleFlags, StyleRun, TerminalId, TerminalInfo, TerminalList, Terminals, Underline,
};
use lectern_ui::MonoMetrics;

use crate::layout::{RowLayout, place};
use crate::paint::{CellMetrics, Scene, build};

/// The default foreground of the builder's frames.
pub const FOREGROUND: Rgb = Rgb {
    r: 0xd0,
    g: 0xd0,
    b: 0xd0,
};
/// The default background of the builder's frames.
pub const BACKGROUND: Rgb = Rgb {
    r: 0x10,
    g: 0x10,
    b: 0x10,
};
/// The default cursor colour of the builder's frames.
pub const CURSOR: Rgb = Rgb {
    r: 0xf0,
    g: 0xf0,
    b: 0xf0,
};

/// The style of the cells nothing styled.
pub fn plain() -> CellStyle {
    CellStyle {
        fg: FOREGROUND,
        bg: None,
        flags: StyleFlags::default(),
        underline: Underline::None,
        underline_color: None,
    }
}

/// A fake `dyn Terminals`. It records every `input`, `watch`,
/// `copy_selection` and `close`; `frame` and `info` answer with what the test
/// set. A `copy_selection` answers at once with the text given to
/// `set_copy_text`.
#[derive(Default)]
pub struct FakeTerminals {
    state: Mutex<FakeState>,
}

#[derive(Default)]
struct FakeState {
    frames: HashMap<TerminalId, Arc<Frame>>,
    states: HashMap<TerminalId, RunState>,
    closed: Vec<TerminalId>,
    inputs: Vec<(TerminalId, Input)>,
    watches: usize,
    copies: Vec<TerminalId>,
    copy_text: Option<String>,
    drop_copy_reply: bool,
}

impl FakeTerminals {
    pub fn new() -> Arc<FakeTerminals> {
        Arc::new(FakeTerminals::default())
    }

    /// The latest frame of a terminal, which also makes the terminal exist
    /// (`Running` unless `set_state` said otherwise).
    pub fn set_frame(&self, id: TerminalId, frame: Arc<Frame>) {
        let mut state = self.state.lock().unwrap();
        state.closed.retain(|closed| *closed != id);
        state
            .states
            .entry(id)
            .or_insert(RunState::Running { pid: 1 });
        state.frames.insert(id, frame);
    }

    pub fn set_state(&self, id: TerminalId, run_state: RunState) {
        self.state.lock().unwrap().states.insert(id, run_state);
    }

    /// From now on `copy_selection` drops its reply without calling it, as
    /// the terminal does when the calling plugin was deactivated.
    pub fn drop_copy_replies(&self) {
        self.state.lock().unwrap().drop_copy_reply = true;
    }

    /// What the next `copy_selection` replies with; None: no selection.
    pub fn set_copy_text(&self, text: Option<&str>) {
        self.state.lock().unwrap().copy_text = text.map(str::to_owned);
    }

    /// Every input in order, for every terminal.
    pub fn inputs(&self) -> Vec<Input> {
        self.state
            .lock()
            .unwrap()
            .inputs
            .iter()
            .map(|(_, input)| input.clone())
            .collect()
    }

    /// Takes the recorded inputs.
    pub fn take_inputs(&self) -> Vec<Input> {
        std::mem::take(&mut self.state.lock().unwrap().inputs)
            .into_iter()
            .map(|(_, input)| input)
            .collect()
    }

    /// How many times `watch` was called: the view must never call it.
    pub fn watch_calls(&self) -> usize {
        self.state.lock().unwrap().watches
    }

    /// How many times `copy_selection` was called.
    pub fn copy_calls(&self) -> usize {
        self.state.lock().unwrap().copies.len()
    }
}

impl Terminals for FakeTerminals {
    fn open(&self, _: OpenSpec) -> Result<TerminalId, LaunchError> {
        Err(LaunchError::TooMany)
    }

    fn list(&self) -> Arc<TerminalList> {
        let state = self.state.lock().unwrap();
        let mut ids: Vec<_> = state.states.keys().copied().collect();
        ids.sort();
        Arc::new(TerminalList {
            generation: 1,
            terminals: ids
                .into_iter()
                .filter_map(|id| info_of(&state, id))
                .collect(),
        })
    }

    fn info(&self, id: TerminalId) -> Option<TerminalInfo> {
        info_of(&self.state.lock().unwrap(), id)
    }

    fn for_session(&self, _: &lectern_services::session::SessionKey) -> Option<TerminalId> {
        None
    }

    fn frame(&self, id: TerminalId) -> Option<Arc<Frame>> {
        let state = self.state.lock().unwrap();
        if state.closed.contains(&id) {
            return None;
        }
        state.frames.get(&id).cloned()
    }

    fn watch(&self, _: TerminalId) -> Option<FrameInterest> {
        self.state.lock().unwrap().watches += 1;
        Some(FrameInterest::new(|| {}))
    }

    fn input(&self, id: TerminalId, input: Input) {
        self.state.lock().unwrap().inputs.push((id, input));
    }

    fn copy_selection(
        &self,
        id: TerminalId,
        _: lectern_kernel::ActivationToken,
        reply: Box<dyn FnOnce(Option<String>) + Send>,
    ) {
        let text = {
            let mut state = self.state.lock().unwrap();
            state.copies.push(id);
            if state.drop_copy_reply {
                return;
            }
            state.copy_text.clone()
        };
        reply(text);
    }

    fn close(&self, id: TerminalId) {
        self.state.lock().unwrap().closed.push(id);
    }
}

fn info_of(state: &FakeState, id: TerminalId) -> Option<TerminalInfo> {
    if state.closed.contains(&id) {
        return None;
    }
    let run_state = *state.states.get(&id)?;
    Some(TerminalInfo {
        id,
        launch: LaunchKind::Shell,
        project: ProjectKey::Folder {
            root: Arc::from(Path::new("/fake")),
        },
        cwd: Arc::from(Path::new("/fake")),
        state: run_state,
        title: None,
        opened_at: Timestamp(0),
        bells: 0,
        dropped_inputs: 0,
    })
}

/// Starts a frame from the text of its rows. `size` defaults to the widest
/// row (at least 20 columns) by the number of rows.
pub fn frame(lines: &[&str]) -> FrameBuilder {
    FrameBuilder {
        lines: lines.iter().map(|line| (*line).to_owned()).collect(),
        cols: None,
        rows: None,
        styles: Vec::new(),
        wrapped: Vec::new(),
        generation: 1,
        cursor: None,
        selection: Vec::new(),
        scroll: None,
        modes: Modes::default(),
        state: RunState::Running { pid: 1 },
        cell: (8, 16),
    }
}

pub struct FrameBuilder {
    lines: Vec<String>,
    cols: Option<u16>,
    rows: Option<u16>,
    styles: Vec<(usize, u16, u16, CellStyle)>,
    wrapped: Vec<usize>,
    generation: u64,
    cursor: Option<Cursor>,
    selection: Vec<SelectionSpan>,
    scroll: Option<ScrollState>,
    modes: Modes,
    state: RunState,
    cell: (u16, u16),
}

impl FrameBuilder {
    pub fn size(mut self, cols: u16, rows: u16) -> Self {
        self.cols = Some(cols);
        self.rows = Some(rows);
        self
    }

    pub fn generation(mut self, generation: u64) -> Self {
        self.generation = generation;
        self
    }

    /// The columns `start..end` of `row` in `style`.
    pub fn style(mut self, row: usize, start: u16, end: u16, style: CellStyle) -> Self {
        self.styles.push((row, start, end, style));
        self
    }

    /// `row` continues the row above it.
    pub fn wrapped(mut self, row: usize) -> Self {
        self.wrapped.push(row);
        self
    }

    pub fn cursor(mut self, col: u16, row: u16, shape: CursorShape) -> Self {
        self.cursor = Some(Cursor {
            col,
            row,
            shape,
            blinking: false,
            wide: false,
        });
        self
    }

    pub fn wide_cursor(mut self, col: u16, row: u16) -> Self {
        self.cursor = Some(Cursor {
            col,
            row,
            shape: CursorShape::Block,
            blinking: false,
            wide: true,
        });
        self
    }

    pub fn selection(mut self, row: u16, start: u16, end: u16) -> Self {
        self.selection.push(SelectionSpan { row, start, end });
        self
    }

    /// The scrollback: `total` lines, the viewport starting at `offset`.
    pub fn scroll(mut self, total: u32, offset: u32) -> Self {
        self.scroll = Some(ScrollState { total, offset });
        self
    }

    pub fn modes(mut self, modes: Modes) -> Self {
        self.modes = modes;
        self
    }

    pub fn state(mut self, state: RunState) -> Self {
        self.state = state;
        self
    }

    pub fn build(self) -> Arc<Frame> {
        let widest = self
            .lines
            .iter()
            .map(|line| cell_count(line))
            .max()
            .unwrap_or(0);
        let cols = self.cols.unwrap_or_else(|| widest.max(20));
        let rows = self
            .rows
            .unwrap_or_else(|| u16::try_from(self.lines.len()).unwrap_or(u16::MAX).max(1));
        let grid_rows: Vec<Arc<GridRow>> = (0..usize::from(rows))
            .map(|row| {
                let text = self.lines.get(row).map_or("", String::as_str);
                let styles: Vec<_> = self
                    .styles
                    .iter()
                    .filter(|(at, ..)| *at == row)
                    .map(|&(_, start, end, style)| (start, end, style))
                    .collect();
                Arc::new(grid_row(text, cols, &styles, self.wrapped.contains(&row)))
            })
            .collect();
        let total = u32::from(rows);
        Arc::new(Frame {
            generation: self.generation,
            size: GridSize {
                cols,
                rows,
                cell_width: self.cell.0,
                cell_height: self.cell.1,
            },
            rows: Arc::from(grid_rows),
            cursor: self.cursor,
            selection: Arc::from(self.selection),
            colors: FrameColors {
                foreground: FOREGROUND,
                background: BACKGROUND,
                cursor: CURSOR,
            },
            scroll: self.scroll.unwrap_or(ScrollState { total, offset: 0 }),
            modes: self.modes,
            state: self.state,
        })
    }
}

/// How many columns the text takes.
fn cell_count(text: &str) -> u16 {
    clusters_of(text)
        .iter()
        .map(|cluster| if cluster.wide { 2 } else { 1 })
        .sum()
}

struct Piece {
    start: usize,
    end: usize,
    wide: bool,
}

/// The grapheme clusters of the fixture text: a mark or a joiner and the
/// character after a joiner belong to the cluster before.
fn clusters_of(text: &str) -> Vec<Piece> {
    let mut pieces: Vec<Piece> = Vec::new();
    let mut after_joiner = false;
    for (at, ch) in text.char_indices() {
        let joins = after_joiner || is_mark(ch) || ch == '\u{200d}';
        match pieces.last_mut() {
            Some(last) if joins => last.end = at + ch.len_utf8(),
            _ => pieces.push(Piece {
                start: at,
                end: at + ch.len_utf8(),
                wide: is_wide(ch),
            }),
        }
        after_joiner = ch == '\u{200d}';
    }
    pieces
}

fn is_mark(ch: char) -> bool {
    matches!(ch, '\u{0300}'..='\u{036f}' | '\u{fe00}'..='\u{fe0f}')
}

fn is_wide(ch: char) -> bool {
    matches!(
        ch,
        '\u{1100}'..='\u{115f}'
            | '\u{2e80}'..='\u{a4cf}'
            | '\u{ac00}'..='\u{d7a3}'
            | '\u{f900}'..='\u{faff}'
            | '\u{fe30}'..='\u{fe6f}'
            | '\u{ff00}'..='\u{ff60}'
            | '\u{ffe0}'..='\u{ffe6}'
            | '\u{1f300}'..='\u{1f64f}'
            | '\u{1f900}'..='\u{1f9ff}'
    )
}

/// One row of `cols` columns from text and the styles of column ranges. The
/// text is as `GridRow::text` has it: a space for an empty cell, no
/// placeholder for the second cell of a wide character, no trailing spaces.
pub fn grid_row(text: &str, cols: u16, styles: &[(u16, u16, CellStyle)], wrapped: bool) -> GridRow {
    let text = text.trim_end_matches(' ');
    let mut clusters = Vec::new();
    let mut col = 0u16;
    for piece in clusters_of(text) {
        clusters.push(Cluster {
            byte: piece.start as u32,
            col,
            wide: piece.wide,
        });
        col += if piece.wide { 2 } else { 1 };
    }
    let mut per_col = vec![plain(); usize::from(cols)];
    for &(start, end, style) in styles {
        for cell in per_col
            .iter_mut()
            .take(usize::from(end))
            .skip(usize::from(start))
        {
            *cell = style;
        }
    }
    let mut runs: Vec<StyleRun> = Vec::new();
    for (at, style) in per_col.iter().enumerate() {
        let at = at as u16;
        match runs.last_mut() {
            Some(last) if last.style == *style => last.end = at + 1,
            _ => runs.push(StyleRun {
                start: at,
                end: at + 1,
                style: *style,
            }),
        }
    }
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    for cluster in &clusters {
        (cluster.byte, cluster.col, cluster.wide).hash(&mut hasher);
    }
    for run in &runs {
        (run.start, run.end, run.style).hash(&mut hasher);
    }
    wrapped.hash(&mut hasher);
    GridRow {
        key: hasher.finish(),
        text: text.into(),
        clusters: clusters.into(),
        runs: runs.into(),
        wrapped,
    }
}

/// Shaping of a row as a text system gives it for a font whose advance
/// differs a little from the grid: a narrow character advances 97 % of a
/// cell, a wide one 5/3 of a cell (a CJK character is one em, 12 points,
/// where two cells are 14.4), a mark and a joiner do not advance. A glyph is
/// an emoji from U+1F000.
pub fn synthetic_line(row: &GridRow, cell_width: Pixels) -> LineLayout {
    let mut position = px(0.0);
    let mut glyphs = Vec::new();
    for (index, ch) in row.text.char_indices() {
        let advance = if is_mark(ch) || ch == '\u{200d}' {
            px(0.0)
        } else if is_wide(ch) {
            cell_width * (5.0 / 3.0)
        } else {
            cell_width * 0.97
        };
        glyphs.push(ShapedGlyph {
            id: GlyphId(ch as u32),
            position: point(position, px(0.0)),
            index,
            is_emoji: ch >= '\u{1f000}',
        });
        position += advance;
    }
    LineLayout {
        font_size: px(12.0),
        width: position,
        ascent: px(12.3),
        descent: px(3.3),
        runs: vec![ShapedRun {
            font_id: FontId(0),
            glyphs,
        }],
        len: row.text.len(),
    }
}

/// The metrics the test text system gives `TextRole::Code`: 7.2 points wide,
/// 16 high (the ascent 12.3 and the descent 3.3 rounded up to half a point).
pub fn test_metrics() -> MonoMetrics {
    MonoMetrics {
        cell_width: px(7.2),
        line_height: px(16.0),
        ascent: px(12.3),
        descent: px(3.3),
        family: lectern_ui::MONO_FAMILY,
    }
}

/// The fixture of `terminal_view/snap_full`: a frame of 120 by 40 cells,
/// every row different, its rows shaped synthetically. `run` places the
/// glyphs of every row (nothing is cached) and builds the draw list; there is
/// no window.
pub struct SnapFull {
    frame: Arc<Frame>,
    lines: Vec<LineLayout>,
    metrics: CellMetrics,
}

impl SnapFull {
    pub fn new() -> SnapFull {
        let lines: Vec<String> = (0..40).map(snap_line).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut builder = frame(&refs).size(120, 40);
        for row in 0..40usize {
            let mut start = 0u16;
            let mut shade = 0u8;
            while start < 118 {
                let end = (start + 8 + (row as u16 % 3)).min(120);
                let mut style = plain();
                style.fg = Rgb {
                    r: 0x40 + shade.wrapping_mul(7),
                    g: 0x80 + (row as u8).wrapping_mul(3),
                    b: 0xe0u8.wrapping_sub(shade.wrapping_mul(11)),
                };
                if shade.is_multiple_of(5) {
                    style.flags = StyleFlags(StyleFlags::BOLD);
                }
                if shade % 7 == 3 {
                    style.underline = Underline::Single;
                }
                if shade % 11 == 4 {
                    style.underline = Underline::Curly;
                    style.underline_color = Some(Rgb {
                        r: 0xff,
                        g: 0x40,
                        b: 0x40,
                    });
                }
                if shade % 13 == 6 {
                    style.bg = Some(Rgb {
                        r: 0x20,
                        g: 0x30,
                        b: 0x50,
                    });
                }
                builder = builder.style(row, start, end, style);
                start = end;
                shade = shade.wrapping_add(1);
            }
        }
        let frame = builder
            .cursor(10, 5, CursorShape::Block)
            .selection(7, 4, 40)
            .build();
        let metrics = test_metrics();
        let lines = frame
            .rows
            .iter()
            .map(|row| synthetic_line(row, metrics.cell_width))
            .collect();
        SnapFull {
            frame,
            lines,
            metrics: CellMetrics::new(&metrics, 2.0),
        }
    }

    /// Places every row and builds the draw list; the number of operations.
    pub fn run(&self) -> usize {
        let layouts: Vec<Arc<RowLayout>> = self
            .frame
            .rows
            .iter()
            .zip(&self.lines)
            .map(|(row, line)| Arc::new(place(row, line, self.metrics.cell.width)))
            .collect();
        let scene = Scene {
            frame: &self.frame,
            layouts: &layouts,
            origin: point(px(4.0), px(4.0)),
            metrics: self.metrics,
            focused: true,
            selection: gpui::Hsla::default(),
            marked: None,
        };
        let mut ops = Vec::with_capacity(6000);
        build(&scene, &mut ops);
        ops.len()
    }

    /// How many glyphs and box characters the fixture has.
    pub fn cells_drawn(&self) -> usize {
        self.frame.rows.iter().map(|row| row.clusters.len()).sum()
    }
}

impl Default for SnapFull {
    fn default() -> Self {
        SnapFull::new()
    }
}

/// A row of text of the fixture: words, and on some rows a box or CJK text.
fn snap_line(row: usize) -> String {
    const WORDS: [&str; 8] = [
        "terminal", "agent", "review", "grid", "frame", "cursor", "scroll", "paste",
    ];
    match row {
        10 => format!("╭{}╮", "─".repeat(118)),
        11..=13 => format!(
            "│ {:<116} │",
            "│ > fix the failing test and explain the change"
        ),
        14 => format!("╰{}╯", "─".repeat(118)),
        20 => "中文提示与英文对齐网格，没有字形错位。".repeat(3),
        _ => {
            let mut line = String::new();
            let mut word = row;
            while line.chars().count() < 112 {
                line.push_str(WORDS[word % WORDS.len()]);
                line.push(' ');
                word += 3;
            }
            line
        }
    }
}
