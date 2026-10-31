//! `TerminalPane`: the view of one terminal (docs/design/terminal_view.md
//! 「接口」「数据结构」). It reads the latest `Arc<Frame>`, draws it through
//! `GridElement`, and turns keys, text, the mouse, the wheel, focus and the
//! size of its area into `Input`s for `dyn Terminals`. Everything runs on the
//! main thread; `Terminals::input` only queues, so nothing here blocks.

use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::channel::oneshot;
use gpui::{
    App, Bounds, ClipboardItem, Context, ElementId, EntityInputHandler, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, KeyUpEvent, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Point, Render, ScrollWheelEvent, Size, Styled,
    Subscription, Task, UTF16Selection, Window, div, point, px, size,
};
use lectern_i18n::keys;
use lectern_kernel::{ActivationToken, ServiceRef};
use lectern_services::terminal::{
    Frame, GridSize, Input, Key, KeyAction, MouseInput, MouseKind, RunState, ScrollInput,
    ScrollTarget, TerminalId, Terminals, Theme,
};
use lectern_ui::semantic::{NodeId, Role, Semantic, State, semantic_interactive};
use lectern_ui::{Text, colors, mono_metrics, strings};

use crate::actions::{self, KEY_CONTEXT};
use crate::colors::{color_of, theme, theme_of};
use crate::diag::{self, FrameStats};
use crate::element::GridElement;
use crate::geometry::{INSET, cells, extent};
use crate::input::{self, Mapped, button_of, cell_at, drag_direction, mods_of, wheel_lines};
use crate::layout::{RowCache, layout_row};
use crate::paint::{self, CellMetrics, MarkedPaint, Op, Scene};

/// How often a drag that left the grid scrolls it.
const AUTO_SCROLL: Duration = Duration::from_millis(50);

/// An input method's composition: the text, and the selected range in it
/// in UTF-16 units.
pub(crate) struct Marked {
    pub text: String,
    pub selected: Range<usize>,
}

/// A mouse button held down in the grid.
struct Drag {
    button: lectern_services::terminal::MouseButton,
    /// The last cell a `Drag` was sent for.
    last: (u16, u16),
    /// The direction the grid scrolls in and the task that scrolls it.
    auto: Option<(i32, Task<()>)>,
}

pub struct TerminalPane {
    terminals: ServiceRef<dyn Terminals>,
    token: ActivationToken,
    id: TerminalId,
    /// None: there is no frame yet, or the terminal is closed.
    frame: Option<Arc<Frame>>,
    /// From `info(id)`, for the semantic node.
    state: Option<RunState>,
    focus: FocusHandle,
    /// The last `Resize` sent.
    sent_size: Option<GridSize>,
    /// The last `SetTheme` sent (or the theme the host opened the terminal with).
    sent_theme: Option<Theme>,
    pub(crate) rows: RowCache,
    pub(crate) marked: Option<Marked>,
    /// The part of a line a wheel has scrolled and not yet sent.
    scroll_rest: f32,
    drag: Option<Drag>,
    /// The cell the pointer was last reported at without a button.
    last_move: Option<(u16, u16)>,
    /// The grid, in window coordinates, as the last prepaint left it.
    pub(crate) grid: Bounds<Pixels>,
    pub(crate) cell: Size<Pixels>,
    /// The keys whose press was handled and whose release is still to come.
    held: Vec<Key>,
    /// Focus as the terminal was last told.
    reported_focus: bool,
    /// A `ScrollTo(Bottom)` was sent and no newer frame has come.
    bottom_requested: bool,
    pub(crate) stats: FrameStats,
    /// The operations of the last paint, for the tests.
    #[cfg(test)]
    pub(crate) painted: Vec<Op>,
    _subscriptions: [Subscription; 3],
}

