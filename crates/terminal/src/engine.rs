//! One terminal's libghostty-vt state: program output in, encoded input and
//! query replies out, viewport frames on demand (design 「数据流与线程」).
//!
//! Everything here is `!Send` (libghostty-vt's types are) and lives on the
//! terminal's own thread. The engine does no IO: the caller feeds it the
//! PTY's output and writes `take_pty_output` back to the PTY.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use lectern_services::terminal::{
    ColorScheme, Cursor, CursorShape, Frame, FrameColors, GridRow, GridSize, Input, KeyInput,
    Modes, MouseButton, MouseInput, MouseKind, RunState, ScrollInput, ScrollState, ScrollTarget,
    SelectionSpan, Theme,
};
use libghostty_vt::fmt::Format;
use libghostty_vt::render::{CellIterator, CursorVisualStyle, Dirty, RowIterator};
use libghostty_vt::selection::{FormatOptions, SelectLineOptions, SelectWordOptions, Selection};
use libghostty_vt::terminal::{
    ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType, Mode, Point,
    PointCoordinate, PointSpace, PrimaryDeviceAttributes, ScrollViewport,
    SecondaryDeviceAttributes, SizeReportSize, TertiaryDeviceAttributes,
};
use libghostty_vt::{RenderState, Terminal, TerminalOptions, focus, key, mouse, paste};

use crate::grid::{RowBuilder, reuse};
use crate::keys;
use crate::pacing::Pacer;
use crate::palette::{palette, rgb};

/// Scrollback limit in bytes. libghostty-vt counts bytes, not rows: about
/// 1 KiB per 120-column row, so 12 MiB holds a little over 10,000 rows of 120
/// columns (design 「性能」, 「与设计的差异」).
pub(crate) const MAX_SCROLLBACK_BYTES: usize = 12 << 20;

const XTVERSION: &str = concat!("Lectern ", env!("CARGO_PKG_VERSION"));

/// What libghostty-vt's callbacks recorded during `vt_write`.
#[derive(Default)]
struct Effects {
    /// Bytes for the PTY: query replies and encoded input, in order.
    pty: Vec<u8>,
    bells: u32,
    title_changed: bool,
}

/// What the program output did besides changing the screen, since the last
/// `take_effects`.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct OutputEffects {
    pub bells: u32,
    pub title_changed: bool,
}

pub(crate) struct Engine {
    // Field order is drop order: the iterators and render state before the
    // terminal they were filled from.
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    render: RenderState<'static>,
    key_encoder: key::Encoder<'static>,
    mouse_encoder: mouse::Encoder<'static>,
    terminal: Terminal<'static, 'static>,
    effects: Rc<RefCell<Effects>>,
    /// Read by the size and colour-scheme callbacks.
    size: Rc<Cell<GridSize>>,
    scheme: Rc<Cell<ColorScheme>>,
    theme: Theme,
    state: RunState,
    builder: RowBuilder,
    previous: Option<Arc<Frame>>,
    generation: u64,
    /// Rebuild every row on the next frame (resize, theme).
    full: bool,
    pacer: Pacer,
    /// Mouse buttons held while the program tracks the mouse.
    buttons_down: u8,
    /// Where a selection drag started, in screen coordinates.
    anchor: Option<PointCoordinate>,
}