impl TerminalPane {
    /// `terminals` is the service reference of the host plugin; `token` is
    /// the host plugin's token for this activation (`PluginHandle::token`),
    /// which `copy_selection` needs. The pane does not call `watch`: the host
    /// holds the `FrameInterest` while the page is shown.
    pub fn new(
        terminals: ServiceRef<dyn Terminals>,
        token: ActivationToken,
        id: TerminalId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        let subscriptions = [
            cx.on_focus(&focus, window, |this, window, cx| {
                this.report_focus(window, cx);
            }),
            cx.on_blur(&focus, window, |this, window, cx| {
                this.report_focus(window, cx);
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                this.report_focus(window, cx);
            }),
        ];
        let (frame, state) = match terminals.get() {
            Some(service) => (service.frame(id), service.info(id).map(|info| info.state)),
            None => (None, None),
        };
        TerminalPane {
            terminals,
            token,
            id,
            frame,
            state,
            focus,
            sent_size: None,
            sent_theme: Some(theme(window, cx)),
            rows: RowCache::default(),
            marked: None,
            scroll_rest: 0.0,
            drag: None,
            last_move: None,
            grid: Bounds::default(),
            cell: Size::default(),
            held: Vec::new(),
            reported_focus: false,
            bottom_requested: false,
            stats: FrameStats::default(),
            #[cfg(test)]
            painted: Vec::new(),
            _subscriptions: subscriptions,
        }
    }

    /// Called when the host receives `TerminalEvent::Frame` for this
    /// terminal, or any `TerminalEvent::Listed`: takes the latest frame and
    /// state, and redraws if the generation or the state changed. It copies
    /// an `Arc` and compares; nothing is laid out here.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(service) = self.terminals.get() else {
            return;
        };
        let frame = service.frame(self.id);
        let state = service.info(self.id).map(|info| info.state);
        drop(service);
        let frame_changed = match (&self.frame, &frame) {
            (Some(old), Some(new)) => old.generation != new.generation,
            (None, None) => false,
            _ => true,
        };
        let state_changed = self.state != state;
        if frame_changed {
            self.bottom_requested = false;
        }
        if frame_changed || state_changed {
            self.frame = frame;
            self.state = state;
            cx.notify();
        }
    }

    /// The keyboard focus of the Terminal page.
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    pub fn id(&self) -> TerminalId {
        self.id
    }

    fn send(&self, input: Input) {
        if let Some(service) = self.terminals.get() {
            service.input(self.id, input);
        }
    }

    /// Whether the viewport shows the newest lines.
    fn at_bottom(&self) -> bool {
        self.frame.as_ref().is_none_or(|frame| {
            frame.scroll.offset + u32::from(frame.size.rows) >= frame.scroll.total
        })
    }

    /// Before keys, text and paste reach the program: the viewport goes back
    /// to the bottom.
    fn before_input(&mut self) {
        if !self.at_bottom() && !self.bottom_requested {
            self.bottom_requested = true;
            self.send(Input::ScrollTo(ScrollTarget::Bottom));
        }
    }

    /// Whether the terminal counts as focused: the pane has the keyboard
    /// focus and its window is the active one.
    pub(crate) fn has_focus(&self, window: &Window) -> bool {
        self.focus.is_focused(window) && window.is_window_active()
    }

    fn report_focus(&mut self, window: &Window, cx: &mut Context<Self>) {
        let focused = self.has_focus(window);
        if focused != self.reported_focus {
            self.reported_focus = focused;
            self.send(Input::Focus(focused));
            // The cursor and the selection change colour.
            cx.notify();
        }
    }

    /// Sends `SetTheme` when the appearance gave other colours.
    fn sync_theme(&mut self, window: &Window, cx: &App) {
        let current = theme(window, cx);
        if self.sent_theme != Some(current) {
            self.sent_theme = Some(current);
            self.send(Input::SetTheme(current));
        }
    }

    // ---- keyboard ----

    fn key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let action = if event.is_held {
            KeyAction::Repeat
        } else {
            KeyAction::Press
        };
        match input::map_key(&event.keystroke, action) {
            Some(Mapped::ScrollTo(target)) => {
                self.send(Input::ScrollTo(target));
                cx.stop_propagation();
            }
            Some(Mapped::Key(key)) => {
                self.before_input();
                if !self.held.contains(&key.key) {
                    self.held.push(key.key);
                }
                self.send(Input::Key(key));
                cx.stop_propagation();
            }
            None => {}
        }
    }

    fn key_up(&mut self, event: &KeyUpEvent, cx: &mut Context<Self>) {
        let Some(Mapped::Key(mut key)) = input::map_key(&event.keystroke, KeyAction::Release)
        else {
            return;
        };
        if let Some(at) = self.held.iter().position(|held| *held == key.key) {
            self.held.remove(at);
            key.action = KeyAction::Release;
            self.send(Input::Key(key));
            cx.stop_propagation();
        }
    }

    // ---- actions ----

    fn copy(&mut self, cx: &mut Context<Self>) {
        let has_selection = self
            .frame
            .as_ref()
            .is_some_and(|frame| !frame.selection.is_empty());
        if !has_selection {
            return;
        }
        let Some(service) = self.terminals.get() else {
            return;
        };
        let (reply, text) = oneshot::channel();
        service.copy_selection(
            self.id,
            self.token.clone(),
            Box::new(move |selected| {
                // The receiver is gone when the pane was dropped.
                let _ = reply.send(selected);
            }),
        );
        drop(service);
        cx.spawn(async move |_, cx| {
            // The sender is dropped without a reply when the host plugin was
            // deactivated: then nothing is written.
            if let Ok(Some(text)) = text.await {
                cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string(text)));
            }
        })
        .detach();
    }

    fn paste_clipboard(&mut self, cx: &mut Context<Self>) {
        let text = cx.read_from_clipboard().and_then(|item| item.text());
        if let Some(text) = text {
            self.paste_text(&text);
        }
    }

    fn paste_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.before_input();
        self.send(Input::Paste(Arc::from(text)));
    }

    // ---- mouse and wheel ----

    fn cell_at(&self, position: Point<Pixels>) -> (u16, u16) {
        let (cols, rows) = self.grid_cells();
        cell_at(position, &self.grid, self.cell, cols, rows)
    }

    fn grid_cells(&self) -> (u16, u16) {
        self.sent_size.map_or((1, 1), |sent| (sent.cols, sent.rows))
    }

    fn mouse_input(
        &self,
        kind: MouseKind,
        button: Option<lectern_services::terminal::MouseButton>,
        position: Point<Pixels>,
        modifiers: &gpui::Modifiers,
        clicks: usize,
    ) -> MouseInput {
        let (col, row) = self.cell_at(position);
        let inside = position - self.grid.origin;
        MouseInput {
            kind,
            button,
            mods: mods_of(modifiers),
            col,
            row,
            x: inside.x.as_f32(),
            y: inside.y.as_f32(),
            clicks: u8::try_from(clicks).unwrap_or(u8::MAX),
        }
    }

    pub(crate) fn mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus.focus(window, cx);
        let Some(button) = button_of(event.button) else {
            return;
        };
        let mouse = self.mouse_input(
            MouseKind::Down,
            Some(button),
            event.position,
            &event.modifiers,
            event.click_count,
        );
        self.drag = Some(Drag {
            button,
            last: (mouse.col, mouse.row),
            auto: None,
        });
        self.send(Input::Mouse(mouse));
        cx.stop_propagation();
    }

    pub(crate) fn mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(drag) = &self.drag {
            let button = drag.button;
            let last = drag.last;
            let cell = self.cell_at(event.position);
            self.set_auto_scroll(drag_direction(event.position, &self.grid), cx);
            if cell != last {
                if let Some(drag) = &mut self.drag {
                    drag.last = cell;
                }
                let mouse = self.mouse_input(
                    MouseKind::Drag,
                    Some(button),
                    event.position,
                    &event.modifiers,
                    0,
                );
                self.send(Input::Mouse(mouse));
            }
            return;
        }
        let tracking = self
            .frame
            .as_ref()
            .is_some_and(|frame| frame.modes.mouse_tracking);
        if !tracking || !hovered {
            return;
        }
        let cell = self.cell_at(event.position);
        if self.last_move != Some(cell) {
            self.last_move = Some(cell);
            let mouse =
                self.mouse_input(MouseKind::Move, None, event.position, &event.modifiers, 0);
            self.send(Input::Mouse(mouse));
        }
    }

    pub(crate) fn mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        // Dropping the drag stops its auto-scroll task.
        let Some(drag) = self.drag.take() else {
            return;
        };
        let mouse = self.mouse_input(
            MouseKind::Up,
            Some(drag.button),
            event.position,
            &event.modifiers,
            event.click_count,
        );
        self.send(Input::Mouse(mouse));
        cx.stop_propagation();
    }

    /// Starts, changes or stops the scrolling of a drag that left the grid.
    fn set_auto_scroll(&mut self, direction: i32, cx: &mut Context<Self>) {
        let Some(drag) = &mut self.drag else {
            return;
        };
        let current = drag.auto.as_ref().map(|(direction, _)| *direction);
        if current == Some(direction) || (current.is_none() && direction == 0) {
            return;
        }
        drag.auto = None;
        if direction == 0 {
            return;
        }
        let task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(AUTO_SCROLL).await;
                let alive = this
                    .update(cx, |pane, _| pane.auto_scroll(direction))
                    .unwrap_or(false);
                if !alive {
                    break;
                }
            }
        });
        if let Some(drag) = &mut self.drag {
            drag.auto = Some((direction, task));
        }
    }

    /// One tick of the scrolling of a drag: a line, and the drag at the edge
    /// of the grid. False when the button is no longer down.
    fn auto_scroll(&mut self, direction: i32) -> bool {
        let (_, rows) = self.grid_cells();
        let Some(drag) = &mut self.drag else {
            return false;
        };
        let (col, _) = drag.last;
        let row = if direction < 0 {
            0
        } else {
            rows.saturating_sub(1)
        };
        drag.last = (col, row);
        let button = drag.button;
        self.send(Input::Scroll(ScrollInput {
            lines: direction,
            col,
            row,
            mods: lectern_services::terminal::Mods::default(),
        }));
        let y = if direction < 0 {
            0.0
        } else {
            self.grid.size.height.as_f32()
        };
        self.send(Input::Mouse(MouseInput {
            kind: MouseKind::Drag,
            button: Some(button),
            mods: lectern_services::terminal::Mods::default(),
            col,
            row,
            x: self.cell.width.as_f32() * (f32::from(col) + 0.5),
            y,
            clicks: 0,
        }));
        true
    }

    pub(crate) fn wheel(&mut self, event: &ScrollWheelEvent) {
        let lines = wheel_lines(
            &mut self.scroll_rest,
            &event.delta,
            self.cell.height.max(px(1.0)),
        );
        if lines == 0 {
            return;
        }
        let (col, row) = self.cell_at(event.position);
        self.send(Input::Scroll(ScrollInput {
            lines,
            col,
            row,
            mods: mods_of(&event.modifiers),
        }));
    }

    // ---- painting ----

    /// The prepaint of the grid element: the size of the grid, the rows of
    /// the frame laid out (those the cache does not hold), and the list of
    /// drawing operations. It sends `Resize` when the size changed.
    pub(crate) fn prepare(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Op> {
        let started = Instant::now();
        let metrics = mono_metrics(window, cx);
        let scale = window.scale_factor();
        let area = cells(bounds.size, &metrics, scale);
        if self.sent_size != Some(area) {
            self.sent_size = Some(area);
            self.send(Input::Resize(area));
            diag::resized(self.id, area);
        }
        let cell_metrics = CellMetrics::new(&metrics, scale);
        self.cell = cell_metrics.cell;
        self.grid = Bounds {
            origin: bounds.origin + point(px(INSET), px(INSET)),
            size: extent(area, &metrics),
        };

        let palette = colors(window, cx);
        let focused = self.has_focus(window);
        let selection = if focused {
            palette.selected_content_background
        } else {
            palette.unemphasized_selected_content_background
        };
        let base = match &self.frame {
            Some(frame) => color_of(frame.colors.background),
            None => color_of(theme_of(palette).background),
        };
        let mut ops = vec![Op::Fill {
            bounds,
            color: base,
        }];
        let mut glyphs = 0;
        if let Some(frame) = self.frame.clone() {
            let text_system = window.text_system().clone();
            let layouts: Vec<_> = frame
                .rows
                .iter()
                .map(|row| {
                    self.rows.get(row, metrics.cell_width, || {
                        layout_row(row, &text_system, &metrics)
                    })
                })
                .collect();
            self.rows.end_frame();
            let marked = self
                .marked
                .as_ref()
                .map(|marked| marked_paint(&marked.text, &text_system, &metrics));
            let scene = Scene {
                frame: &frame,
                layouts: &layouts,
                origin: self.grid.origin,
                metrics: cell_metrics,
                focused,
                selection,
                marked: marked.as_ref(),
            };
            glyphs = paint::build(&scene, &mut ops).glyphs;
        }
        let laid_out = self.rows.take_laid_out();
        self.stats
            .record_prepaint(laid_out, glyphs, started.elapsed());
        ops
    }

    /// The semantic node: sizes and states, never the screen's text.
    fn node(&self, window: &Window, cx: &App) -> Semantic {
        let (cols, rows) = self
            .frame
            .as_ref()
            .map(|frame| (frame.size.cols, frame.size.rows))
            .or_else(|| self.sent_size.map(|sent| (sent.cols, sent.rows)))
            .unwrap_or((0, 0));
        let state = match self.state {
            None => "closed",
            Some(RunState::Starting) => "starting",
            Some(RunState::Running { .. }) => "running",
            Some(RunState::Exited { .. }) => "exited",
            Some(RunState::Failed(_)) => "failed",
        };
        let alt = self
            .frame
            .as_ref()
            .is_some_and(|frame| frame.modes.alt_screen);
        Semantic::new(NodeId::new("terminal", "grid", None), Role::Group)
            .label(&Text::key(&strings(cx), keys::terminal::GRID))
            .state(State {
                focused: self.focus.is_focused(window),
                ..State::default()
            })
            .attr("cols", cols.to_string())
            .attr("rows", rows.to_string())
            .attr("state", state)
            .attr("alt", alt.to_string())
            .attr("scrolled", (!self.at_bottom()).to_string())
    }
}