impl Engine {
    pub(crate) fn new(size: GridSize, theme: Theme) -> libghostty_vt::error::Result<Engine> {
        let size = sanitize(size);
        let effects = Rc::new(RefCell::new(Effects::default()));
        let shared_size = Rc::new(Cell::new(size));
        let scheme = Rc::new(Cell::new(theme.scheme));
        let mut terminal = Terminal::new(TerminalOptions {
            cols: size.cols,
            rows: size.rows,
            max_scrollback: MAX_SCROLLBACK_BYTES,
        })?;
        terminal.resize(
            size.cols,
            size.rows,
            u32::from(size.cell_width),
            u32::from(size.cell_height),
        )?;
        // Grapheme clustering (mode 2027), as Ghostty itself defaults to: ZWJ
        // emoji sequences and skin tones stay in one cell (design 「网格与颜色」).
        terminal.set_mode(Mode::GRAPHEME_CLUSTER, true)?;
        // No `on_clipboard_write` (OSC 52 is ignored) and no `on_pwd_changed`
        // (OSC 7 is not used): design 「安全边界」.
        terminal
            .on_pty_write({
                let effects = effects.clone();
                move |_, data| {
                    if !is_title_report(data) {
                        effects.borrow_mut().pty.extend_from_slice(data);
                    }
                }
            })?
            .on_bell({
                let effects = effects.clone();
                move |_| effects.borrow_mut().bells += 1
            })?
            .on_title_changed({
                let effects = effects.clone();
                move |_| effects.borrow_mut().title_changed = true
            })?
            // ENQ gets an empty answer.
            .on_enquiry(|_| None)?
            .on_xtversion(|_| Some(XTVERSION))?
            .on_size({
                let size = shared_size.clone();
                move |_| {
                    let size = size.get();
                    Some(SizeReportSize {
                        rows: size.rows,
                        columns: size.cols,
                        cell_width: u32::from(size.cell_width),
                        cell_height: u32::from(size.cell_height),
                    })
                }
            })?
            .on_color_scheme({
                let scheme = scheme.clone();
                move |_| Some(vt_scheme(scheme.get()))
            })?
            .on_device_attributes(|_| Some(device_attributes()))?;
        let mut engine = Engine {
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            render: RenderState::new()?,
            key_encoder: key::Encoder::new()?,
            mouse_encoder: mouse::Encoder::new()?,
            terminal,
            effects,
            size: shared_size,
            scheme,
            theme,
            state: RunState::Starting,
            builder: RowBuilder::default(),
            previous: None,
            generation: 0,
            full: true,
            pacer: Pacer::default(),
            buttons_down: 0,
            anchor: None,
        };
        engine.apply_theme(theme)?;
        Ok(engine)
    }

    /// Program output from the PTY.
    pub(crate) fn write_output(&mut self, bytes: &[u8], now: Instant) {
        self.terminal.vt_write(bytes);
        let sync = self.mode(Mode::SYNC_OUTPUT);
        self.pacer.changed(now, sync);
    }

    pub(crate) fn set_state(&mut self, state: RunState, now: Instant) {
        if self.state != state {
            self.state = state;
            self.pacer.changed(now, self.mode(Mode::SYNC_OUTPUT));
        }
    }

    /// Input from the view. Input meant for the program is dropped unless
    /// the program is running (`Terminals::input`).
    pub(crate) fn input(&mut self, input: Input, now: Instant) -> libghostty_vt::error::Result<()> {
        let running = matches!(self.state, RunState::Running { .. });
        let mut changed = false;
        match input {
            Input::Key(key) if running => self.key(&key)?,
            Input::Text(text) if running => self.write_pty(text.as_bytes()),
            Input::Paste(text) if running => self.paste(&text)?,
            Input::Focus(gained) if running => {
                if self.mode(Mode::FOCUS_EVENT) {
                    let mut buf = [0u8; 8];
                    let event = if gained {
                        focus::Event::Gained
                    } else {
                        focus::Event::Lost
                    };
                    let n = event.encode(&mut buf)?;
                    self.write_pty(&buf[..n]);
                }
            }
            Input::Key(_) | Input::Text(_) | Input::Paste(_) | Input::Focus(_) => {}
            Input::Mouse(mouse) => {
                // Whoever gets the event, the buttons held stay right, so a
                // program that starts tracking later sees no stuck drag.
                let bit = button_bit(mouse.button);
                match mouse.kind {
                    MouseKind::Down => self.buttons_down |= bit,
                    MouseKind::Up => self.buttons_down &= !bit,
                    MouseKind::Drag | MouseKind::Move => {}
                }
                if self.tracks_mouse(mouse.mods.shift) {
                    if running {
                        self.mouse(&mouse)?;
                    }
                } else {
                    changed = self.select(&mouse)?;
                }
            }
            Input::Scroll(scroll) => changed = self.scroll(&scroll, running)?,
            Input::ScrollTo(target) => {
                let page = self.size.get().rows as isize;
                self.terminal.scroll_viewport(match target {
                    ScrollTarget::Top => ScrollViewport::Top,
                    ScrollTarget::Bottom => ScrollViewport::Bottom,
                    ScrollTarget::PageUp => ScrollViewport::Delta(-page),
                    ScrollTarget::PageDown => ScrollViewport::Delta(page),
                });
                changed = true;
            }
            Input::Resize(size) => {
                let size = sanitize(size);
                if size != self.size.get() {
                    self.terminal.resize(
                        size.cols,
                        size.rows,
                        u32::from(size.cell_width),
                        u32::from(size.cell_height),
                    )?;
                    self.size.set(size);
                    // The text reflows: a drag starts again.
                    self.anchor = None;
                    self.full = true;
                    changed = true;
                }
            }
            Input::SelectAll => {
                let all = self.terminal.select_all()?;
                self.terminal.set_selection(all.as_ref())?;
                self.anchor = None;
                changed = true;
            }
            Input::ClearSelection => {
                self.terminal.set_selection(None)?;
                self.anchor = None;
                changed = true;
            }
            Input::ClearScrollback => {
                // ED 3: erase the scrollback, leave the screen as it is.
                self.terminal.vt_write(b"\x1b[3J");
                self.anchor = None;
                changed = true;
            }
            Input::SetTheme(theme) => {
                let scheme_changed = theme.scheme != self.theme.scheme;
                self.apply_theme(theme)?;
                if scheme_changed && running && self.mode(Mode::COLOR_SCHEME_REPORT) {
                    let mut buf = [0u8; 16];
                    let n = vt_scheme(theme.scheme).encode_report(&mut buf)?;
                    self.write_pty(&buf[..n]);
                }
                changed = true;
            }
        }
        if changed {
            self.pacer.changed(now, self.mode(Mode::SYNC_OUTPUT));
        }
        Ok(())
    }