/// The composition shaped on its own line (it is not on the grid).
fn marked_paint(
    text: &str,
    text_system: &gpui::WindowTextSystem,
    metrics: &lectern_ui::MonoMetrics,
) -> MarkedPaint {
    let run = gpui::TextRun {
        len: text.len(),
        font: gpui::font(metrics.family),
        color: gpui::Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = text_system.layout_line(text, lectern_ui::TextRole::Code.size(), &[run], None);
    MarkedPaint {
        width: line.width,
        glyphs: line
            .runs
            .iter()
            .flat_map(|run| {
                run.glyphs
                    .iter()
                    .map(|glyph| (glyph.position.x, run.font_id, glyph.id, glyph.is_emoji))
            })
            .collect(),
    }
}

impl Focusable for TerminalPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_theme(window, cx);
        let node = self.node(window, cx);
        let root = div()
            .id(ElementId::from(&node.id))
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .on_action(cx.listener(|this, _: &actions::Copy, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &actions::Paste, _, cx| this.paste_clipboard(cx)))
            .on_action(cx.listener(|this, _: &actions::SelectAll, _, _| {
                this.send(Input::SelectAll);
            }))
            .on_action(cx.listener(|this, _: &actions::ClearScrollback, _, _| {
                this.send(Input::ClearScrollback);
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                this.key_down(event, cx);
            }))
            .on_key_up(cx.listener(|this, event: &KeyUpEvent, _, cx| {
                this.key_up(event, cx);
            }))
            .child(GridElement::new(cx.entity(), self.focus.clone()));
        semantic_interactive(node, root)
    }
}

/// The input method (docs/design/terminal_view.md「键盘」第 3 步). The terminal's
/// content is not given to the input method: it only learns where the
/// cursor is, so that it can place its candidate window.
impl EntityInputHandler for TerminalPane {
    fn text_for_range(
        &mut self,
        _range: Range<usize>,
        _adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self
                .marked
                .as_ref()
                .map_or(0..0, |marked| marked.selected.clone()),
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked
            .as_ref()
            .map(|marked| 0..marked.text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.marked.take().is_some() {
            cx.notify();
        }
    }

    fn paste(&mut self, item: ClipboardItem, _window: &mut Window, _cx: &mut Context<Self>) {
        // A paste the platform started goes the way ⌘V goes: bracketed paste
        // and the filtering of control characters are the terminal's.
        if let Some(text) = item.text() {
            self.paste_text(&text);
        }
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.marked.take().is_some() {
            cx.notify();
        }
        if !text.is_empty() {
            self.before_input();
            self.send(Input::Text(Arc::from(text)));
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = (!new_text.is_empty()).then(|| {
            let length = new_text.encode_utf16().count();
            Marked {
                text: new_text.to_owned(),
                selected: new_selected_range.unwrap_or(length..length),
            }
        });
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let (col, row) = self
            .frame
            .as_ref()
            .and_then(|frame| frame.cursor)
            .map_or((0, 0), |cursor| (cursor.col, cursor.row));
        Some(Bounds {
            origin: self.grid.origin
                + point(
                    self.cell.width * f32::from(col),
                    self.cell.height * f32::from(row),
                ),
            size: size(self.cell.width, self.cell.height),
        })
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}