    /// Bytes to write to the PTY: query replies and encoded input, in order.
    pub(crate) fn take_pty_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.effects.borrow_mut().pty)
    }

    pub(crate) fn take_effects(&mut self) -> OutputEffects {
        let mut effects = self.effects.borrow_mut();
        let out = OutputEffects {
            bells: effects.bells,
            title_changed: effects.title_changed,
        };
        effects.bells = 0;
        effects.title_changed = false;
        out
    }

    /// The title set with OSC 0 / 2, if any.
    pub(crate) fn title(&self) -> Option<Arc<str>> {
        match self.terminal.title() {
            Ok(title) if !title.is_empty() => Some(Arc::from(title)),
            _ => None,
        }
    }

    /// The selection's plain text: soft wraps joined, trailing blanks trimmed.
    pub(crate) fn copy_selection(&self) -> Option<String> {
        let options = FormatOptions::new()
            .with_emit_format(Format::Plain)
            .with_unwrap(true)
            .with_trim(true);
        let bytes = self.terminal.format_selection_alloc(None, options).ok()??;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Whether a frame is due at `now` (design 「帧的节奏」).
    pub(crate) fn frame_due(&self, now: Instant) -> bool {
        self.pacer.is_due(now)
    }

    /// Builds and publishes a frame at `now`.
    pub(crate) fn publish(&mut self, now: Instant) -> libghostty_vt::error::Result<Arc<Frame>> {
        let frame = self.frame()?;
        self.pacer.published(now);
        Ok(frame)
    }

    /// Builds a frame of the viewport. Rows whose content did not change are
    /// the previous frame's `Arc`.
    pub(crate) fn frame(&mut self) -> libghostty_vt::error::Result<Arc<Frame>> {
        let size = self.size.get();
        let previous = self.previous.take();
        let snapshot = self.render.update(&self.terminal)?;
        let colors = snapshot.colors()?;
        let previous_rows = previous
            .as_ref()
            .map(|f| f.rows.clone())
            .filter(|rows| rows.len() == usize::from(size.rows));
        let full = self.full || snapshot.dirty()? == Dirty::Full || previous_rows.is_none();
        let mut rows: Vec<Arc<GridRow>> = Vec::with_capacity(usize::from(size.rows));
        let mut selection = Vec::new();
        let mut iteration = self.rows.update(&snapshot)?;
        while let Some(row) = iteration.next() {
            let y = rows.len();
            let prev = previous_rows.as_ref().and_then(|r| r.get(y));
            if let Some(span) = row.selection()? {
                selection.push(SelectionSpan {
                    row: y as u16,
                    start: span.start_x,
                    end: span.end_x.saturating_add(1),
                });
            }
            let built = match prev {
                Some(prev) if !full && !row.dirty()? => prev.clone(),
                _ => {
                    let wrapped = row.raw_row()?.is_wrap_continuation()?;
                    let mut cells = self.cells.update(row)?;
                    reuse(prev, self.builder.build(&mut cells, &colors, wrapped)?)
                }
            };
            row.set_dirty(false)?;
            rows.push(built);
        }
        let cursor = if snapshot.cursor_visible()? {
            snapshot.cursor_viewport()?.map(|at| {
                let (col, wide) = if at.at_wide_tail {
                    (at.x.saturating_sub(1), true)
                } else {
                    let wide = rows
                        .get(usize::from(at.y))
                        .is_some_and(|row| row.clusters.iter().any(|c| c.col == at.x && c.wide));
                    (at.x, wide)
                };
                (col, at.y, wide)
            })
        } else {
            None
        };
        let cursor = match cursor {
            Some((col, row, wide)) => Some(Cursor {
                col,
                row,
                wide,
                shape: match snapshot.cursor_visual_style()? {
                    CursorVisualStyle::Bar => CursorShape::Bar,
                    CursorVisualStyle::Underline => CursorShape::Underline,
                    _ => CursorShape::Block,
                },
                blinking: snapshot.cursor_blinking()?,
            }),
            None => None,
        };
        snapshot.set_dirty(Dirty::Clean)?;
        let foreground = crate::palette::from_vt(colors.foreground);
        let frame_colors = FrameColors {
            foreground,
            background: crate::palette::from_vt(colors.background),
            cursor: colors.cursor.map_or(foreground, crate::palette::from_vt),
        };
        let scroll = self.terminal.scrollbar()?;
        self.generation += 1;
        self.full = false;
        let frame = Arc::new(Frame {
            generation: self.generation,
            size,
            rows: rows.into(),
            cursor,
            selection: selection.into(),
            colors: frame_colors,
            scroll: ScrollState {
                total: u32::try_from(scroll.total).unwrap_or(u32::MAX),
                offset: u32::try_from(scroll.offset).unwrap_or(u32::MAX),
            },
            modes: Modes {
                bracketed_paste: self.mode(Mode::BRACKETED_PASTE),
                mouse_tracking: self.terminal.is_mouse_tracking().unwrap_or(false),
                alt_screen: self.terminal.active_screen().ok()
                    == Some(libghostty_vt::screen::Screen::Alternate),
                focus_events: self.mode(Mode::FOCUS_EVENT),
            },
            state: self.state,
        });
        self.previous = Some(frame.clone());
        Ok(frame)
    }

    fn mode(&self, mode: Mode) -> bool {
        self.terminal.mode(mode).unwrap_or(false)
    }

    fn write_pty(&mut self, bytes: &[u8]) {
        self.effects.borrow_mut().pty.extend_from_slice(bytes);
    }

    fn apply_theme(&mut self, theme: Theme) -> libghostty_vt::error::Result<()> {
        self.terminal
            .set_default_fg_color(Some(rgb(theme.foreground)))?
            .set_default_bg_color(Some(rgb(theme.background)))?
            .set_default_cursor_color(Some(rgb(theme.cursor)))?
            .set_default_color_palette(Some(palette(&theme)))?;
        self.theme = theme;
        self.scheme.set(theme.scheme);
        self.full = true;
        Ok(())
    }

    fn key(&mut self, input: &KeyInput) -> libghostty_vt::error::Result<()> {
        let mut event = key::Event::new()?;
        event
            .set_action(keys::action(input.action))
            .set_key(keys::physical(input.key))
            .set_mods(keys::mods(input.mods))
            .set_utf8(input.text.as_deref());
        if let Some(c) = keys::unshifted(input.key) {
            event.set_unshifted_codepoint(c);
        }
        let mut out = Vec::new();
        self.key_encoder
            .set_options_from_terminal(&self.terminal)
            .encode_to_vec(&event, &mut out)?;
        self.write_pty(&out);
        Ok(())
    }

    /// Strips C0 controls except tab and newline (ESC among them) and C1
    /// controls, then lets libghostty-vt bracket it or turn newlines into
    /// carriage returns. No carriage return is added (`Input::Paste`).
    fn paste(&mut self, text: &str) -> libghostty_vt::error::Result<()> {
        let mut data: Vec<u8> = text
            .chars()
            .filter(|&c| !is_stripped_control(c))
            .collect::<String>()
            .into_bytes();
        let bracketed = self.mode(Mode::BRACKETED_PASTE);
        let mut buf = vec![0u8; data.len() + 16];
        let n = match paste::encode(&mut data, bracketed, &mut buf) {
            Err(libghostty_vt::Error::OutOfSpace { required }) => {
                buf.resize(required, 0);
                paste::encode(&mut data, bracketed, &mut buf)?
            }
            other => other?,
        };
        self.write_pty(&buf[..n]);
        Ok(())
    }

    fn tracks_mouse(&self, shift: bool) -> bool {
        !shift && self.terminal.is_mouse_tracking().unwrap_or(false)
    }

    fn mouse(&mut self, input: &MouseInput) -> libghostty_vt::error::Result<()> {
        let button = input.button.map(|b| match b {
            MouseButton::Left => mouse::Button::Left,
            MouseButton::Middle => mouse::Button::Middle,
            MouseButton::Right => mouse::Button::Right,
        });
        let action = match input.kind {
            MouseKind::Down => mouse::Action::Press,
            MouseKind::Up => mouse::Action::Release,
            MouseKind::Drag | MouseKind::Move => mouse::Action::Motion,
        };
        let mut event = mouse::Event::new()?;
        event
            .set_action(action)
            .set_button(button)
            .set_mods(keys::mods(input.mods))
            .set_position(mouse::Position {
                x: input.x,
                y: input.y,
            });
        self.encode_mouse(&event)
    }

    fn encode_mouse(&mut self, event: &mouse::Event<'_>) -> libghostty_vt::error::Result<()> {
        let size = self.size.get();
        let mut out = Vec::new();
        self.mouse_encoder
            .set_options_from_terminal(&self.terminal)
            .set_size(mouse::EncoderSize {
                screen_width: u32::from(size.cols) * u32::from(size.cell_width),
                screen_height: u32::from(size.rows) * u32::from(size.cell_height),
                cell_width: u32::from(size.cell_width),
                cell_height: u32::from(size.cell_height),
                padding_top: 0,
                padding_bottom: 0,
                padding_right: 0,
                padding_left: 0,
            })
            .set_any_button_pressed(self.buttons_down != 0)
            .encode_to_vec(event, &mut out)?;
        self.write_pty(&out);
        Ok(())
    }

    /// Selection with the mouse: a click starts it (two clicks select the
    /// word, three the line), a drag extends it. Returns whether it changed.
    fn select(&mut self, input: &MouseInput) -> libghostty_vt::error::Result<bool> {
        let size = self.size.get();
        let at = Point::Viewport(PointCoordinate {
            x: input.col.min(size.cols - 1),
            y: u32::from(input.row.min(size.rows - 1)),
        });
        match input.kind {
            MouseKind::Down if input.button == Some(MouseButton::Left) => {
                let here = self.terminal.grid_ref(at)?;
                match input.clicks {
                    2 => {
                        let word = self.terminal.select_word(SelectWordOptions::new(here))?;
                        self.terminal.set_selection(word.as_ref())?;
                        self.anchor = None;
                    }
                    3 => {
                        let line = self.terminal.select_line(SelectLineOptions::new(here))?;
                        self.terminal.set_selection(line.as_ref())?;
                        self.anchor = None;
                    }
                    _ => {
                        self.anchor = self
                            .terminal
                            .point_from_grid_ref(&here, PointSpace::Screen)?;
                        self.terminal.set_selection(None)?;
                    }
                }
                Ok(true)
            }
            MouseKind::Drag => {
                let Some(anchor) = self.anchor else {
                    return Ok(false);
                };
                // The anchor's row can leave the scrollback (the oldest rows
                // are dropped): the drag then does nothing.
                let (Ok(start), Ok(end)) = (
                    self.terminal.grid_ref(Point::Screen(anchor)),
                    self.terminal.grid_ref(at),
                ) else {
                    self.anchor = None;
                    return Ok(false);
                };
                let selection = Selection::new(start, end, false);
                self.terminal.set_selection(Some(&selection))?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Returns whether the viewport changed.
    fn scroll(&mut self, input: &ScrollInput, running: bool) -> libghostty_vt::error::Result<bool> {
        if input.lines == 0 {
            return Ok(false);
        }
        let count = input.lines.unsigned_abs();
        if self.tracks_mouse(input.mods.shift) {
            if running {
                let size = self.size.get();
                let mut event = mouse::Event::new()?;
                event
                    .set_action(mouse::Action::Press)
                    .set_button(Some(if input.lines < 0 {
                        mouse::Button::Four
                    } else {
                        mouse::Button::Five
                    }))
                    .set_mods(keys::mods(input.mods))
                    .set_position(mouse::Position {
                        x: (f32::from(input.col) + 0.5) * f32::from(size.cell_width),
                        y: (f32::from(input.row) + 0.5) * f32::from(size.cell_height),
                    });
                for _ in 0..count {
                    self.encode_mouse(&event)?;
                }
            }
            return Ok(false);
        }
        let alt_screen =
            self.terminal.active_screen().ok() == Some(libghostty_vt::screen::Screen::Alternate);
        if alt_screen {
            // Alternate scroll: the wheel becomes arrow keys.
            if running {
                let key = KeyInput {
                    key: if input.lines < 0 {
                        lectern_services::terminal::Key::Up
                    } else {
                        lectern_services::terminal::Key::Down
                    },
                    mods: Default::default(),
                    action: lectern_services::terminal::KeyAction::Press,
                    text: None,
                };
                for _ in 0..count {
                    self.key(&key)?;
                }
            }
            return Ok(false);
        }
        self.terminal
            .scroll_viewport(ScrollViewport::Delta(input.lines as isize));
        Ok(true)
    }
}

/// The bit of `button` in `Engine::buttons_down`.
fn button_bit(button: Option<MouseButton>) -> u8 {
    match button {
        Some(MouseButton::Left) => 1,
        Some(MouseButton::Middle) => 2,
        Some(MouseButton::Right) => 4,
        None => 0,
    }
}

/// libghostty-vt 0.2.2 answers the title and icon reports (CSI 21 t /
/// CSI 20 t) with `OSC l` / `OSC L` and the program's own title, and has no
/// switch for it. The answer would type program-controlled text into the
/// program's input (design 「安全边界」), so it is dropped. Each reply is one
/// write.
fn is_title_report(data: &[u8]) -> bool {
    data.starts_with(b"\x1b]l") || data.starts_with(b"\x1b]L")
}

/// C0 controls other than tab and newline, DEL, and C1 controls.
fn is_stripped_control(c: char) -> bool {
    matches!(c, '\0'..='\x08' | '\x0b'..='\x1f' | '\x7f' | '\u{80}'..='\u{9f}')
}

/// libghostty-vt needs at least one cell and a non-zero cell size.
fn sanitize(size: GridSize) -> GridSize {
    GridSize {
        cols: size.cols.max(1),
        rows: size.rows.max(1),
        cell_width: size.cell_width.max(1),
        cell_height: size.cell_height.max(1),
    }
}

fn vt_scheme(scheme: ColorScheme) -> libghostty_vt::terminal::ColorScheme {
    match scheme {
        ColorScheme::Light => libghostty_vt::terminal::ColorScheme::Light,
        ColorScheme::Dark => libghostty_vt::terminal::ColorScheme::Dark,
    }
}

/// Fixed answers: a VT220 with ANSI colour.
fn device_attributes() -> DeviceAttributes {
    DeviceAttributes {
        primary: PrimaryDeviceAttributes::new(
            ConformanceLevel::VT220,
            &[DeviceAttributeFeature::ANSI_COLOR],
        ),
        secondary: SecondaryDeviceAttributes {
            device_type: DeviceType::VT220,
            firmware_version: 1,
            rom_cartridge: 0,
        },
        tertiary: TertiaryDeviceAttributes { unit_id: 0 },
    }
}
